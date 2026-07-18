use std::{
    collections::HashSet,
    ffi::{OsStr, OsString},
    fs::File,
    io,
    path::Path,
    time::Duration,
};

#[cfg(unix)]
use std::os::unix::ffi::OsStrExt;

use rusqlite::{Connection, OpenFlags, OptionalExtension, TransactionBehavior};

use crate::{
    StoreError,
    fs_security::{create_owner_temp_file, open_bound_owner_directory, validate_owner_file},
};

const INITIAL: &str = include_str!("schema/0001_initial.sql");
const MIGRATION_2: &str = "
CREATE TABLE source_generation_identities (
    source_id TEXT NOT NULL,
    generation INTEGER NOT NULL,
    file_identity TEXT NOT NULL,
    PRIMARY KEY (source_id, generation),
    FOREIGN KEY (source_id, generation)
        REFERENCES source_generations(source_id, generation)
) STRICT;
INSERT INTO source_generation_identities(source_id, generation, file_identity)
SELECT source_generations.source_id, source_generations.generation, sources.file_identity
FROM source_generations
INNER JOIN sources USING (source_id);
";
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

    let database_exists = owner_file_exists(&directory, name)?;
    validate_optional_owner_file(&directory, &wal_name)?;
    validate_optional_owner_file(&directory, &shm_name)?;

    let database_path = canonical_parent.join(name);
    let version = if database_exists {
        validate_existing_database(&database_path)?
    } else {
        ensure_owner_file(&directory, name)?;
        0
    };

    let mut connection = Connection::open_with_flags(
        database_path,
        OpenFlags::SQLITE_OPEN_READ_WRITE
            | OpenFlags::SQLITE_OPEN_CREATE
            | OpenFlags::SQLITE_OPEN_NOFOLLOW
            | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    connection.busy_timeout(Duration::from_secs(5))?;

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
        transaction.pragma_update(None, "user_version", 2)?;
        transaction.execute(
            "INSERT INTO schema_migrations(version, applied_at)
             VALUES (1, strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))",
            [],
        )?;
        transaction.execute(
            "INSERT INTO schema_migrations(version, applied_at)
             VALUES (2, strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))",
            [],
        )?;
        transaction.commit()?;
        checkpoint_schema_contract(&connection)?;
        validate_v2_schema(&connection)?;
    } else if version == 1 {
        let transaction = connection.transaction()?;
        if v1_has_ambiguous_generation_history(&transaction)? {
            return Err(StoreError::IncompatibleSchema);
        }
        transaction.execute_batch(MIGRATION_2)?;
        transaction.pragma_update(None, "user_version", 2)?;
        transaction.execute(
            "INSERT INTO schema_migrations(version, applied_at)
             VALUES (2, strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))",
            [],
        )?;
        transaction.commit()?;
        checkpoint_schema_contract(&connection)?;
        validate_v2_schema(&connection)?;
    }

    validate_owner_file(&directory, name)?;
    validate_optional_owner_file(&directory, &wal_name)?;
    validate_optional_owner_file(&directory, &shm_name)?;

    Ok(OpenedWriter {
        connection,
        directory,
    })
}

fn v1_has_ambiguous_generation_history(connection: &Connection) -> rusqlite::Result<bool> {
    connection.query_row(
        "SELECT EXISTS(
             SELECT 1 FROM source_generations
             GROUP BY source_id
             HAVING count(*) > 1
         )",
        [],
        |row| row.get(0),
    )
}

fn validate_existing_database(path: &Path) -> Result<i64, StoreError> {
    // Schema DDL, migration markers, and user_version are checkpointed into
    // the main database before a newly initialized writer is returned.
    // Runtime WAL traffic is DML-only, so immutable mode can validate the
    // stable schema contract without recovery or WAL-index writes. Every
    // future schema migration must preserve this checkpoint boundary.
    let connection = Connection::open_with_flags(
        immutable_database_uri(path),
        OpenFlags::SQLITE_OPEN_READ_ONLY
            | OpenFlags::SQLITE_OPEN_URI
            | OpenFlags::SQLITE_OPEN_NOFOLLOW
            | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    let version: i64 = connection.pragma_query_value(None, "user_version", |row| row.get(0))?;
    match version {
        1 => validate_v1_schema(&connection).map(|()| 1),
        2 => validate_v2_schema(&connection).map(|()| 2),
        0 => Err(StoreError::IncompatibleSchema),
        unsupported => Err(StoreError::UnsupportedSchema(unsupported)),
    }
}

fn checkpoint_schema_contract(connection: &Connection) -> Result<(), StoreError> {
    let (busy, log_frames, checkpointed_frames): (i64, i64, i64) =
        connection.query_row("PRAGMA wal_checkpoint(FULL)", [], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?))
        })?;
    if busy == 0 && log_frames == checkpointed_frames {
        Ok(())
    } else {
        Err(rusqlite::Error::InvalidQuery.into())
    }
}

fn immutable_database_uri(path: &Path) -> String {
    #[cfg(unix)]
    let bytes = path.as_os_str().as_bytes();
    #[cfg(not(unix))]
    let path_text = path.to_string_lossy();
    #[cfg(not(unix))]
    let bytes = path_text.as_bytes();

    let mut uri = String::with_capacity(bytes.len().saturating_mul(3).saturating_add(17));
    uri.push_str("file:");
    for byte in bytes {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'/' | b'-' | b'.' | b'_' | b'~') {
            uri.push(char::from(*byte));
        } else {
            const HEX: &[u8; 16] = b"0123456789ABCDEF";
            uri.push('%');
            uri.push(char::from(HEX[usize::from(byte >> 4)]));
            uri.push(char::from(HEX[usize::from(byte & 0x0f)]));
        }
    }
    uri.push_str("?immutable=1");
    uri
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

fn validate_v2_schema(connection: &Connection) -> Result<(), StoreError> {
    validate_v1_schema(connection)?;
    match v2_schema_is_compatible(connection) {
        Ok(true) => Ok(()),
        Ok(false) | Err(_) => Err(StoreError::IncompatibleSchema),
    }
}

fn v2_schema_is_compatible(connection: &Connection) -> rusqlite::Result<bool> {
    let marker_exists: bool = connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM schema_migrations WHERE version = 2)",
        [],
        |row| row.get(0),
    )?;
    if !marker_exists {
        return Ok(false);
    }
    let table_kind: Option<String> = connection
        .query_row(
            "SELECT type FROM sqlite_schema WHERE name = 'source_generation_identities'",
            [],
            |row| row.get(0),
        )
        .optional()?;
    if table_kind.as_deref() != Some("table") {
        return Ok(false);
    }
    let mut statement = connection.prepare("PRAGMA table_info(source_generation_identities)")?;
    let columns: Vec<ColumnShape> = statement
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
        (2, "file_identity", "TEXT", true, 0),
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

fn v1_schema_is_compatible(connection: &Connection) -> rusqlite::Result<bool> {
    let mut table_statement = connection.prepare(
        "SELECT name
         FROM sqlite_schema
         WHERE type = 'table'",
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

fn owner_file_exists(directory: &File, name: &OsStr) -> Result<bool, io::Error> {
    match validate_owner_file(directory, name) {
        Ok(()) => Ok(true),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error),
    }
}
