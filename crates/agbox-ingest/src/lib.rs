mod attribution;
mod coordinator;
mod discovery;
mod history;
mod identity;
mod project;
mod queue;
mod record;
mod spool;
mod watcher;

pub use attribution::{SourceAttributionError, resolve_source_project};
pub use discovery::{
    DISCOVERY_ENTRIES_PER_YIELD, DiscoveryBatch, DiscoveryCursor, DiscoveryError, DiscoveryFault,
    DiscoveryFaultClass, DiscoveryWalker, MAX_DISCOVERY_CURSOR_BYTES,
    deduplicate_overlapping_sources,
};
pub use history::{HISTORY_DAYS, HistoryDecision, HistoryPolicy};
pub use identity::{
    GenerationError, SourceGeneration, SourceSnapshot, VerifiedOpenError, VerifiedSourceOpener,
    reconcile_generation,
};
pub use project::{ProjectError, ProjectResolver, ResolvedProject};
pub use queue::{
    DECODER_WORKERS, EnqueueOutcome, KeyedQueue, MAX_DECODER_WORKERS, MAX_SOURCE_QUEUE_CAPACITY,
    QueueConfigError, QueueError, QueueItem, SOURCE_QUEUE_CAPACITY, SourceKey, SourceKeyError,
    WorkPriority, validate_decoder_workers, validate_source_queue_capacity,
};
pub use record::{READ_BUFFER_BYTES, RecordScanner, RecordWindow, ScanOutcome, WindowReader};
pub use spool::{
    HookEventKind, HookSignal, HookSourceVerifier, HookSpool, HookSpoolLimits,
    MAX_HOOK_PAYLOAD_BYTES, MAX_SPOOL_BYTES, MAX_SPOOL_ENTRIES, MAX_SPOOL_ENTRY_BYTES, SpoolError,
};
pub use watcher::{
    MAX_BACKEND_EVENT_PATHS, POLL_INTERVAL, WATCH_SIGNAL_CAPACITY, WatchEventKind, WatchRoot,
    WatchSignal, WatchSignalBridge, WatchedSource, WatcherCatalog, WatcherError, WatcherHandle,
    WatcherRuntime,
};

#[cfg(feature = "test-support")]
pub mod test_support {
    #![allow(clippy::missing_errors_doc)]

    use std::{
        os::unix::ffi::OsStrExt,
        path::{Path, PathBuf},
        sync::Arc,
    };

    use agbox_adapters::{DiscoveredSource, RootClass};
    use agbox_core::{ProjectId, Provider};
    use agbox_store::{MemoryKeyProvider, ReadStore, SourceRegistration, StoreRuntime};
    use rustix::fs;
    use time::OffsetDateTime;
    use zeroize::Zeroizing;

    use crate::{
        CoordinatorSource, IngestError, IngestionCoordinator, IngestionRuntime, ProcessReport,
        RetryClock, RetryPolicy, SourceHealth, SourceKey, TokioRetryClock, WorkPriority,
    };

    pub struct FixtureRuntime {
        _directory: tempfile::TempDir,
        store: StoreRuntime,
        coordinator: Arc<IngestionCoordinator>,
        key: SourceKey,
        source_size: u64,
        source_path: PathBuf,
        database_path: PathBuf,
    }

