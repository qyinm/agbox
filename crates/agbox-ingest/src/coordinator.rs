use std::{
    collections::{HashMap, HashSet},
    fmt,
    future::Future,
    io,
    path::PathBuf,
    pin::Pin,
    sync::{
        Arc, Mutex, RwLock,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use agbox_adapters::{
    DecodeContext, DecodeDisposition, DecodeError, DecodedRecord, DecoderState, DiscoveredSource,
    SourceAdapter,
};
use agbox_core::{ContentRef, EventPayload, PrivacyLabel, ProjectId, Provider, SourceObservation};
use agbox_store::{
    ContentRefWrite, CursorState, EvidenceLink, EvidenceOwner, EvidenceWrite, GraphActionRow,
    GraphArtifactRow, GraphFinishRow, GraphObservedFinishRow, GraphRunRow, GraphSessionContextRow,
    GraphWriteBatch, IngestionChunk, IngestionFault, MAX_BATCH_BYTES, MAX_BATCH_RECORDS, ReadStore,
    SchemaFingerprintUpdate, StoreError, WriterHandle, stable_content_ref_id,
};
use agbox_workgraph::{
    CommittedEvent, DeterministicReducer, GraphMutation, ReduceError, ReducedFact,
};
use time::{OffsetDateTime, format_description::well_known::Rfc3339};
use tokio::sync::Notify;

use crate::{
    DECODER_WORKERS, KeyedQueue, QueueError, QueueItem, RecordScanner, ScanOutcome, SourceKey,
    VerifiedOpenError, VerifiedSourceOpener, WatcherError, WatcherRuntime, WorkPriority,
};

pub const GRAPH_REDUCER_NAME: &str = "deterministic-facts-v1";
const GRAPH_REDUCER_CONFLICT_RETRIES: usize = 3;

/// Immutable source facts needed to decode one registered generation.
#[derive(Clone)]
pub struct CoordinatorSource {
    pub discovered: DiscoveredSource,
    pub project_id: ProjectId,
    pub project_root: Option<PathBuf>,
    pub format: String,
    pub observed_at: OffsetDateTime,
}

impl fmt::Debug for CoordinatorSource {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CoordinatorSource")
            .field("provider", &self.discovered.provider)
            .field("generation", &self.discovered.generation)
            .field("size", &self.discovered.size)
            .field("project_id", &self.project_id)
            .finish_non_exhaustive()
    }
}

/// Result of one bounded source slice.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProcessReport {
    pub key: SourceKey,
    pub committed_records: usize,
    pub committed_events: usize,
    pub cursor_offset: u64,
    pub requeued: bool,
}

/// Bounded orchestration failure with no source plaintext or local path.
#[derive(thiserror::Error)]
pub enum IngestError {
    #[error("source generation is not registered")]
    SourceNotRegistered,
    #[error("source identity changed")]
    IdentityChanged,
    #[error("record I/O failed")]
    Io(#[from] io::Error),
    #[error("record decode failed")]
    Decode(#[from] DecodeError),
    #[error("store operation failed")]
    Store(#[from] StoreError),
    #[error("source queue is full")]
    Queue(#[from] QueueError),
    #[error("coordinator state is unavailable")]
    StateUnavailable,
    #[error("semantic byte accounting mismatch")]
    SemanticMeasurementMismatch,
    #[error("bounded ingestion chunk cannot make progress")]
    NoProgress,
    #[error("decoder worker stopped")]
    WorkerStopped,
    #[error("source watcher stopped")]
    WatcherStopped,
    #[error("injected retryable ingestion failure")]
    InjectedRetry(RetryClass),
    #[error("graph mutation is invalid")]
    InvalidGraphMutation,
    #[error("graph reduction failed")]
    Reduce(#[from] ReduceError),
}

impl fmt::Debug for IngestError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::SourceNotRegistered => "SourceNotRegistered",
            Self::IdentityChanged => "IdentityChanged",
            Self::Io(_) => "Io",
            Self::Decode(_) => "Decode",
            Self::Store(_) => "Store",
            Self::Queue(_) => "Queue",
            Self::StateUnavailable => "StateUnavailable",
            Self::SemanticMeasurementMismatch => "SemanticMeasurementMismatch",
            Self::NoProgress => "NoProgress",
            Self::WorkerStopped => "WorkerStopped",
            Self::WatcherStopped => "WatcherStopped",
            Self::InjectedRetry(_) => "InjectedRetry",
            Self::InvalidGraphMutation => "InvalidGraphMutation",
            Self::Reduce(_) => "Reduce",
        })
    }
}

/// Translates a pure workgraph mutation into the store-owned persistence DTO.
///
/// The boundary is intentionally one-way: store signatures never name a
/// workgraph type.
///
/// # Errors
///
/// Returns [`IngestError::InvalidGraphMutation`] when the mutation has no
/// committed watermark or a deterministic store identifier cannot be built.
#[allow(clippy::too_many_lines)]
pub fn graph_write_batch(mutation: GraphMutation) -> Result<GraphWriteBatch, IngestError> {
    let next_event_seq = mutation
        .through_event_seq
        .ok_or(IngestError::InvalidGraphMutation)?;
    let next_event_id = mutation
        .through_event_id
        .ok_or(IngestError::InvalidGraphMutation)?;
    let mut batch = GraphWriteBatch {
        reducer_name: GRAPH_REDUCER_NAME.to_owned(),
        expected_event_seq: mutation.expected_event_seq,
        next_event_seq,
        next_event_id,
        runs: Vec::new(),
        contexts: Vec::new(),
        actions: Vec::new(),
        artifacts: Vec::new(),
        observed_finishes: Vec::new(),
        finishes: Vec::new(),
    };
    for fact in mutation.facts {
        match fact {
            ReducedFact::AgentRunStarted {
                project_id,
                session_id,
                provider,
                native_agent_id,
                observed_at,
                evidence,
            } => batch.runs.push(GraphRunRow {
                run_id: stable_graph_id(
                    "run",
                    &[
                        project_id.as_str(),
                        session_id.as_str(),
                        provider.as_str(),
                        &native_agent_id,
                    ],
                ),
                project_id,
                provider,
                session_id,
                observed_at,
                finished: false,
                succeeded: None,
                evidence_event_id: evidence,
            }),
            ReducedFact::AgentRunFinished {
                project_id,
                session_id,
                provider,
                native_agent_id,
                succeeded,
                observed_at,
                evidence,
            } => batch.runs.push(GraphRunRow {
                run_id: stable_graph_id(
                    "run",
                    &[
                        project_id.as_str(),
                        session_id.as_str(),
                        provider.as_str(),
                        &native_agent_id,
                    ],
                ),
                project_id,
                provider,
                session_id,
                observed_at,
                finished: true,
                succeeded: Some(succeeded),
                evidence_event_id: evidence,
            }),
            ReducedFact::SessionContext {
                project_id,
                session_id,
                provider,
                branch_hash,
                observed_at,
                evidence,
            } => batch.contexts.push(GraphSessionContextRow {
                context_run_id: stable_graph_id(
                    "context",
                    &[project_id.as_str(), session_id.as_str(), provider.as_str()],
                ),
                project_id,
                session_id,
                provider,
                branch_hash,
                observed_at,
                evidence_event_id: evidence,
            }),
            ReducedFact::Artifact {
                project_id,
                session_id,
                path_hash,
                project_relative_path,
                operation,
                content_hash,
                observed_at,
                evidence,
            } => {
                let work_id = agbox_core::WorkId::parse_wire(&stable_graph_id(
                    "work",
                    &[project_id.as_str(), session_id.as_str()],
                ))
                .ok_or(IngestError::InvalidGraphMutation)?;
                batch.artifacts.push(GraphArtifactRow {
                    artifact_id: stable_graph_id(
                        "artifact",
                        &[evidence.as_str(), path_hash.as_str()],
                    ),
                    work_id,
                    project_id,
                    path_hash,
                    project_relative_path,
                    content_hash,
                    operation,
                    observed_at,
                    evidence_event_id: evidence,
                });
            }
            ReducedFact::ActionRequested {
                project_id,
                session_id,
                native_action_id,
                tool_name,
                input_hash,
                redacted_input,
                evidence,
            } => batch.actions.push(GraphActionRow {
                project_id,
                session_id,
                native_action_id,
                request_event_id: evidence,
                tool_name,
                input_hash,
                redacted_command: redacted_input,
            }),
            ReducedFact::ActionFinishedObserved {
                project_id,
                session_id,
                native_action_id,
                succeeded,
                observed_at,
                evidence,
            } => batch.observed_finishes.push(GraphObservedFinishRow {
                project_id,
                session_id,
                native_action_id,
                succeeded,
                finish_event_id: evidence,
                observed_at,
            }),
            ReducedFact::EligibleVerificationObserved {
                project_id,
                session_id,
                native_action_id,
                succeeded,
                basis,
                observed_at,
                evidence,
                ..
            }
            | ReducedFact::Verification {
                project_id,
                session_id,
                native_action_id,
                succeeded,
                basis,
                observed_at,
                evidence,
                ..
            } => batch.finishes.push(GraphFinishRow {
                verification_id: stable_graph_id("verification", &[evidence.as_str()]),
                project_id,
                session_id,
                native_action_id,
                succeeded,
                basis: basis.to_owned(),
                finish_event_id: evidence,
                observed_at,
            }),
            ReducedFact::HumanObjective { .. }
            | ReducedFact::HumanConstraint { .. }
            | ReducedFact::AgentStatement { .. } => {}
        }
    }
    batch.validate()?;
    Ok(batch)
}

/// Loads one bounded store page and translates it into the workgraph's pure
/// committed-event input.
///
/// # Errors
///
/// Returns a store read or bounded decoding error.
pub async fn reducer_events_after(
    read: &ReadStore,
    through_event_seq: u64,
    max_events: usize,
    max_bytes: usize,
) -> Result<Vec<CommittedEvent>, IngestError> {
    Ok(read
        .events_after(through_event_seq, max_events, max_bytes)
        .await?
        .into_iter()
        .map(|stored| CommittedEvent {
            event_seq: stored.event_seq,
            event: stored.event,
        })
        .collect())
}

fn stable_graph_id(prefix: &str, parts: &[&str]) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"agbox.graph.id.v1");
    hasher.update(prefix.as_bytes());
    for part in parts {
        hasher.update(&(part.len() as u64).to_le_bytes());
        hasher.update(part.as_bytes());
    }
    format!("{prefix}_{}", &hasher.finalize().to_hex()[..24])
}

