mod crypto;
mod evidence;
mod fs_security;
mod migrate;
mod read;
mod writer;

use std::{ffi::OsStr, fmt, fs::File, path::Path, sync::Arc};

use rusqlite::Connection;

#[cfg(feature = "test-support")]
pub use crypto::MemoryKeyProvider;
pub use crypto::{CryptoError, KeyProvider, KeyringKeyProvider, open, seal};
pub use evidence::{EvidenceContext, EvidenceError, EvidenceOwnerRef, EvidenceVault};
#[cfg(feature = "test-support")]
pub use read::GraphCounts;
pub use read::{
    MAX_EVENT_PAGE_BYTES, MAX_EVENT_PAGE_ROWS, READ_POOL_SIZE, ReadPool, ReadStore,
    ReducerWatermark, StoredEvent,
};
pub use writer::{
    CommitReceipt, CommitSubmission, ContentRefWrite, CursorState, EvidenceLink, EvidenceOwner,
    EvidenceWrite, ExtractorApplyReceipt, ExtractorWriteBatch, GraphActionRow, GraphApplyReceipt,
    GraphArtifactRow, GraphFinishRow, GraphObservedFinishRow, GraphRunRow, GraphSessionContextRow,
    GraphWriteBatch, IngestionChunk, IngestionFault, MAX_BATCH_BYTES, MAX_BATCH_RECORDS,
    MAX_GRAPH_FACTS, SchemaFingerprintUpdate, SemanticEvidenceRow, SourceRegistration,
    SourceRegistrationReceipt, StoredWorkCandidate, WRITER_QUEUE_CAPACITY, WorkApplyReceipt,
    WorkCandidatePage, WorkCandidateQuery, WorkContractRow, WorkEdgeRow, WorkWriteBatch,
    WriterHandle, stable_content_ref_id,
};

#[derive(thiserror::Error)]
pub enum StoreError {
    #[error("filesystem security check failed")]
    Io(#[from] std::io::Error),
    #[error("database operation failed")]
    Sqlite(#[from] rusqlite::Error),
    #[error("evidence persistence failed")]
    Evidence(#[from] EvidenceError),
    #[error("normalized value serialization failed")]
    Serialization(#[from] serde_json::Error),
    #[error("the legacy database name is reserved")]
    LegacyDatabaseReserved,
    #[error("unsupported database schema version {0}")]
    UnsupportedSchema(i64),
    #[error("database schema is incompatible with this runtime")]
    IncompatibleSchema,
    #[error("ingestion batch is invalid")]
    InvalidBatch,
    #[error("content reference ID is not the stable project-scoped ID")]
    InvalidContentRefId,
    #[error("registered source was not found")]
    SourceNotFound,
    #[error("ingestion project or provider does not match the registered source")]
    ProjectMismatch,
    #[error("evidence owner or link reference is invalid")]
    InvalidReference,
    #[error("cursor conflict")]
    CursorConflict,
    #[error("reducer watermark conflict")]
    ReducerWatermarkConflict,
    #[error("immutable row conflict")]
    ImmutableConflict,
    #[error("writer stopped")]
    WriterStopped,
    #[error("read pool stopped")]
    ReaderStopped,
    #[error("read pool size must be exactly four")]
    InvalidReadPoolSize,
}

impl fmt::Debug for StoreError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let label = match self {
            Self::Io(_) => "Io",
            Self::Sqlite(_) => "Sqlite",
            Self::Evidence(_) => "Evidence",
            Self::Serialization(_) => "Serialization",
            Self::LegacyDatabaseReserved => "LegacyDatabaseReserved",
            Self::UnsupportedSchema(_) => "UnsupportedSchema",
            Self::IncompatibleSchema => "IncompatibleSchema",
            Self::InvalidBatch => "InvalidBatch",
            Self::InvalidContentRefId => "InvalidContentRefId",
            Self::SourceNotFound => "SourceNotFound",
            Self::ProjectMismatch => "ProjectMismatch",
            Self::InvalidReference => "InvalidReference",
            Self::CursorConflict => "CursorConflict",
            Self::ReducerWatermarkConflict => "ReducerWatermarkConflict",
            Self::ImmutableConflict => "ImmutableConflict",
            Self::WriterStopped => "WriterStopped",
            Self::ReaderStopped => "ReaderStopped",
            Self::InvalidReadPoolSize => "InvalidReadPoolSize",
        };
        formatter
            .debug_struct("StoreError")
            .field("kind", &label)
            .finish()
    }
}

impl StoreError {
    #[must_use]
    pub fn is_busy_or_locked(&self) -> bool {
        matches!(
            self,
            Self::Sqlite(rusqlite::Error::SqliteFailure(code, _))
                if matches!(
                    code.code,
                    rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked
                )
        )
    }

    #[must_use]
    pub fn is_retryable_store_failure(&self) -> bool {
        matches!(self, Self::Io(_) | Self::Evidence(_) | Self::Sqlite(_))
    }
}

pub struct Store {
    pub(crate) writer: Connection,
    pub(crate) _directory: File,
}

impl std::fmt::Debug for Store {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.debug_struct("Store").finish_non_exhaustive()
    }
}

pub struct StoreRuntime {
    writer: WriterHandle,
    read: ReadStore,
    _directory: File,
    writer_thread: Option<std::thread::JoinHandle<()>>,
    shutdown_enqueued: bool,
}

impl fmt::Debug for StoreRuntime {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("StoreRuntime")
            .field("writer", &self.writer)
            .field("read", &self.read)
            .finish_non_exhaustive()
    }
}

