use std::{
    fmt,
    fs::File,
    io,
    path::Path,
    sync::{Arc, Mutex},
};

use agbox_core::{
    ActivityEventDraft, ActivityEventV1, Actor, EventId, EventPayload, PrivacyLabel, ProjectId,
    SemanticKey, SessionId, SourceRef,
};
use rusqlite::{OpenFlags, OptionalExtension};
use time::{OffsetDateTime, format_description::well_known::Rfc3339};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

use crate::{CursorState, StoreError, fs_security};

pub const READ_POOL_SIZE: usize = 4;
pub const MAX_EVENT_PAGE_ROWS: usize = agbox_core::limits::MAX_BATCH_RECORDS;
pub const MAX_EVENT_PAGE_BYTES: usize = agbox_core::limits::MAX_BATCH_SEMANTIC_BYTES;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StoredEvent {
    pub event_seq: u64,
    pub event: ActivityEventV1,
}

#[cfg(feature = "test-support")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct GraphCounts {
    pub projects: u64,
    pub runs: u64,
    pub actions: u64,
    pub artifacts: u64,
    pub verifications: u64,
    pub evidence: u64,
    pub evidence_joins: u64,
}

struct ReadPoolInner {
    connections: Mutex<Vec<rusqlite::Connection>>,
    available: Arc<Semaphore>,
    _directory: File,
}

impl fmt::Debug for ReadPoolInner {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ReadPoolInner")
            .field("size", &READ_POOL_SIZE)
            .finish_non_exhaustive()
    }
}

#[derive(Clone)]
pub struct ReadPool {
    inner: Arc<ReadPoolInner>,
}

impl fmt::Debug for ReadPool {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ReadPool")
            .field("size", &READ_POOL_SIZE)
            .finish_non_exhaustive()
    }
}

impl ReadPool {
    pub(crate) fn open(path: &Path, size: usize) -> Result<Self, StoreError> {
        if size != READ_POOL_SIZE {
            return Err(StoreError::InvalidReadPoolSize);
        }
        let name = path.file_name().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "state database path has no file name",
            )
        })?;
        let parent = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "state database path has no parent directory",
                )
            })?;
        let (canonical_parent, directory) = fs_security::open_bound_owner_directory(parent)?;
        fs_security::validate_owner_file(&directory, name)?;
        let database = canonical_parent.join(name);
        let mut connections = Vec::with_capacity(READ_POOL_SIZE);
        for _ in 0..READ_POOL_SIZE {
            fs_security::validate_owner_file(&directory, name)?;
            let connection = rusqlite::Connection::open_with_flags(
                &database,
                OpenFlags::SQLITE_OPEN_READ_ONLY
                    | OpenFlags::SQLITE_OPEN_NOFOLLOW
                    | OpenFlags::SQLITE_OPEN_NO_MUTEX,
            )?;
            connection.pragma_update(None, "query_only", true)?;
            connections.push(connection);
        }
        Ok(Self {
            inner: Arc::new(ReadPoolInner {
                connections: Mutex::new(connections),
                available: Arc::new(Semaphore::new(READ_POOL_SIZE)),
                _directory: directory,
            }),
        })
    }

    async fn execute<R, F>(&self, query: F) -> Result<R, StoreError>
    where
        R: Send + 'static,
        F: FnOnce(&rusqlite::Connection) -> Result<R, StoreError> + Send + 'static,
    {
        let permit = Arc::clone(&self.inner.available)
            .acquire_owned()
            .await
            .map_err(|_| StoreError::ReaderStopped)?;
        let inner = Arc::clone(&self.inner);
        tokio::task::spawn_blocking(move || {
            let checkout = Checkout::new(inner, permit)?;
            query(checkout.connection())
        })
        .await
        .map_err(|_| StoreError::ReaderStopped)?
    }
}

struct Checkout {
    pool: Arc<ReadPoolInner>,
    connection: Option<rusqlite::Connection>,
    _permit: OwnedSemaphorePermit,
}

impl Checkout {
    fn new(pool: Arc<ReadPoolInner>, permit: OwnedSemaphorePermit) -> Result<Self, StoreError> {
        let connection = pool
            .connections
            .lock()
            .map_err(|_| StoreError::ReaderStopped)?
            .pop()
            .ok_or(StoreError::ReaderStopped)?;
        Ok(Self {
            pool,
            connection: Some(connection),
            _permit: permit,
        })
    }

    fn connection(&self) -> &rusqlite::Connection {
        self.connection
            .as_ref()
            .unwrap_or_else(|| unreachable!("checkout always owns a connection"))
    }
}