/// Outcome of one bounded production graph-reducer page.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct GraphPageReport {
    pub through_event_seq: u64,
    pub scanned_events: usize,
    pub reduced_facts: usize,
    pub applied: bool,
    pub replayed: bool,
}

impl From<VerifiedOpenError> for IngestError {
    fn from(_: VerifiedOpenError) -> Self {
        Self::IdentityChanged
    }
}

impl From<WatcherError> for IngestError {
    fn from(_: WatcherError) -> Self {
        Self::WatcherStopped
    }
}

/// Bounded retry category retained in per-source health.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RetryClass {
    IdentityChanged,
    CursorConflict,
    Busy,
    StoreFailure,
}

/// Bounded health state for one source generation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SourceHealth {
    Healthy,
    Retrying { attempt: u8, class: RetryClass },
    Exhausted { attempts: u8, class: RetryClass },
}

/// Injected bounded retry schedule. Durations include caller-selected jitter.
#[derive(Clone, Debug)]
pub struct RetryPolicy {
    delays: Arc<[Duration]>,
}

impl RetryPolicy {
    pub const MAX_RETRIES: usize = 8;

    /// Builds a bounded retry policy.
    ///
    /// # Errors
    ///
    /// Returns an error when more than eight retries are configured.
    pub fn new(delays: Vec<Duration>) -> Result<Self, IngestError> {
        if delays.len() > Self::MAX_RETRIES {
            return Err(IngestError::StateUnavailable);
        }
        Ok(Self {
            delays: delays.into(),
        })
    }

    fn delay(&self, retry: usize) -> Option<Duration> {
        self.delays.get(retry).copied()
    }
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            delays: Arc::from([
                Duration::from_millis(5),
                Duration::from_millis(17),
                Duration::from_millis(43),
            ]),
        }
    }
}

/// Async clock and writer-submission observation point used by retry tests.
pub trait RetryClock: Send + Sync {
    fn sleep(&self, duration: Duration) -> Pin<Box<dyn Future<Output = ()> + Send + '_>>;

    fn before_attempt(&self) -> Pin<Box<dyn Future<Output = Option<RetryClass>> + Send + '_>> {
        Box::pin(std::future::ready(None))
    }

    fn before_writer_submit(&self) -> Pin<Box<dyn Future<Output = ()> + Send + '_>> {
        Box::pin(std::future::ready(()))
    }

    fn writer_submitted(&self) -> Pin<Box<dyn Future<Output = ()> + Send + '_>> {
        Box::pin(std::future::ready(()))
    }
}

#[derive(Debug)]
pub struct TokioRetryClock;

impl RetryClock for TokioRetryClock {
    fn sleep(&self, duration: Duration) -> Pin<Box<dyn Future<Output = ()> + Send + '_>> {
        Box::pin(tokio::time::sleep(duration))
    }
}

/// Capacity-owning queue lease. Dropping it returns work to the shared queue.
pub struct WorkLease {
    queue: Arc<Mutex<KeyedQueue>>,
    notify: Arc<Notify>,
    item: QueueItem,
    active: bool,
}

impl fmt::Debug for WorkLease {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("WorkLease")
            .field("item", &self.item)
            .field("active", &self.active)
            .finish_non_exhaustive()
    }
}

