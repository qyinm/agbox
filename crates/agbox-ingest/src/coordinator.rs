use std::{
    collections::{HashMap, HashSet},
    fmt, io,
    path::PathBuf,
    sync::{Arc, Mutex, RwLock},
};

use agbox_adapters::{
    DecodeContext, DecodeDisposition, DecodeError, DecodedRecord, DecodedRecordParts, DecoderState,
    DiscoveredSource, SourceAdapter,
};
use agbox_core::{ContentRef, EventPayload, PrivacyLabel, ProjectId, Provider, SourceObservation};
use agbox_store::{
    CommitReceipt, ContentRefWrite, CursorState, EvidenceLink, EvidenceOwner, EvidenceWrite,
    IngestionChunk, IngestionFault, MAX_BATCH_BYTES, MAX_BATCH_RECORDS, ReadStore,
    SchemaFingerprintUpdate, StoreError, WriterHandle, stable_content_ref_id,
};
use time::{OffsetDateTime, format_description::well_known::Rfc3339};

use crate::{
    DECODER_WORKERS, KeyedQueue, QueueError, QueueItem, RecordScanner, ScanOutcome, SourceKey,
    VerifiedOpenError, VerifiedSourceOpener, WorkPriority,
};

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
        })
    }
}

impl From<VerifiedOpenError> for IngestError {
    fn from(_: VerifiedOpenError) -> Self {
        Self::IdentityChanged
    }
}

/// Coordinates verified record decoding and the store's sole writer.
pub struct IngestionCoordinator {
    read: ReadStore,
    writer: WriterHandle,
    queue: Mutex<KeyedQueue>,
    sources: RwLock<HashMap<SourceKey, CoordinatorSource>>,
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
        Self {
            read,
            writer,
            queue: Mutex::new(KeyedQueue::new(queue_capacity)),
            sources: RwLock::new(HashMap::new()),
        }
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
        Ok(())
    }

    /// Pops the highest-priority queued source.
    ///
    /// # Errors
    ///
    /// Returns an error if coordinator state is unavailable.
    pub fn pop(&self) -> Result<Option<QueueItem>, IngestError> {
        Ok(self
            .queue
            .lock()
            .map_err(|_| IngestError::StateUnavailable)?
            .pop())
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
    pub async fn process_one(&self, item: QueueItem) -> Result<ProcessReport, IngestError> {
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
        let requeue_target = decode_target;
        let (chunk, record_count, needs_continuation) = tokio::task::spawn_blocking(move || {
            build_chunk(source, expected_cursor, decode_target)
        })
        .await
        .map_err(|_| IngestError::WorkerStopped)??;

        if record_count == 0 {
            return Ok(ProcessReport {
                key: item.key,
                committed_records: 0,
                committed_events: 0,
                cursor_offset: chunk.expected_cursor.offset,
                requeued: false,
            });
        }
        let measured = chunk.measured_semantic_bytes()?;
        if measured > MAX_BATCH_BYTES {
            return Err(IngestError::SemanticMeasurementMismatch);
        }
        let receipt = self.writer.commit_ingestion(chunk).await?;
        let should_requeue = needs_continuation || receipt.cursor_offset < requeue_target;
        if should_requeue {
            self.try_enqueue(item.key.clone(), item.target_offset, item.priority)?;
        }
        Ok(report(item.key, record_count, &receipt, should_requeue))
    }
}

fn report(
    key: SourceKey,
    committed_records: usize,
    receipt: &CommitReceipt,
    requeued: bool,
) -> ProcessReport {
    ProcessReport {
        key,
        committed_records,
        committed_events: receipt.inserted_events,
        cursor_offset: receipt.cursor_offset,
        requeued,
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
                batch.try_push_fault(
                    window.start(),
                    window.content_end(),
                    next_offset,
                    decode_error_class(&error),
                    &context,
                    adapter.provider(),
                )?;
                continue;
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
        if candidate > MAX_BATCH_BYTES && self.record_count != 0 {
            return Ok(BatchPush::FullBeforeRecord);
        }
        if candidate > MAX_BATCH_BYTES {
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
        if candidate > MAX_BATCH_BYTES {
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
    ) -> Result<(), IngestError> {
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
            return Err(IngestError::NoProgress);
        }
        self.accept(contribution, candidate);
        Ok(())
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
        let DecodedRecordParts {
            observation,
            events,
            evidence,
            disposition,
            next_state,
            ..
        } = decoded.into_parts();
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
        mut input: tokio::sync::mpsc::Receiver<QueueItem>,
    ) -> Result<(), IngestError> {
        let mut senders = Vec::with_capacity(DECODER_WORKERS);
        let mut workers = tokio::task::JoinSet::new();
        for _ in 0..DECODER_WORKERS {
            let (sender, mut receiver) = tokio::sync::mpsc::channel::<QueueItem>(1);
            senders.push(sender);
            let coordinator = Arc::clone(&self.coordinator);
            workers.spawn(async move {
                while let Some(item) = receiver.recv().await {
                    coordinator.process_one(item).await?;
                    while let Some(requeued) = coordinator.pop()? {
                        coordinator.process_one(requeued).await?;
                    }
                }
                Ok::<(), IngestError>(())
            });
        }
        let mut next = 0;
        while let Some(item) = input.recv().await {
            senders[next]
                .send(item)
                .await
                .map_err(|_| IngestError::WorkerStopped)?;
            next = (next + 1) % DECODER_WORKERS;
        }
        drop(senders);
        while let Some(result) = workers.join_next().await {
            result.map_err(|_| IngestError::WorkerStopped)??;
        }
        Ok(())
    }
}