impl Drop for Checkout {
    fn drop(&mut self) {
        if let Some(connection) = self.connection.take()
            && let Ok(mut connections) = self.pool.connections.lock()
        {
            connections.push(connection);
        }
    }
}

#[derive(Clone, Debug)]
pub struct ReadStore {
    pool: ReadPool,
}

impl ReadStore {
    pub(crate) fn new(pool: ReadPool) -> Self {
        Self { pool }
    }

    /// Returns the number of retained activity events.
    ///
    /// # Errors
    ///
    /// Returns a read-pool or database error.
    pub async fn event_count(&self) -> Result<u64, StoreError> {
        self.pool
            .execute(|connection| {
                let value: i64 =
                    connection
                        .query_row("SELECT count(*) FROM activity_events", [], |row| row.get(0))?;
                u64::try_from(value).map_err(|_| StoreError::InvalidBatch)
            })
            .await
    }

    /// Returns a bounded page of committed events ordered by the local event
    /// sequence.
    ///
    /// The sequence is only a local reducer watermark. The returned page is
    /// clamped to 1,000 rows and four MiB of serialized semantic event data.
    ///
    /// # Errors
    ///
    /// Returns a validation, read-pool, database, timestamp, or event decoding
    /// error.
    pub async fn events_after(
        &self,
        through_event_seq: u64,
        max_events: usize,
        max_bytes: usize,
    ) -> Result<Vec<StoredEvent>, StoreError> {
        if through_event_seq > i64::MAX as u64 || max_events == 0 || max_bytes == 0 {
            return Err(StoreError::InvalidBatch);
        }
        let row_limit = max_events.min(MAX_EVENT_PAGE_ROWS);
        let byte_limit = max_bytes.min(MAX_EVENT_PAGE_BYTES);
        self.pool
            .execute(move |connection| {
                let mut statement = connection.prepare(
                    "SELECT event_seq, event_id, semantic_key, schema_version,
                            occurred_at, observed_at, project_id, session_id,
                            turn_id, actor, correlation_id, causation_id,
                            source_json, payload_json, privacy
                     FROM activity_events
                     WHERE event_seq > ?1
                     ORDER BY event_seq
                     LIMIT ?2",
                )?;
                let mut rows = statement.query(rusqlite::params![
                    i64::try_from(through_event_seq).map_err(|_| StoreError::InvalidBatch)?,
                    i64::try_from(row_limit).map_err(|_| StoreError::InvalidBatch)?,
                ])?;
                let mut events = Vec::with_capacity(row_limit);
                let mut semantic_bytes = 0_usize;
                while let Some(row) = rows.next()? {
                    let event_seq: i64 = row.get(0)?;
                    let event = decode_event_row(row)?;
                    let event_bytes = serde_json::to_vec(&event)?
                        .len()
                        .checked_add(size_of::<u64>())
                        .ok_or(StoreError::InvalidBatch)?;
                    let next_bytes = semantic_bytes
                        .checked_add(event_bytes)
                        .ok_or(StoreError::InvalidBatch)?;
                    if next_bytes > byte_limit {
                        break;
                    }
                    semantic_bytes = next_bytes;
                    events.push(StoredEvent {
                        event_seq: u64::try_from(event_seq)
                            .map_err(|_| StoreError::InvalidBatch)?,
                        event,
                    });
                }
                Ok(events)
            })
            .await
    }

    /// Returns the number of quarantined records for one source generation.
    ///
    /// # Errors
    ///
    /// Returns a validation, read-pool, database, or numeric error.
    pub async fn fault_count(
        &self,
        source_id: impl Into<String>,
        generation: u64,
    ) -> Result<u64, StoreError> {
        let source_id = source_id.into();
        validate_source_generation(&source_id, generation)?;
        self.pool
            .execute(move |connection| {
                let value: i64 = connection.query_row(
                    "SELECT count(*) FROM ingestion_faults
                     WHERE source_id = ?1 AND generation = ?2",
                    rusqlite::params![
                        source_id,
                        i64::try_from(generation).map_err(|_| StoreError::InvalidBatch)?
                    ],
                    |row| row.get(0),
                )?;
                u64::try_from(value).map_err(|_| StoreError::InvalidBatch)
            })
            .await
    }

    #[cfg(feature = "test-support")]
    #[doc(hidden)]
    pub async fn event_ids_for_test(&self, limit: usize) -> Result<Vec<String>, StoreError> {
        const MAX_TEST_EVENT_IDS: usize = 4_096;
        if limit > MAX_TEST_EVENT_IDS {
            return Err(StoreError::InvalidBatch);
        }
        self.pool
            .execute(move |connection| {
                let mut statement = connection
                    .prepare("SELECT event_id FROM activity_events ORDER BY event_id LIMIT ?1")?;
                let rows = statement.query_map(
                    [i64::try_from(limit).map_err(|_| StoreError::InvalidBatch)?],
                    |row| row.get::<_, String>(0),
                )?;
                rows.collect::<Result<Vec<_>, _>>()
                    .map_err(StoreError::from)
            })
            .await
    }