impl WorkLease {
    #[must_use]
    pub fn item(&self) -> &QueueItem {
        &self.item
    }

    fn finish(
        mut self,
        processed_offset: u64,
        force_requeue: bool,
        allow_target_gap: bool,
    ) -> Result<bool, IngestError> {
        let requeued = self
            .queue
            .lock()
            .map_err(|_| IngestError::StateUnavailable)?
            .finish_lease_after_progress(
                &self.item.key,
                processed_offset,
                force_requeue,
                allow_target_gap,
            );
        self.active = false;
        self.notify.notify_waiters();
        Ok(requeued)
    }

    fn abandon(mut self) -> Result<(), IngestError> {
        self.queue
            .lock()
            .map_err(|_| IngestError::StateUnavailable)?
            .abandon_lease(&self.item.key);
        self.active = false;
        self.notify.notify_waiters();
        Ok(())
    }
}

impl Drop for WorkLease {
    fn drop(&mut self) {
        if self.active {
            if let Ok(mut queue) = self.queue.lock() {
                queue.cancel_lease(&self.item.key);
            }
            self.notify.notify_waiters();
        }
    }
}

/// Coordinates verified record decoding and the store's sole writer.
pub struct IngestionCoordinator {
    read: ReadStore,
    writer: WriterHandle,
    queue: Arc<Mutex<KeyedQueue>>,
    notify: Arc<Notify>,
    sources: RwLock<HashMap<SourceKey, CoordinatorSource>>,
    health: Mutex<HashMap<SourceKey, SourceHealth>>,
    retry_policy: RetryPolicy,
    clock: Arc<dyn RetryClock>,
}

impl fmt::Debug for IngestionCoordinator {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("IngestionCoordinator")
            .finish_non_exhaustive()
    }
}

impl IngestionCoordinator {
    #[must_use]
    pub fn new(read: ReadStore, writer: WriterHandle, queue_capacity: usize) -> Self {
        Self::with_retry(
            read,
            writer,
            queue_capacity,
            RetryPolicy::default(),
            Arc::new(TokioRetryClock),
        )
    }

    #[must_use]
    pub fn with_retry(
        read: ReadStore,
        writer: WriterHandle,
        queue_capacity: usize,
        retry_policy: RetryPolicy,
        clock: Arc<dyn RetryClock>,
    ) -> Self {
        Self {
            read,
            writer,
            queue: Arc::new(Mutex::new(KeyedQueue::new(queue_capacity))),
            notify: Arc::new(Notify::new()),
            sources: RwLock::new(HashMap::new()),
            health: Mutex::new(HashMap::with_capacity(queue_capacity)),
            retry_policy,
            clock,
        }
    }

    /// Reduces and atomically applies the next durable activity-event page.
    ///
    /// This is deliberately separate from source cursor commits: a graph
    /// failure cannot strand an ingestion cursor. The durable reducer
    /// watermark makes restart safe, and bounded conflict retries converge
    /// concurrent callers on the current local watermark.
    ///
    /// # Errors
    ///
    /// Returns a bounded read, reduction, translation, writer, or watermark
    /// error. No graph rows or watermark are partially committed.
    pub async fn reduce_next_graph_page(&self) -> Result<GraphPageReport, IngestError> {
        for attempt in 0..GRAPH_REDUCER_CONFLICT_RETRIES {
            let watermark = self.read.reducer_watermark(GRAPH_REDUCER_NAME).await?;
            let events = reducer_events_after(
                &self.read,
                watermark.through_event_seq,
                agbox_store::MAX_EVENT_PAGE_ROWS,
                agbox_store::MAX_EVENT_PAGE_BYTES,
            )
            .await?;
            if events.is_empty() {
                return Ok(GraphPageReport {
                    through_event_seq: watermark.through_event_seq,
                    scanned_events: 0,
                    reduced_facts: 0,
                    applied: false,
                    replayed: false,
                });
            }
            let scanned_events = events.len();
            let mutation = DeterministicReducer.reduce(&events)?;
            let reduced_facts = mutation.facts.len();
            let batch = graph_write_batch(mutation)?;
            match self.writer.apply_graph(batch).await {
                Ok(receipt) => {
                    return Ok(GraphPageReport {
                        through_event_seq: receipt.through_event_seq,
                        scanned_events,
                        reduced_facts,
                        applied: true,
                        replayed: receipt.replayed,
                    });
                }
                Err(StoreError::ReducerWatermarkConflict)
                    if attempt + 1 < GRAPH_REDUCER_CONFLICT_RETRIES => {}
                Err(error) => return Err(error.into()),
            }
        }
        Err(IngestError::Store(StoreError::ReducerWatermarkConflict))
    }

    /// Registers immutable decode facts for an already store-registered source.
    ///
    /// # Errors
    ///
    /// Returns an error if the source key and source facts disagree or a
    /// coordinator lock was poisoned.
    pub fn register_source(&self, source: CoordinatorSource) -> Result<SourceKey, IngestError> {
        let key = SourceKey::new(
            source.discovered.source_id.clone(),
            source.discovered.generation,
        )
        .map_err(|_| IngestError::SourceNotRegistered)?;
        self.sources
            .write()
            .map_err(|_| IngestError::StateUnavailable)?
            .insert(key.clone(), source);
        Ok(key)
    }

    /// Adds or coalesces one source signal.
    ///
    /// # Errors
    ///
    /// Returns a bounded queue or coordinator-state error.
    pub fn try_enqueue(
        &self,
        key: SourceKey,
        target_offset: u64,
        priority: WorkPriority,
    ) -> Result<(), IngestError> {
        self.queue
            .lock()
            .map_err(|_| IngestError::StateUnavailable)?
            .try_enqueue(key, target_offset, priority)?;
        self.notify.notify_one();
        Ok(())
    }

    /// Leases the highest-priority queued source while reserving its capacity.
    ///
    /// # Errors
    ///
    /// Returns an error if coordinator state is unavailable.
    pub fn lease_one(&self) -> Result<Option<WorkLease>, IngestError> {
        let item = self
            .queue
            .lock()
            .map_err(|_| IngestError::StateUnavailable)?
            .lease_next();
        Ok(item.map(|item| WorkLease {
            queue: Arc::clone(&self.queue),
            notify: Arc::clone(&self.notify),
            item,
            active: true,
        }))
    }

    /// Returns the bounded health state for one source generation.
    ///
    /// # Errors
    ///
    /// Returns an error when coordinator health state is unavailable.
    pub fn source_health(&self, key: &SourceKey) -> Result<SourceHealth, IngestError> {
        Ok(self
            .health
            .lock()
            .map_err(|_| IngestError::StateUnavailable)?
            .get(key)
            .copied()
            .unwrap_or(SourceHealth::Healthy))
    }

