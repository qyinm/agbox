use std::{
    ffi::{OsStr, OsString},
    fs::File,
    io,
    path::Path,
    time::Duration,
};

use rusqlite::{Connection, OpenFlags, TransactionBehavior};

use crate::{
    StoreError,
    fs_security::{create_owner_temp_file, open_bound_private_directory, validate_owner_file},
};

const INITIAL: &str = include_str!("schema/0001_initial.sql");

pub(crate) struct OpenedWriter {
    pub(crate) connection: Connection,
    pub(crate) directory: File,
}

pub(crate) fn open_writer(path: &Path) -> Result<OpenedWriter, StoreError> {
    let name = path.file_name().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "state database path has no file name",
        )
    })?;
    let parent = database_parent(path)?;
    let (canonical_parent, directory) = open_bound_private_directory(parent)?;
    let wal_name = sidecar_name(name, "-wal");
    let shm_name = sidecar_name(name, "-shm");

    validate_optional_owner_file(&directory, name)?;
    validate_optional_owner_file(&directory, &wal_name)?;
    validate_optional_owner_file(&directory, &shm_name)?;
    ensure_owner_file(&directory, name)?;

    let database_path = canonical_parent.join(name);
    let mut connection = Connection::open_with_flags(
        database_path,
        OpenFlags::SQLITE_OPEN_READ_WRITE
            | OpenFlags::SQLITE_OPEN_CREATE
            | OpenFlags::SQLITE_OPEN_NOFOLLOW
            | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    connection.busy_timeout(Duration::from_secs(5))?;

    let version: i64 = connection.pragma_query_value(None, "user_version", |row| row.get(0))?;
    if version != 0 && version != 1 {
        return Err(StoreError::UnsupportedSchema(version));
    }

    // Reserve SQLite's sidecar names through the held directory descriptor.
    // SQLite then opens owner-only regular files instead of creating them by
    // path during WAL initialization.
    ensure_owner_file(&directory, &wal_name)?;
    ensure_owner_file(&directory, &shm_name)?;

    connection.pragma_update(None, "journal_mode", "WAL")?;
    connection.pragma_update(None, "synchronous", "FULL")?;
    connection.pragma_update(None, "foreign_keys", "ON")?;
    connection.set_transaction_behavior(TransactionBehavior::Immediate);

    if version == 0 {
        let transaction = connection.transaction()?;
        transaction.execute_batch(INITIAL)?;
        transaction.pragma_update(None, "user_version", 1)?;
        transaction.execute(
            "INSERT INTO schema_migrations(version, applied_at)
             VALUES (1, strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))",
            [],
        )?;
        transaction.commit()?;
    }

    validate_owner_file(&directory, name)?;
    validate_optional_owner_file(&directory, &wal_name)?;
    validate_optional_owner_file(&directory, &shm_name)?;

    Ok(OpenedWriter {
        connection,
        directory,
    })
}

fn database_parent(path: &Path) -> Result<&Path, io::Error> {
    path.parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "state database path has no parent directory",
            )
        })
}

fn sidecar_name(name: &OsStr, suffix: &str) -> OsString {
    let mut sidecar = name.to_os_string();
    sidecar.push(suffix);
    sidecar
}

fn ensure_owner_file(directory: &File, name: &OsStr) -> Result<(), io::Error> {
    match validate_owner_file(directory, name) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            match create_owner_temp_file(directory, name) {
                Ok(file) => {
                    file.sync_all()?;
                    validate_owner_file(directory, name)
                }
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                    validate_owner_file(directory, name)
                }
                Err(error) => Err(error),
            }
        }
        Err(error) => Err(error),
    }
}

fn validate_optional_owner_file(directory: &File, name: &OsStr) -> Result<(), io::Error> {
    match validate_owner_file(directory, name) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}