    /// Returns one bounded source cursor without exposing a `SQLite` connection.
    ///
    /// # Errors
    ///
    /// Returns a read-pool, database, numeric, or parser-state bound error.
    pub async fn cursor(
        &self,
        source_id: impl Into<String>,
        generation: u64,
    ) -> Result<Option<CursorState>, StoreError> {
        let source_id = source_id.into();
        validate_source_generation(&source_id, generation)?;
        self.pool
            .execute(move |connection| {
                let row: Option<(i64, Vec<u8>)> = connection
                    .query_row(
                        "SELECT cursor_offset, parser_state
                         FROM source_cursors
                         WHERE source_id = ?1 AND generation = ?2",
                        rusqlite::params![
                            source_id,
                            i64::try_from(generation).map_err(|_| StoreError::InvalidBatch)?
                        ],
                        |row| Ok((row.get(0)?, row.get(1)?)),
                    )
                    .optional()?;
                row.map(|(offset, parser_state)| {
                    if parser_state.len() > agbox_core::limits::MAX_DECODER_STATE_BYTES {
                        return Err(StoreError::InvalidBatch);
                    }
                    Ok(CursorState {
                        source_id,
                        generation,
                        offset: u64::try_from(offset).map_err(|_| StoreError::InvalidBatch)?,
                        parser_state,
                    })
                })
                .transpose()
            })
            .await
    }

    #[cfg(feature = "test-support")]
    #[doc(hidden)]
    pub async fn graph_counts_for_test(&self) -> Result<GraphCounts, StoreError> {
        self.pool
            .execute(|connection| {
                Ok(GraphCounts {
                    projects: count_table(connection, "projects")?,
                    runs: count_table(connection, "agent_runs")?,
                    actions: count_table(connection, "action_facts")?,
                    artifacts: count_table(connection, "artifacts")?,
                    verifications: count_table(connection, "verification_facts")?,
                    evidence: count_table(connection, "evidence_objects")?,
                    evidence_joins: count_table(connection, "work_evidence")?,
                })
            })
            .await
    }
}

fn decode_event_row(row: &rusqlite::Row<'_>) -> Result<ActivityEventV1, StoreError> {
    let event_id: String = row.get(1)?;
    let semantic_key: String = row.get(2)?;
    let schema_version: i64 = row.get(3)?;
    let occurred_at: String = row.get(4)?;
    let observed_at: String = row.get(5)?;
    let project_id: String = row.get(6)?;
    let session_id: String = row.get(7)?;
    let actor: String = row.get(9)?;
    let source_json: String = row.get(12)?;
    let payload_json: String = row.get(13)?;
    let privacy: String = row.get(14)?;
    ActivityEventV1::new(ActivityEventDraft {
        event_id: EventId::parse_wire(&event_id).ok_or(StoreError::InvalidBatch)?,
        semantic_key: SemanticKey::parse_wire(&semantic_key).ok_or(StoreError::InvalidBatch)?,
        schema_version: u16::try_from(schema_version).map_err(|_| StoreError::InvalidBatch)?,
        occurred_at: OffsetDateTime::parse(&occurred_at, &Rfc3339)
            .map_err(|_| StoreError::InvalidBatch)?,
        observed_at: OffsetDateTime::parse(&observed_at, &Rfc3339)
            .map_err(|_| StoreError::InvalidBatch)?,
        project_id: ProjectId::parse_wire(&project_id).ok_or(StoreError::InvalidBatch)?,
        session_id: SessionId::parse_wire(&session_id).ok_or(StoreError::InvalidBatch)?,
        turn_id: row.get(8)?,
        actor: parse_actor(&actor)?,
        correlation_id: row.get(10)?,
        causation_id: row.get(11)?,
        source: serde_json::from_str::<SourceRef>(&source_json)?,
        payload: serde_json::from_str::<EventPayload>(&payload_json)?,
        privacy: parse_privacy(&privacy)?,
    })
    .map_err(|_| StoreError::InvalidBatch)
}

fn parse_actor(value: &str) -> Result<Actor, StoreError> {
    match value {
        "human" => Ok(Actor::Human),
        "agent" => Ok(Actor::Agent),
        "tool" => Ok(Actor::Tool),
        "system" => Ok(Actor::System),
        _ => Err(StoreError::InvalidBatch),
    }
}