    /// Decodes and atomically commits one bounded source slice.
    ///
    /// Blocking source and JSON work runs on the blocking pool. No coordinator
    /// lock is held while awaiting the cursor read, decode task, writer receipt,
    /// or requeue.
    ///
    /// # Errors
    ///
    /// Returns a bounded identity, I/O, decode, store, queue, or accounting
    /// error. Failed commits never advance the durable cursor.
    pub async fn process_one(&self, lease: WorkLease) -> Result<ProcessReport, IngestError> {
        let item = lease.item().clone();
        let mut retry = 0_usize;
        loop {
            let attempt_result = match self.clock.before_attempt().await {
                Some(class) => Err(IngestError::InjectedRetry(class)),
                None => self.process_attempt(&item).await,
            };
            match attempt_result {
                Ok(attempt) => {
                    self.set_health(&item.key, SourceHealth::Healthy)?;
                    let requeued = lease.finish(
                        attempt.cursor_offset,
                        attempt.needs_continuation,
                        attempt.committed_records != 0,
                    )?;
                    return Ok(ProcessReport {
                        key: item.key,
                        committed_records: attempt.committed_records,
                        committed_events: attempt.committed_events,
                        cursor_offset: attempt.cursor_offset,
                        requeued,
                    });
                }
                Err(error) => {
                    let Some(class) = retry_class(&error) else {
                        self.set_health(
                            &item.key,
                            SourceHealth::Exhausted {
                                attempts: u8::try_from(retry).unwrap_or(u8::MAX),
                                class: RetryClass::StoreFailure,
                            },
                        )?;
                        lease.abandon()?;
                        return Err(error);
                    };
                    let Some(delay) = self.retry_policy.delay(retry) else {
                        self.set_health(
                            &item.key,
                            SourceHealth::Exhausted {
                                attempts: u8::try_from(retry).unwrap_or(u8::MAX),
                                class,
                            },
                        )?;
                        lease.abandon()?;
                        return Err(error);
                    };
                    retry += 1;
                    self.set_health(
                        &item.key,
                        SourceHealth::Retrying {
                            attempt: u8::try_from(retry).unwrap_or(u8::MAX),
                            class,
                        },
                    )?;
                    self.clock.sleep(delay).await;
                }
            }
        }
    }

    async fn process_attempt(&self, item: &QueueItem) -> Result<AttemptReport, IngestError> {
        // Reload both the registered source facts and durable cursor on every
        // retry. A caller may atomically re-register refreshed facts for this
        // key while the injected backoff is pending.
        let source = self
            .sources
            .read()
            .map_err(|_| IngestError::StateUnavailable)?
            .get(&item.key)
            .cloned()
            .ok_or(IngestError::SourceNotRegistered)?;
        let expected_cursor = self
            .read
            .cursor(item.key.source_id().to_owned(), item.key.generation())
            .await?
            .ok_or(StoreError::SourceNotFound)?;
        let decode_target = item.target_offset.min(source.discovered.size);
        let (chunk, record_count, needs_continuation) = tokio::task::spawn_blocking(move || {
            build_chunk(source, expected_cursor, decode_target)
        })
        .await
        .map_err(|_| IngestError::WorkerStopped)??;

        if record_count == 0 {
            return Ok(AttemptReport {
                committed_records: 0,
                committed_events: 0,
                cursor_offset: chunk.expected_cursor.offset,
                needs_continuation: false,
            });
        }
        let measured = chunk.measured_semantic_bytes()?;
        if measured > MAX_BATCH_BYTES {
            return Err(IngestError::SemanticMeasurementMismatch);
        }
        self.clock.before_writer_submit().await;
        let submission = self.writer.submit_ingestion(chunk).await?;
        self.clock.writer_submitted().await;
        let receipt = submission.receive().await?;
        Ok(AttemptReport {
            committed_records: record_count,
            committed_events: receipt.inserted_events,
            cursor_offset: receipt.cursor_offset,
            needs_continuation,
        })
    }

    fn set_health(&self, key: &SourceKey, value: SourceHealth) -> Result<(), IngestError> {
        let mut health = self
            .health
            .lock()
            .map_err(|_| IngestError::StateUnavailable)?;
        if value == SourceHealth::Healthy {
            health.remove(key);
            return Ok(());
        }
        let capacity = self
            .queue
            .lock()
            .map_err(|_| IngestError::StateUnavailable)?
            .capacity();
        if !health.contains_key(key)
            && health.len() == capacity
            && let Some(evicted) = health.keys().next().cloned()
        {
            health.remove(&evicted);
        }
        health.insert(key.clone(), value);
        Ok(())
    }
}

struct AttemptReport {
    committed_records: usize,
    committed_events: usize,
    cursor_offset: u64,
    needs_continuation: bool,
}

fn retry_class(error: &IngestError) -> Option<RetryClass> {
    match error {
        IngestError::IdentityChanged => Some(RetryClass::IdentityChanged),
        IngestError::InjectedRetry(class) => Some(*class),
        IngestError::Io(_)
        | IngestError::WorkerStopped
        | IngestError::Decode(DecodeError::Io(_)) => Some(RetryClass::StoreFailure),
        IngestError::Store(StoreError::CursorConflict) => Some(RetryClass::CursorConflict),
        IngestError::Store(error) if error.is_busy_or_locked() => Some(RetryClass::Busy),
        IngestError::Store(error) if error.is_retryable_store_failure() => {
            Some(RetryClass::StoreFailure)
        }
        _ => None,
    }
}

fn adapter_for(provider: Provider) -> Result<&'static dyn SourceAdapter, IngestError> {
    agbox_adapters::adapters()
        .iter()
        .copied()
        .find(|adapter| adapter.provider() == provider)
        .ok_or(IngestError::SourceNotRegistered)
}

fn build_chunk(
    source: CoordinatorSource,
    expected_cursor: CursorState,
    target_offset: u64,
) -> Result<(IngestionChunk, usize, bool), IngestError> {
    let adapter = adapter_for(source.discovered.provider)?;
    let opener = VerifiedSourceOpener::new(&source.discovered.root)?;
    let file = opener.open(&source.discovered)?;
    let mut scanner = RecordScanner::new(file, expected_cursor.offset, target_offset)?;
    let mut state = DecoderState::default();
    state.replace(expected_cursor.parser_state.clone())?;
    let context = DecodeContext {
        project_id: source.project_id,
        project_root: source.project_root,
        source_id: expected_cursor.source_id.clone(),
        observed_at: source.observed_at,
        source_generation: expected_cursor.generation,
        format: source.format,
    };
    let mut batch = BatchBuilder::new(expected_cursor, state)?;
    let mut needs_continuation = false;

    loop {
        if batch.record_count() == MAX_BATCH_RECORDS {
            needs_continuation = true;
            break;
        }
        if let Some(decoded) = adapter.decode_continuation(&context, batch.decoder_state())? {
            match batch.try_push(decoded, None, &context)? {
                BatchPush::Accepted => continue,
                BatchPush::FullBeforeRecord => {
                    needs_continuation = true;
                    break;
                }
            }
        }

        let window = match scanner.next()? {
            ScanOutcome::Complete(window) => window,
            ScanOutcome::Incomplete { .. } | ScanOutcome::Eof => break,
        };
        let next_offset = window.next_offset();
        let decoded = adapter.decode(&window, &context, batch.decoder_state());
        verify_terminal(&window)?;
        let decoded = match decoded {
            Ok(decoded) => decoded,
            Err(error) if recoverable_decode_error(&error) => {
                match batch.try_push_fault(
                    window.start(),
                    window.content_end(),
                    next_offset,
                    decode_error_class(&error),
                    &context,
                    adapter.provider(),
                )? {
                    BatchPush::Accepted => continue,
                    BatchPush::FullBeforeRecord => {
                        needs_continuation = true;
                        break;
                    }
                }
            }
            Err(error) => return Err(error.into()),
        };
        match batch.try_push(decoded, Some(next_offset), &context)? {
            BatchPush::Accepted => {}
            BatchPush::FullBeforeRecord => {
                needs_continuation = true;
                break;
            }
        }
    }
    let record_count = batch.record_count();
    let chunk = batch.finish()?;
    Ok((chunk, record_count, needs_continuation))
}

