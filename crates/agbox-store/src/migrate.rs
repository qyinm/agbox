use std::{
    collections::HashSet,
    ffi::{OsStr, OsString},
    fs::File,
    io,
    path::Path,
    time::Duration,
};

use rusqlite::{Connection, OpenFlags, TransactionBehavior};

use crate::{
    StoreError,
    fs_security::{create_owner_temp_file, open_bound_owner_directory, validate_owner_file},
};

const INITIAL: &str = include_str!("schema/0001_initial.sql");
const REQUIRED_V1_TABLES: &[&str] = &[
    "schema_migrations",
    "projects",
    "sources",
    "source_generations",
    "source_cursors",
    "source_observations",
    "activity_events",
    "event_evidence",
    "content_refs",
    "schema_fingerprints",
    "ingestion_faults",
    "agent_runs",
    "work_items",
    "evidence_objects",
    "work_assertions",
    "work_edges",
    "artifacts",
    "work_evidence",
    "work_contract_revisions",
    "extractor_runs",
    "handoff_reads",
    "audit_events",
    "evidence_delete_queue",
    "reducer_watermarks",
    "action_facts",
    "verification_facts",
    "work_search",
];

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
    let (canonical_parent, directory) = open_bound_owner_directory(parent)?;
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
    if version == 1 {
        validate_v1_schema(&connection)?;
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
        validate_v1_schema(&connection)?;
    }

    validate_owner_file(&directory, name)?;
    validate_optional_owner_file(&directory, &wal_name)?;
    validate_optional_owner_file(&directory, &shm_name)?;

    Ok(OpenedWriter {
        connection,
        directory,
    })
}

#[derive(Debug, Eq, PartialEq)]
struct ColumnShape {
    position: i64,
    name: String,
    declared_type: String,
    not_null: bool,
    primary_key_position: i64,
}

fn validate_v1_schema(connection: &Connection) -> Result<(), StoreError> {
    match v1_schema_is_compatible(connection) {
        Ok(true) => Ok(()),
        Ok(false) | Err(_) => Err(StoreError::IncompatibleSchema),
    }
}

fn v1_schema_is_compatible(connection: &Connection) -> rusqlite::Result<bool> {
    let mut table_statement = connection.prepare(
        "SELECT name
         FROM sqlite_schema
         WHERE type IN ('table', 'view')",
    )?;
    let tables: HashSet<String> = table_statement
        .query_map([], |row| row.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    if !REQUIRED_V1_TABLES
        .iter()
        .all(|required| tables.contains(*required))
    {
        return Ok(false);
    }

    let marker_exists: bool = connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM schema_migrations WHERE version = 1)",
        [],
        |row| row.get(0),
    )?;
    if !marker_exists {
        return Ok(false);
    }

    let mut column_statement = connection.prepare("PRAGMA table_info(source_cursors)")?;
    let columns: Vec<ColumnShape> = column_statement
        .query_map([], |row| {
            Ok(ColumnShape {
                position: row.get(0)?,
                name: row.get(1)?,
                declared_type: row.get(2)?,
                not_null: row.get::<_, i64>(3)? == 1,
                primary_key_position: row.get(5)?,
            })
        })?
        .collect::<rusqlite::Result<_>>()?;
    let expected = [
        (0, "source_id", "TEXT", true, 1),
        (1, "generation", "INTEGER", true, 2),
        (2, "cursor_offset", "INTEGER", true, 0),
        (3, "parser_state", "BLOB", true, 0),
        (4, "last_commit_digest", "TEXT", true, 0),
        (5, "updated_at", "TEXT", true, 0),
    ];
    Ok(columns.len() == expected.len()
        && columns.iter().zip(expected).all(
            |(column, (position, name, declared_type, not_null, primary_key_position))| {
                column.position == position
                    && column.name == name
                    && column.declared_type.eq_ignore_ascii_case(declared_type)
                    && column.not_null == not_null
                    && column.primary_key_position == primary_key_position
            },
        ))
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