fn parse_privacy(value: &str) -> Result<PrivacyLabel, StoreError> {
    match value {
        "restricted_local" => Ok(PrivacyLabel::RestrictedLocal),
        "private_local" => Ok(PrivacyLabel::PrivateLocal),
        "derived_local" => Ok(PrivacyLabel::DerivedLocal),
        "sync_eligible" => Ok(PrivacyLabel::SyncEligible),
        _ => Err(StoreError::InvalidBatch),
    }
}

#[cfg(feature = "test-support")]
fn count_table(connection: &rusqlite::Connection, table: &'static str) -> Result<u64, StoreError> {
    let query = match table {
        "projects" => "SELECT count(*) FROM projects",
        "agent_runs" => "SELECT count(*) FROM agent_runs",
        "action_facts" => "SELECT count(*) FROM action_facts",
        "artifacts" => "SELECT count(*) FROM artifacts",
        "verification_facts" => "SELECT count(*) FROM verification_facts",
        "evidence_objects" => "SELECT count(*) FROM evidence_objects",
        "work_evidence" => "SELECT count(*) FROM work_evidence",
        _ => return Err(StoreError::InvalidBatch),
    };
    let value: i64 = connection.query_row(query, [], |row| row.get(0))?;
    u64::try_from(value).map_err(|_| StoreError::InvalidBatch)
}

fn validate_source_generation(source_id: &str, generation: u64) -> Result<(), StoreError> {
    if source_id.is_empty()
        || source_id.len() > 128
        || generation == 0
        || generation > i64::MAX as u64
    {
        return Err(StoreError::InvalidBatch);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use std::{
        panic::{AssertUnwindSafe, catch_unwind},
        sync::{Arc, Barrier},
        time::Duration,
    };

    use super::{Checkout, READ_POOL_SIZE, ReadPool};

    #[test]
    fn checkout_returns_connection_after_worker_panic() {
        let directory = tempfile::tempdir().unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;

            std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o700))
                .unwrap();
        }
        let database = directory.path().join("state.db");
        let _store = crate::Store::open_new(&database).unwrap();
        let pool = ReadPool::open(&database, READ_POOL_SIZE).unwrap();
        let permit = pool.inner.available.clone().try_acquire_owned().unwrap();
        let inner = pool.inner.clone();

        let _ = catch_unwind(AssertUnwindSafe(|| {
            let _checkout = Checkout::new(inner, permit).unwrap();
            panic!("simulated worker panic");
        }));

        assert_eq!(pool.inner.connections.lock().unwrap().len(), READ_POOL_SIZE);
        assert_eq!(pool.inner.available.available_permits(), READ_POOL_SIZE);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn checkout_returns_connection_after_caller_cancellation() {
        let directory = tempfile::tempdir().unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;

            std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o700))
                .unwrap();
        }
        let database = directory.path().join("state.db");
        let _store = crate::Store::open_new(&database).unwrap();
        let pool = ReadPool::open(&database, READ_POOL_SIZE).unwrap();
        let started = Arc::new(Barrier::new(2));
        let release = Arc::new(Barrier::new(2));
        let worker_started = Arc::clone(&started);
        let worker_release = Arc::clone(&release);
        let worker_pool = pool.clone();
        let caller = tokio::spawn(async move {
            worker_pool
                .execute(move |_| {
                    worker_started.wait();
                    worker_release.wait();
                    Ok(())
                })
                .await
        });

        tokio::task::block_in_place(|| started.wait());
        caller.abort();
        tokio::task::block_in_place(|| release.wait());
        assert!(caller.await.unwrap_err().is_cancelled());

        let permits = tokio::time::timeout(
            Duration::from_secs(2),
            pool.inner
                .available
                .clone()
                .acquire_many_owned(u32::try_from(READ_POOL_SIZE).unwrap()),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(pool.inner.connections.lock().unwrap().len(), READ_POOL_SIZE);
        drop(permits);
        assert_eq!(pool.inner.available.available_permits(), READ_POOL_SIZE);
    }

    #[tokio::test]
    async fn checkout_returns_connection_after_query_error() {
        let directory = tempfile::tempdir().unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;

            std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o700))
                .unwrap();
        }
        let database = directory.path().join("state.db");
        let _store = crate::Store::open_new(&database).unwrap();
        let pool = ReadPool::open(&database, READ_POOL_SIZE).unwrap();

        let error = pool
            .execute(|connection| {
                connection.execute_batch("SELECT * FROM definitely_missing_table")?;
                Ok(())
            })
            .await
            .unwrap_err();

        assert!(matches!(error, crate::StoreError::Sqlite(_)));
        assert_eq!(pool.inner.connections.lock().unwrap().len(), READ_POOL_SIZE);
        assert_eq!(pool.inner.available.available_permits(), READ_POOL_SIZE);
    }
}