fn verify_terminal(window: &crate::RecordWindow) -> io::Result<()> {
    let mut reader = window.open()?;
    io::copy(&mut reader, &mut io::sink())?;
    Ok(())
}

fn recoverable_decode_error(error: &DecodeError) -> bool {
    matches!(
        error,
        DecodeError::Malformed(_)
            | DecodeError::MissingIdentity(_)
            | DecodeError::OutputTooLarge
            | DecodeError::StateTooLarge
    )
}

fn decode_error_class(error: &DecodeError) -> &'static str {
    match error {
        DecodeError::OutputTooLarge | DecodeError::StateTooLarge => "oversized",
        _ => "malformed",
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum BatchPush {
    Accepted,
    FullBeforeRecord,
}

struct BatchBuilder {
    chunk: IngestionChunk,
    state: DecoderState,
    measured: usize,
    record_count: usize,
    content_ids: HashSet<String>,
}

impl BatchBuilder {
    fn new(expected_cursor: CursorState, state: DecoderState) -> Result<Self, IngestError> {
        let chunk = IngestionChunk {
            expected_cursor: expected_cursor.clone(),
            next_cursor: expected_cursor,
            observations: Vec::new(),
            events: Vec::new(),
            evidence: Vec::new(),
            evidence_links: Vec::new(),
            content_refs: Vec::new(),
            fingerprints: Vec::new(),
            faults: Vec::new(),
        };
        let measured = chunk.measured_semantic_bytes()?;
        Ok(Self {
            chunk,
            state,
            measured,
            record_count: 0,
            content_ids: HashSet::new(),
        })
    }

    const fn record_count(&self) -> usize {
        self.record_count
    }

    const fn decoder_state(&self) -> &DecoderState {
        &self.state
    }

    fn try_push(
        &mut self,
        decoded: DecodedRecord,
        next_offset: Option<u64>,
        context: &DecodeContext,
    ) -> Result<BatchPush, IngestError> {
        let mut contribution = Contribution::from_decoded(
            decoded,
            next_offset.unwrap_or(self.chunk.next_cursor.offset),
            context,
        )?;
        self.retain_new_content_refs(&mut contribution);
        let candidate = self.candidate_bytes(&contribution)?;
        if candidate_over_limit(candidate) && self.record_count != 0 {
            return Ok(BatchPush::FullBeforeRecord);
        }
        if candidate_over_limit(candidate) {
            return self.try_push_oversized(contribution);
        }
        self.accept(contribution, candidate);
        Ok(BatchPush::Accepted)
    }

    fn try_push_oversized(&mut self, contribution: Contribution) -> Result<BatchPush, IngestError> {
        let start = contribution.observation.range().start();
        let end = contribution.observation.range().end();
        let next_offset = contribution.next_cursor.offset;
        let fault = IngestionFault {
            fault_id: stable_fault_id(
                &self.chunk.expected_cursor.source_id,
                self.chunk.expected_cursor.generation,
                start,
                end,
                "oversized",
            ),
            source_id: self.chunk.expected_cursor.source_id.clone(),
            generation: self.chunk.expected_cursor.generation,
            byte_start: start,
            byte_end: end,
            class: "oversized".to_owned(),
            bounded_detail: "record rejected by bounded decoder".to_owned(),
        };
        let next_cursor = CursorState {
            source_id: self.chunk.expected_cursor.source_id.clone(),
            generation: self.chunk.expected_cursor.generation,
            offset: next_offset,
            parser_state: self.state.as_bytes().to_vec(),
        };
        let oversized = Contribution {
            observation: contribution.observation,
            events: Vec::new(),
            evidence: Vec::new(),
            evidence_links: Vec::new(),
            content_refs: Vec::new(),
            fingerprint: contribution.fingerprint,
            fault: Some(fault),
            next_cursor,
            next_state: self.state.clone(),
        };
        let candidate = self.candidate_bytes(&oversized)?;
        if candidate_over_limit(candidate) {
            return Err(IngestError::NoProgress);
        }
        self.accept(oversized, candidate);
        Ok(BatchPush::Accepted)
    }

    fn try_push_fault(
        &mut self,
        start: u64,
        end: u64,
        next_offset: u64,
        class: &str,
        context: &DecodeContext,
        provider: Provider,
    ) -> Result<BatchPush, IngestError> {
        let fault = IngestionFault {
            fault_id: stable_fault_id(
                &self.chunk.expected_cursor.source_id,
                self.chunk.expected_cursor.generation,
                start,
                end,
                class,
            ),
            source_id: self.chunk.expected_cursor.source_id.clone(),
            generation: self.chunk.expected_cursor.generation,
            byte_start: start,
            byte_end: end,
            class: class.to_owned(),
            bounded_detail: "record rejected by bounded decoder".to_owned(),
        };
        let observation = synthetic_fault_observation(
            &self.chunk.expected_cursor,
            start,
            end,
            class,
            context,
            provider,
        )?;
        let mut next_cursor = self.chunk.next_cursor.clone();
        next_cursor.offset = next_offset;
        let contribution = Contribution {
            observation,
            events: Vec::new(),
            evidence: Vec::new(),
            evidence_links: Vec::new(),
            content_refs: Vec::new(),
            fingerprint: None,
            fault: Some(fault),
            next_cursor,
            next_state: self.state.clone(),
        };
        let candidate = self.candidate_bytes(&contribution)?;
        if candidate > MAX_BATCH_BYTES {
            if self.record_count != 0 {
                return Ok(BatchPush::FullBeforeRecord);
            }
            return Err(IngestError::NoProgress);
        }
        self.accept(contribution, candidate);
        Ok(BatchPush::Accepted)
    }

    fn candidate_bytes(&self, contribution: &Contribution) -> Result<usize, IngestError> {
        let old_next = cursor_semantic_bytes(&self.chunk.next_cursor)?;
        let new_next = cursor_semantic_bytes(&contribution.next_cursor)?;
        let added = contribution.semantic_bytes()?;
        self.measured
            .checked_sub(old_next)
            .and_then(|value| value.checked_add(new_next))
            .and_then(|value| value.checked_add(added))
            .ok_or(IngestError::SemanticMeasurementMismatch)
    }

    fn accept(&mut self, mut contribution: Contribution, candidate: usize) {
        self.chunk.observations.push(contribution.observation);
        self.chunk.events.append(&mut contribution.events);
        self.chunk.evidence.append(&mut contribution.evidence);
        self.chunk
            .evidence_links
            .append(&mut contribution.evidence_links);
        for content in contribution.content_refs {
            if self.content_ids.insert(content.content_ref_id.clone()) {
                self.chunk.content_refs.push(content);
            }
        }
        if let Some(fingerprint) = contribution.fingerprint {
            self.chunk.fingerprints.push(fingerprint);
        }
        if let Some(fault) = contribution.fault {
            self.chunk.faults.push(fault);
        }
        self.chunk.next_cursor = contribution.next_cursor;
        self.state = contribution.next_state;
        self.measured = candidate;
        self.record_count += 1;
    }

    fn retain_new_content_refs(&self, contribution: &mut Contribution) {
        let mut candidate_ids = HashSet::new();
        contribution.content_refs.retain(|content| {
            !self.content_ids.contains(&content.content_ref_id)
                && candidate_ids.insert(content.content_ref_id.clone())
        });
    }

    fn finish(mut self) -> Result<IngestionChunk, IngestError> {
        self.chunk.next_cursor.parser_state = self.state.as_bytes().to_vec();
        let authoritative = self.chunk.measured_semantic_bytes()?;
        if authoritative != self.measured {
            return Err(IngestError::SemanticMeasurementMismatch);
        }
        Ok(self.chunk)
    }
}

const fn candidate_over_limit(candidate: usize) -> bool {
    candidate > MAX_BATCH_BYTES
}

struct Contribution {
    observation: SourceObservation,
    events: Vec<agbox_core::ActivityEventV1>,
    evidence: Vec<EvidenceWrite>,
    evidence_links: Vec<EvidenceLink>,
    content_refs: Vec<ContentRefWrite>,
    fingerprint: Option<SchemaFingerprintUpdate>,
    fault: Option<IngestionFault>,
    next_cursor: CursorState,
    next_state: DecoderState,
}

impl Contribution {
    #[allow(clippy::too_many_lines)]
    fn from_decoded(
        decoded: DecodedRecord,
        next_offset: u64,
        context: &DecodeContext,
    ) -> Result<Self, IngestError> {
        let (observation, events, evidence, disposition, next_state, _) =
            decoded.into_parts().decompose();
        let mut content_refs = Vec::new();
        if let Some(content) = observation.bounded_record() {
            content_refs.push(content_write(
                &context.project_id,
                content,
                PrivacyLabel::RestrictedLocal,
            )?);
        }
        for event in &events {
            for content in event_content_refs(event.payload()) {
                content_refs.push(content_write(
                    &context.project_id,
                    content,
                    event.privacy(),
                )?);
            }
        }
        let mut writes = Vec::with_capacity(evidence.len());
        let mut links = Vec::with_capacity(evidence.len());
        for decoded_evidence in evidence {
            let owner = events
                .iter()
                .find(|event| event.event_id() == &decoded_evidence.owner_event_id)
                .ok_or(StoreError::InvalidReference)?;
            content_refs.push(content_write(
                &context.project_id,
                &decoded_evidence.content,
                owner.privacy(),
            )?);
            links.push(EvidenceLink {
                event_id: decoded_evidence.owner_event_id.as_str().to_owned(),
                observation_id: observation.observation_id().to_owned(),
                evidence_id: decoded_evidence.evidence_id.as_str().to_owned(),
            });
            writes.push(EvidenceWrite {
                evidence_id: decoded_evidence.evidence_id,
                project_id: context.project_id.clone(),
                owner: EvidenceOwner::Event(decoded_evidence.owner_event_id),
                content_hash: decoded_evidence.content.hash().to_owned(),
                media_type: decoded_evidence.content.media_type().to_owned(),
                privacy: owner.privacy(),
                disclosure_class: decoded_evidence.content.disclosure_class(),
                redacted_excerpt: decoded_evidence
                    .content
                    .redacted_excerpt()
                    .unwrap_or_default()
                    .to_owned(),
                expires_at: None,
                plaintext: decoded_evidence.plaintext,
            });
        }
        let fault = disposition.class().map(|class| IngestionFault {
            fault_id: stable_fault_id(
                &context.source_id,
                context.source_generation,
                observation.range().start(),
                observation.range().end(),
                class,
            ),
            source_id: context.source_id.clone(),
            generation: context.source_generation,
            byte_start: observation.range().start(),
            byte_end: observation.range().end(),
            class: match disposition {
                DecodeDisposition::Malformed { .. } => "malformed",
                DecodeDisposition::Oversized { .. } => "oversized",
                _ => class,
            }
            .to_owned(),
            bounded_detail: "record rejected by bounded decoder".to_owned(),
        });
        let fingerprint = Some(SchemaFingerprintUpdate {
            provider: observation.source().provider().as_str().to_owned(),
            format: observation.source().format().to_owned(),
            fingerprint: observation.schema_fingerprint().to_owned(),
            observed_at: observation.observed_at(),
        });
        let next_cursor = CursorState {
            source_id: context.source_id.clone(),
            generation: context.source_generation,
            offset: next_offset,
            parser_state: next_state.as_bytes().to_vec(),
        };
        Ok(Self {
            observation,
            events,
            evidence: writes,
            evidence_links: links,
            content_refs,
            fingerprint,
            fault,
            next_cursor,
            next_state,
        })
    }

    fn semantic_bytes(&self) -> Result<usize, IngestError> {
        let mut total = serde_json::to_vec(&self.observation)
            .map_err(StoreError::from)?
            .len();
        for event in &self.events {
            checked_add(
                &mut total,
                serde_json::to_vec(event).map_err(StoreError::from)?.len(),
            )?;
        }
        for evidence in &self.evidence {
            checked_add(&mut total, evidence.evidence_id.as_str().len())?;
            checked_add(&mut total, evidence.project_id.as_str().len())?;
            let (owner_kind, owner_len) = match &evidence.owner {
                EvidenceOwner::Event(owner) => ("event", owner.as_str().len()),
                EvidenceOwner::Work(owner) => ("work", owner.as_str().len()),
            };
            checked_add(&mut total, owner_kind.len())?;
            checked_add(&mut total, owner_len)?;
            checked_add(&mut total, evidence.content_hash.len())?;
            checked_add(&mut total, evidence.media_type.len())?;
            checked_add(&mut total, privacy_wire(evidence.privacy).len())?;
            checked_add(&mut total, disclosure_wire(evidence.disclosure_class).len())?;
            checked_add(&mut total, evidence.redacted_excerpt.len())?;
            checked_add(&mut total, 1)?;
            if let Some(expires_at) = evidence.expires_at {
                checked_add(
                    &mut total,
                    expires_at
                        .format(&Rfc3339)
                        .map_err(|_| StoreError::InvalidBatch)?
                        .len(),
                )?;
            }
            checked_add(&mut total, evidence.plaintext.len())?;
        }
        for link in &self.evidence_links {
            checked_add(&mut total, link.event_id.len())?;
            checked_add(&mut total, link.observation_id.len())?;
            checked_add(&mut total, link.evidence_id.len())?;
        }
        for content in &self.content_refs {
            checked_add(&mut total, content.content_ref_id.len())?;
            checked_add(&mut total, content.project_id.as_str().len())?;
            checked_add(
                &mut total,
                serde_json::to_vec(&content.content)
                    .map_err(StoreError::from)?
                    .len(),
            )?;
            checked_add(&mut total, privacy_wire(content.privacy).len())?;
        }
        if let Some(fingerprint) = &self.fingerprint {
            checked_add(&mut total, fingerprint.provider.len())?;
            checked_add(&mut total, fingerprint.format.len())?;
            checked_add(&mut total, fingerprint.fingerprint.len())?;
            checked_add(
                &mut total,
                fingerprint
                    .observed_at
                    .format(&Rfc3339)
                    .map_err(|_| StoreError::InvalidBatch)?
                    .len(),
            )?;
        }
        if let Some(fault) = &self.fault {
            checked_add(&mut total, fault.fault_id.len())?;
            checked_add(&mut total, fault.source_id.len())?;
            checked_add(&mut total, fault.class.len())?;
            checked_add(&mut total, fault.bounded_detail.len())?;
            checked_add(&mut total, std::mem::size_of::<u64>() * 3)?;
        }
        Ok(total)
    }
}

fn checked_add(total: &mut usize, value: usize) -> Result<(), IngestError> {
    *total = total
        .checked_add(value)
        .ok_or(IngestError::SemanticMeasurementMismatch)?;
    Ok(())
}

fn cursor_semantic_bytes(cursor: &CursorState) -> Result<usize, IngestError> {
    cursor
        .source_id
        .len()
        .checked_add(cursor.parser_state.len())
        .and_then(|value| value.checked_add(std::mem::size_of::<u64>() * 2))
        .ok_or(IngestError::SemanticMeasurementMismatch)
}

fn content_write(
    project_id: &ProjectId,
    content: &ContentRef,
    privacy: PrivacyLabel,
) -> Result<ContentRefWrite, IngestError> {
    Ok(ContentRefWrite {
        content_ref_id: stable_content_ref_id(project_id, content)?,
        project_id: project_id.clone(),
        content: content.clone(),
        privacy,
    })
}

fn event_content_refs(payload: &EventPayload) -> Vec<&ContentRef> {
    match payload {
        EventPayload::SessionStarted { context } => context.iter().collect(),
        EventPayload::SessionContextChanged { context, .. } => vec![context],
        EventPayload::MessageCreated { content } => vec![content],
        EventPayload::ActionRequested { input, .. } => vec![input],
        EventPayload::ActionFinished { output, .. } => output.iter().collect(),
        EventPayload::ArtifactChanged { path, .. } => vec![path],
        EventPayload::PlanObserved { plan } => vec![plan],
        EventPayload::DiagnosticObserved { message, .. } => vec![message],
        EventPayload::TurnStarted { .. }
        | EventPayload::TurnFinished { .. }
        | EventPayload::AgentStarted { .. }
        | EventPayload::AgentFinished { .. }
        | EventPayload::ContextCompacted { .. } => Vec::new(),
    }
}

fn stable_fault_id(source_id: &str, generation: u64, start: u64, end: u64, class: &str) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"agbox.ingestion.fault.v1");
    hasher.update(source_id.as_bytes());
    hasher.update(&generation.to_le_bytes());
    hasher.update(&start.to_le_bytes());
    hasher.update(&end.to_le_bytes());
    hasher.update(class.as_bytes());
    format!("fault_{}", &hasher.finalize().to_hex()[..32])
}