impl StoreRuntime {
    /// Starts the production store runtime with the OS credential provider.
    ///
    /// # Errors
    ///
    /// Returns an error when the database, read pool, evidence vault, or
    /// dedicated writer thread cannot start.
    pub async fn start(path: impl AsRef<Path>) -> Result<Self, StoreError> {
        Self::start_with_key_provider(path, Arc::new(KeyringKeyProvider)).await
    }

    /// Starts the same production runtime with a dependency-injected key source.
    ///
    /// # Errors
    ///
    /// Returns an error when the database, read pool, evidence vault, or
    /// dedicated writer thread cannot start.
    pub async fn start_with_key_provider(
        path: impl AsRef<Path>,
        keys: Arc<dyn KeyProvider>,
    ) -> Result<Self, StoreError> {
        let path = path.as_ref().to_path_buf();
        let (connection, directory, read, vault) = tokio::task::spawn_blocking(move || {
            let store = Store::open_new(&path)?;
            let read = ReadStore::new(ReadPool::open(&path, READ_POOL_SIZE)?);
            let evidence_root = path
                .parent()
                .filter(|parent| !parent.as_os_str().is_empty())
                .ok_or_else(|| {
                    std::io::Error::new(
                        std::io::ErrorKind::InvalidInput,
                        "state database path has no parent directory",
                    )
                })?
                .join("evidence");
            let vault = Arc::new(EvidenceVault::open(evidence_root, keys)?);
            let Store {
                writer: connection,
                _directory: directory,
            } = store;
            Ok::<_, StoreError>((connection, directory, read, vault))
        })
        .await
        .map_err(|_| StoreError::WriterStopped)??;
        let (sender, receiver) = tokio::sync::mpsc::channel(WRITER_QUEUE_CAPACITY);
        let writer = WriterHandle { sender };
        let writer_thread = std::thread::Builder::new()
            .name("agbox-sqlite-writer".into())
            .spawn(move || writer::run_writer(connection, vault, receiver))?;
        Ok(Self {
            writer,
            read,
            _directory: directory,
            writer_thread: Some(writer_thread),
            shutdown_enqueued: false,
        })
    }

    #[must_use]
    pub fn writer(&self) -> &WriterHandle {
        &self.writer
    }

    #[must_use]
    pub fn read(&self) -> &ReadStore {
        &self.read
    }

    /// Drains queued writes and joins the dedicated writer without blocking an
    /// async executor thread.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::WriterStopped`] if the writer has already stopped
    /// or its thread panics during shutdown.
    pub async fn shutdown(mut self) -> Result<(), StoreError> {
        let (reply, receive) = tokio::sync::oneshot::channel();
        let send_result = self
            .writer
            .sender
            .send(writer::WriteCommand::Shutdown { reply })
            .await
            .map_err(|_| StoreError::WriterStopped);
        if send_result.is_ok() {
            self.shutdown_enqueued = true;
        }
        let receive_result = if send_result.is_ok() {
            receive.await.map_err(|_| StoreError::WriterStopped)
        } else {
            Err(StoreError::WriterStopped)
        };
        let thread = self.writer_thread.take();
        let join_result = tokio::task::spawn_blocking(move || {
            thread
                .map(std::thread::JoinHandle::join)
                .transpose()
                .map_err(|_| StoreError::WriterStopped)
        })
        .await
        .map_err(|_| StoreError::WriterStopped)?;
        send_result?;
        receive_result?;
        join_result.map(|_| ())
    }
}

impl Drop for StoreRuntime {
    fn drop(&mut self) {
        if !self.shutdown_enqueued {
            let (reply, _receive) = tokio::sync::oneshot::channel();
            let mut shutdown = writer::WriteCommand::Shutdown { reply };
            loop {
                match self.writer.sender.try_send(shutdown) {
                    Ok(()) | Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => break,
                    Err(tokio::sync::mpsc::error::TrySendError::Full(command)) => {
                        shutdown = command;
                        std::thread::yield_now();
                    }
                }
            }
        }
        if let Some(thread) = self.writer_thread.take() {
            let _ = thread.join();
        }
    }
}

impl Store {
    /// Opens the clean-slate v2 state database.
    ///
    /// # Errors
    ///
    /// Returns an error when the path is reserved, the database files are not
    /// owner-controlled, the schema version is unsupported, or `SQLite` cannot
    /// initialize the database.
    pub fn open_new(path: impl AsRef<Path>) -> Result<Self, StoreError> {
        let path = path.as_ref();
        if path.file_name() == Some(OsStr::new("agbox.db")) {
            return Err(StoreError::LegacyDatabaseReserved);
        }
        let opened = migrate::open_writer(path)?;
        Ok(Self {
            writer: opened.connection,
            _directory: opened.directory,
        })
    }

    /// Returns the `SQLite` user schema version.
    ///
    /// # Errors
    ///
    /// Returns an error when `SQLite` cannot read the schema version.
    pub fn schema_version(&self) -> Result<i64, StoreError> {
        Ok(self
            .writer
            .pragma_query_value(None, "user_version", |row| row.get(0))?)
    }

    /// Returns the active `SQLite` journal mode.
    ///
    /// # Errors
    ///
    /// Returns an error when `SQLite` cannot read the journal mode.
    pub fn journal_mode(&self) -> Result<String, StoreError> {
        Ok(self
            .writer
            .pragma_query_value(None, "journal_mode", |row| row.get(0))?)
    }

    #[cfg(feature = "test-support")]
    /// Reports whether a table or virtual table exists.
    ///
    /// # Errors
    ///
    /// Returns an error when `SQLite` cannot query its schema catalog.
    pub fn table_exists(&self, table: &str) -> Result<bool, StoreError> {
        Ok(self.writer.query_row(
            "SELECT EXISTS(
                SELECT 1 FROM sqlite_schema
                WHERE type IN ('table', 'view') AND name = ?1
            )",
            [table],
            |row| row.get(0),
        )?)
    }
}
