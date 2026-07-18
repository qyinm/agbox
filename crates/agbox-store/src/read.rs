use std::{
    fmt,
    fs::File,
    io,
    path::Path,
    sync::{Arc, Mutex},
};

use rusqlite::{OpenFlags, OptionalExtension};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

use crate::{CursorState, StoreError, fs_security};

pub const READ_POOL_SIZE: usize = 4;

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