fn synthetic_fault_observation(
    cursor: &CursorState,
    start: u64,
    end: u64,
    class: &str,
    context: &DecodeContext,
    provider: Provider,
) -> Result<SourceObservation, IngestError> {
    use agbox_core::{ByteRange, DecodeStatus, SourceObservationDraft, SourceRef, SourceRefDraft};
    let source = SourceRef::new(SourceRefDraft {
        provider,
        format: context.format.clone(),
        native_session_id: "rejected-record".to_owned(),
        native_record_type: class.to_owned(),
        native_record_id: None,
        source_generation: cursor.generation,
        byte_offset: start,
        ordinal: None,
        record_hash: stable_fault_id(&cursor.source_id, cursor.generation, start, end, class),
        decoder_version: "coordinator-v1".to_owned(),
    })
    .map_err(|_| IngestError::NoProgress)?;
    SourceObservation::new(SourceObservationDraft {
        observation_id: stable_fault_id(&cursor.source_id, cursor.generation, start, end, "obs"),
        source,
        range: ByteRange::new(start, end).map_err(|_| IngestError::NoProgress)?,
        observed_at: context.observed_at,
        status: if class == "oversized" {
            DecodeStatus::Oversized
        } else {
            DecodeStatus::Malformed
        },
        bounded_record: None,
        schema_fingerprint: "rejected-record".to_owned(),
    })
    .map_err(|_| IngestError::NoProgress)
}