    impl std::fmt::Debug for FixtureRuntime {
        fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            formatter
                .debug_struct("FixtureRuntime")
                .field("source_size", &self.source_size)
                .finish_non_exhaustive()
        }
    }

    impl FixtureRuntime {
        #[allow(clippy::expect_used, clippy::missing_panics_doc)]
        pub async fn codex_records(count: usize) -> Self {
            let records = (0..count).map(|index| {
                format!(
                    r#"{{"type":"event_msg","ordinal":{},"payload":{{"type":"user_message","message":"message-{}"}}}}"#,
                    index + 1,
                    index + 1
                )
            });
            Self::records(records).await
        }

        #[allow(clippy::expect_used, clippy::missing_panics_doc)]
        pub async fn records<I, S>(records: I) -> Self
        where
            I: IntoIterator<Item = S>,
            S: AsRef<str>,
        {
            Self::try_records(records)
                .await
                .expect("coordinator fixture must initialize")
        }

        pub async fn try_records<I, S>(records: I) -> Result<Self, IngestError>
        where
            I: IntoIterator<Item = S>,
            S: AsRef<str>,
        {
            Self::try_records_with_retry(
                records,
                crate::SOURCE_QUEUE_CAPACITY,
                RetryPolicy::default(),
                Arc::new(TokioRetryClock),
            )
            .await
        }

        pub async fn try_records_with_retry<I, S>(
            records: I,
            queue_capacity: usize,
            retry_policy: RetryPolicy,
            clock: Arc<dyn RetryClock>,
        ) -> Result<Self, IngestError>
        where
            I: IntoIterator<Item = S>,
            S: AsRef<str>,
        {
            let mut bytes = Vec::new();
            for record in records {
                bytes.extend_from_slice(record.as_ref().as_bytes());
                bytes.push(b'\n');
            }
            Self::try_source_bytes_with_retry(bytes, queue_capacity, retry_policy, clock).await
        }

        pub async fn try_source_bytes_with_retry(
            bytes: Vec<u8>,
            queue_capacity: usize,
            retry_policy: RetryPolicy,
            clock: Arc<dyn RetryClock>,
        ) -> Result<Self, IngestError> {
            let directory = tempfile::tempdir().map_err(IngestError::Io)?;
            #[cfg(unix)]
            std::fs::set_permissions(
                directory.path(),
                std::os::unix::fs::PermissionsExt::from_mode(0o700),
            )
            .map_err(IngestError::Io)?;
            let source_path = directory.path().join("source.jsonl");
            std::fs::write(&source_path, &bytes).map_err(IngestError::Io)?;
            let root = directory.path().canonicalize().map_err(IngestError::Io)?;
            let source_path = source_path.canonicalize().map_err(IngestError::Io)?;
            let stat = fs::stat(&source_path).map_err(|_| IngestError::IdentityChanged)?;
            let source_size =
                u64::try_from(stat.st_size).map_err(|_| IngestError::IdentityChanged)?;
            let device = u64::try_from(stat.st_dev).map_err(|_| IngestError::IdentityChanged)?;
            let file_identity = format!("unix:{device}:{}", stat.st_ino);
            let root_stat = fs::stat(&root).map_err(|_| IngestError::IdentityChanged)?;
            let root_device =
                u64::try_from(root_stat.st_dev).map_err(|_| IngestError::IdentityChanged)?;
            let repository_identity = format!("repo-fs-v1:{root_device}:{}", root_stat.st_ino);
            let mtime = stat_time(stat.st_mtime, stat.st_mtime_nsec)?;
            let ctime = stat_time(stat.st_ctime, stat.st_ctime_nsec)?;
            let source_id = stable_source_id(&file_identity);
            let project_id = ProjectId::for_test("project_fixture");
            let discovered = DiscoveredSource {
                source_id: source_id.clone(),
                provider: Provider::Codex,
                root: root.clone(),
                path: source_path.clone(),
                class: RootClass::Active,
                file_identity: file_identity.clone(),
                generation: 1,
                size: source_size,
                mtime,
                ctime,
                session_time: None,
            };
            let database = directory.path().join("state-v2.db");
            let store = StoreRuntime::start_with_key_provider(
                &database,
                Arc::new(MemoryKeyProvider::fixed([41; 32])),
            )
            .await?;
            store
                .writer()
                .register_source(SourceRegistration {
                    project_id: project_id.clone(),
                    repository_identity,
                    project_root: Zeroizing::new(path_bytes(&root)),
                    source_id: source_id.clone(),
                    provider: Provider::Codex,
                    root_class: "active".to_owned(),
                    source_path: Zeroizing::new(path_bytes(&source_path)),
                    file_identity,
                    generation: 1,
                    size_bytes: source_size,
                    mtime,
                    session_time: None,
                    initial_cursor: 0,
                })
                .await?;
            let coordinator = Arc::new(IngestionCoordinator::with_retry(
                store.read().clone(),
                store.writer().clone(),
                queue_capacity,
                retry_policy,
                clock,
            ));
            let key = coordinator.register_source(CoordinatorSource {
                discovered,
                project_id,
                project_root: Some(root),
                format: "codex-rollout-1".to_owned(),
                observed_at: OffsetDateTime::UNIX_EPOCH,
            })?;
            coordinator.try_enqueue(key.clone(), source_size, WorkPriority::Live)?;
            Ok(Self {
                _directory: directory,
                store,
                coordinator,
                key,
                source_size,
                source_path,
                database_path: database,
            })
        }

        pub async fn process_one(&self) -> Result<ProcessReport, IngestError> {
            let lease = self
                .coordinator
                .lease_one()?
                .ok_or(IngestError::NoProgress)?;
            self.coordinator.process_one(lease).await
        }

        pub async fn process_target(
            &self,
            target_offset: u64,
        ) -> Result<ProcessReport, IngestError> {
            self.coordinator
                .try_enqueue(self.key.clone(), target_offset, WorkPriority::Live)?;
            self.process_one().await
        }

        pub fn enqueue(&self) -> Result<(), IngestError> {
            self.coordinator
                .try_enqueue(self.key.clone(), self.source_size, WorkPriority::Live)
        }

        pub fn replace_source(&self, bytes: &[u8]) -> Result<(), IngestError> {
            let replacement = self.source_path.with_extension("replacement");
            std::fs::write(&replacement, bytes)?;
            std::fs::rename(replacement, &self.source_path)?;
            Ok(())
        }

        pub async fn drain(&self) -> Result<(), IngestError> {
            while let Some(lease) = self.coordinator.lease_one()? {
                let _ = self.coordinator.process_one(lease).await?;
            }
            Ok(())
        }

        pub async fn reduce_and_publish_next(
            &self,
        ) -> Result<Option<crate::WorkPublicationReport>, IngestError> {
            self.coordinator.reduce_and_publish_next().await
        }

        #[must_use]
        pub fn read(&self) -> FixtureRead {
            FixtureRead {
                read: self.store.read().clone(),
                key: self.key.clone(),
            }
        }

        #[must_use]
        pub const fn source_size(&self) -> u64 {
            self.source_size
        }

        pub fn first_record_end(&self) -> Result<u64, IngestError> {
            let bytes = std::fs::read(&self.source_path)?;
            bytes
                .iter()
                .position(|byte| *byte == b'\n')
                .and_then(|index| u64::try_from(index + 1).ok())
                .ok_or(IngestError::NoProgress)
        }

        #[must_use]
        pub fn database_path(&self) -> &Path {
            &self.database_path
        }

        #[must_use]
        pub fn read_store(&self) -> &ReadStore {
            self.store.read()
        }

        #[must_use]
        pub fn writer(&self) -> &agbox_store::WriterHandle {
            self.store.writer()
        }

        pub fn health(&self) -> Result<SourceHealth, IngestError> {
            self.coordinator.source_health(&self.key)
        }

        pub async fn run_signals(&self, signals: Vec<u64>) -> Result<(), IngestError> {
            let (sender, receiver) = tokio::sync::mpsc::channel(signals.len().max(1));
            for target_offset in signals {
                sender
                    .send(crate::QueueItem {
                        key: self.key.clone(),
                        target_offset,
                        priority: WorkPriority::Live,
                    })
                    .await
                    .map_err(|_| IngestError::WorkerStopped)?;
            }
            drop(sender);
            IngestionRuntime::new(Arc::clone(&self.coordinator))
                .run(receiver)
                .await
        }

        pub async fn run_with_unregistered_signal(&self) -> Result<(), IngestError> {
            let invalid = SourceKey::new("source_ffffffffffffffffffffffffffffffff".to_owned(), 1)
                .map_err(|_| IngestError::SourceNotRegistered)?;
            let (sender, receiver) = tokio::sync::mpsc::channel(1);
            sender
                .send(crate::QueueItem {
                    key: invalid,
                    target_offset: 1,
                    priority: WorkPriority::Live,
                })
                .await
                .map_err(|_| IngestError::WorkerStopped)?;
            drop(sender);
            IngestionRuntime::new(Arc::clone(&self.coordinator))
                .run(receiver)
                .await
        }

        #[must_use]
        pub fn production_runtime(&self) -> IngestionRuntime {
            IngestionRuntime::new(Arc::clone(&self.coordinator))
        }
    }

    #[derive(Clone, Debug)]
    pub struct FixtureRead {
        read: ReadStore,
        key: SourceKey,
    }

    impl FixtureRead {
        pub async fn event_count(&self) -> Result<u64, agbox_store::StoreError> {
            self.read.event_count().await
        }

        pub async fn fault_count(&self) -> Result<u64, agbox_store::StoreError> {
            self.read
                .fault_count(self.key.source_id().to_owned(), self.key.generation())
                .await
        }

        pub async fn cursor_offset(&self) -> Result<u64, agbox_store::StoreError> {
            self.read
                .cursor(self.key.source_id().to_owned(), self.key.generation())
                .await?
                .map(|cursor| cursor.offset)
                .ok_or(agbox_store::StoreError::SourceNotFound)
        }

        pub async fn parser_state(&self) -> Result<Vec<u8>, agbox_store::StoreError> {
            self.read
                .cursor(self.key.source_id().to_owned(), self.key.generation())
                .await?
                .map(|cursor| cursor.parser_state)
                .ok_or(agbox_store::StoreError::SourceNotFound)
        }

        pub async fn event_ids(&self) -> Result<Vec<String>, agbox_store::StoreError> {
            self.read.event_ids_for_test(4_096).await
        }
    }

    fn path_bytes(path: &Path) -> Vec<u8> {
        path.as_os_str().as_bytes().to_vec()
    }

    fn stat_time(seconds: i64, nanoseconds: i64) -> Result<OffsetDateTime, IngestError> {
        i128::from(seconds)
            .checked_mul(1_000_000_000)
            .and_then(|value| value.checked_add(i128::from(nanoseconds)))
            .and_then(|value| OffsetDateTime::from_unix_timestamp_nanos(value).ok())
            .ok_or(IngestError::IdentityChanged)
    }

    fn stable_source_id(identity: &str) -> String {
        format!(
            "source_{}",
            &blake3::hash(identity.as_bytes()).to_hex()[..32]
        )
    }
}
pub use coordinator::{
    CoordinatorSource, GRAPH_REDUCER_NAME, GraphPageReport, IngestError, IngestionCoordinator,
    IngestionRuntime, ProcessReport, RetryClass, RetryClock, RetryPolicy, SourceHealth,
    TokioRetryClock, WORK_VISIBILITY_REDUCER_NAME, WorkLease, WorkPublicationReport,
    WorkPublicationRequest, graph_write_batch, reducer_events_after, work_write_batch,
};