const fn privacy_wire(value: PrivacyLabel) -> &'static str {
    match value {
        PrivacyLabel::RestrictedLocal => "restricted_local",
        PrivacyLabel::PrivateLocal => "private_local",
        PrivacyLabel::DerivedLocal => "derived_local",
        PrivacyLabel::SyncEligible => "sync_eligible",
    }
}

const fn disclosure_wire(value: agbox_core::DisclosureClass) -> &'static str {
    use agbox_core::DisclosureClass;
    match value {
        DisclosureClass::HumanIntent => "human_intent",
        DisclosureClass::AgentStatement => "agent_statement",
        DisclosureClass::ObservedState => "observed_state",
        DisclosureClass::ToolResult => "tool_result",
        DisclosureClass::Reasoning => "reasoning",
        DisclosureClass::SystemInstruction => "system_instruction",
        DisclosureClass::DeveloperInstruction => "developer_instruction",
        DisclosureClass::DerivedText => "derived_text",
    }
}

/// Fixed-width worker owner for coordinator queue items.
pub struct IngestionRuntime {
    coordinator: Arc<IngestionCoordinator>,
}

impl fmt::Debug for IngestionRuntime {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("IngestionRuntime")
            .field("decoder_workers", &DECODER_WORKERS)
            .finish_non_exhaustive()
    }
}

impl IngestionRuntime {
    #[must_use]
    pub fn new(coordinator: Arc<IngestionCoordinator>) -> Self {
        Self { coordinator }
    }

    /// Runs exactly four long-lived decoder workers until input closes.
    ///
    /// # Errors
    ///
    /// Returns the first worker or coordinator failure after all worker tasks
    /// have been joined.
    pub async fn run(
        self,
        input: tokio::sync::mpsc::Receiver<QueueItem>,
    ) -> Result<(), IngestError> {
        let (shutdown, receive_shutdown) = tokio::sync::watch::channel(false);
        let result = self.run_until(input, receive_shutdown).await;
        drop(shutdown);
        result
    }

    /// Starts a live watcher through its readiness barrier, then feeds its
    /// bounded signals through the same four-worker keyed-queue runtime.
    ///
    /// # Errors
    ///
    /// Returns watcher startup/failure or the first coordinator worker failure.
    pub async fn run_watcher_until(
        self,
        watcher: WatcherRuntime,
        shutdown: tokio::sync::watch::Receiver<bool>,
    ) -> Result<(), IngestError> {
        let handle = watcher.start(shutdown).await?;
        let (input, watcher_task, mut readiness) = handle.into_runtime_parts()?;
        // Watcher cancellation closes admission only after its bounded backend
        // signals have been reconciled. The unchanged four-worker `run` path
        // then drains the input channel and keyed queue before joining.
        let runtime = self.run(input);
        tokio::pin!(runtime);
        let mut runtime_result = None;
        tokio::select! {
            ready = &mut readiness => {
                ready.map_err(|_| IngestError::WatcherStopped)?;
            }
            result = &mut runtime => {
                runtime_result = Some(result);
            }
        }
        let runtime_result = match runtime_result {
            Some(result) => result,
            None => runtime.await,
        };
        let watcher_result = watcher_task
            .await
            .map_err(|_| IngestError::WatcherStopped)?
            .map_err(IngestError::from);
        runtime_result.and(watcher_result)
    }

    /// Runs exactly four workers until input closes or explicit shutdown is signaled.
    ///
    /// A `true` shutdown value stops accepting new input. Work already in the
    /// shared keyed queue, including durable partial-commit continuations, is
    /// drained before all four worker tasks are joined and this method returns.
    /// Blocking decode jobs are awaited; shutdown never aborts a worker or
    /// detaches a `spawn_blocking` continuation.
    ///
    /// # Errors
    ///
    /// Returns the first worker or coordinator failure after all worker tasks
    /// have been joined.
    pub async fn run_until(
        self,
        mut input: tokio::sync::mpsc::Receiver<QueueItem>,
        mut shutdown: tokio::sync::watch::Receiver<bool>,
    ) -> Result<(), IngestError> {
        let closed = Arc::new(AtomicBool::new(false));
        let mut workers = tokio::task::JoinSet::new();
        for _ in 0..DECODER_WORKERS {
            let coordinator = Arc::clone(&self.coordinator);
            let closed = Arc::clone(&closed);
            workers.spawn(async move {
                loop {
                    let notified = coordinator.notify.notified();
                    tokio::pin!(notified);
                    notified.as_mut().enable();
                    if let Some(lease) = coordinator.lease_one()? {
                        let _ = coordinator.process_one(lease).await;
                        coordinator.notify.notify_waiters();
                        continue;
                    }
                    let idle = coordinator
                        .queue
                        .lock()
                        .map_err(|_| IngestError::StateUnavailable)?
                        .is_empty();
                    if closed.load(Ordering::Acquire) && idle {
                        break;
                    }
                    notified.await;
                }
                Ok::<(), IngestError>(())
            });
        }

        loop {
            if *shutdown.borrow() {
                break;
            }
            let item = tokio::select! {
                biased;
                changed = shutdown.changed() => {
                    match changed {
                        Ok(()) if *shutdown.borrow() => None,
                        Ok(()) => continue,
                        Err(_) => None,
                    }
                }
                item = input.recv() => item,
            };
            let Some(item) = item else {
                break;
            };
            loop {
                let notified = self.coordinator.notify.notified();
                tokio::pin!(notified);
                notified.as_mut().enable();
                match self.coordinator.try_enqueue(
                    item.key.clone(),
                    item.target_offset,
                    item.priority,
                ) {
                    Ok(()) => break,
                    Err(IngestError::Queue(QueueError::Full { .. })) => notified.await,
                    Err(error) => {
                        closed.store(true, Ordering::Release);
                        self.coordinator.notify.notify_waiters();
                        while workers.join_next().await.is_some() {}
                        return Err(error);
                    }
                }
            }
        }
        closed.store(true, Ordering::Release);
        self.coordinator.notify.notify_waiters();
        let mut worker_error = None;
        while let Some(result) = workers.join_next().await {
            let error = match result {
                Ok(Ok(())) => None,
                Ok(Err(error)) => Some(error),
                Err(_) => Some(IngestError::WorkerStopped),
            };
            if worker_error.is_none() {
                worker_error = error;
            }
        }
        worker_error.map_or(Ok(()), Err)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    fn cursor() -> CursorState {
        CursorState {
            source_id: "source_00000000000000000000000000000001".to_owned(),
            generation: 1,
            offset: 0,
            parser_state: Vec::new(),
        }
    }

    fn context() -> DecodeContext {
        DecodeContext {
            project_id: ProjectId::parse_wire("project_fixture").unwrap(),
            project_root: None,
            source_id: cursor().source_id,
            observed_at: OffsetDateTime::UNIX_EPOCH,
            source_generation: 1,
            format: "codex-rollout-1".to_owned(),
        }
    }

    #[test]
    fn malformed_fault_over_nonempty_cap_returns_full_without_mutation() {
        let mut builder = BatchBuilder::new(cursor(), DecoderState::default()).unwrap();
        builder.record_count = 1;
        builder.measured = MAX_BATCH_BYTES;
        let outcome = builder
            .try_push_fault(0, 10, 11, "malformed", &context(), Provider::Codex)
            .unwrap();
        assert_eq!(outcome, BatchPush::FullBeforeRecord);
        assert_eq!(builder.record_count(), 1);
        assert!(builder.chunk.faults.is_empty());
    }

    #[test]
    fn exact_batch_cap_is_inclusive_and_incremental_mismatch_is_rejected() {
        assert!(!candidate_over_limit(MAX_BATCH_BYTES));
        assert!(candidate_over_limit(MAX_BATCH_BYTES + 1));
        let mut builder = BatchBuilder::new(cursor(), DecoderState::default()).unwrap();
        let authoritative = builder.chunk.measured_semantic_bytes().unwrap();
        assert_eq!(builder.measured, authoritative);
        builder.measured += 1;
        assert!(matches!(
            builder.finish(),
            Err(IngestError::SemanticMeasurementMismatch)
        ));
    }
}
