#![allow(clippy::unwrap_used)]

use std::{
    fs,
    os::unix::fs::{PermissionsExt, symlink},
    path::Path,
};

#[cfg(feature = "test-support")]
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

#[cfg(feature = "test-support")]
use agbox_store::{CryptoError, KeyProvider, MemoryKeyProvider, StoreRuntime};
use agbox_store::{Store, StoreError};
use rusqlite::{Connection, params};
#[cfg(feature = "test-support")]
use zeroize::Zeroizing;

#[cfg(feature = "test-support")]
#[derive(Debug)]
struct CountingKeyProvider {
    calls: Arc<AtomicUsize>,
}

#[cfg(feature = "test-support")]
impl KeyProvider for CountingKeyProvider {
    fn master_key(&self) -> Result<Zeroizing<[u8; 32]>, CryptoError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(Zeroizing::new([37_u8; 32]))
    }
}

#[test]
fn creates_v2_schema_without_touching_legacy_db() {
    let home = tempfile::tempdir().unwrap();
    set_mode(home.path(), 0o700);
    let legacy = home.path().join("agbox.db");
    std::fs::write(&legacy, b"legacy sentinel").unwrap();

    let store = Store::open_new(home.path().join("state.db")).unwrap();
    assert_eq!(store.schema_version().unwrap(), 2);
    assert_eq!(store.journal_mode().unwrap(), "wal");
    assert_eq!(std::fs::read(&legacy).unwrap(), b"legacy sentinel");
    for table in [
        "sources",
        "source_generations",
        "source_generation_identities",
        "source_cursors",
        "source_observations",
        "activity_events",
        "evidence_objects",
        "event_evidence",
        "content_refs",
        "schema_fingerprints",
        "ingestion_faults",
        "projects",
        "agent_runs",
        "work_items",
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
    ] {
        assert!(store.table_exists(table).unwrap(), "missing {table}");
    }
    let connection = Connection::open_with_flags(
        format!(
            "file:{}?immutable=1",
            home.path()
                .canonicalize()
                .unwrap()
                .join("state.db")
                .display()
        ),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY
            | rusqlite::OpenFlags::SQLITE_OPEN_URI
            | rusqlite::OpenFlags::SQLITE_OPEN_NOFOLLOW
            | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .unwrap();
    assert_eq!(
        connection
            .pragma_query_value(None, "user_version", |row| row.get::<_, i64>(0))
            .unwrap(),
        2
    );
    let manifest_column: (String, i64) = connection
        .query_row(
            "SELECT type, \"notnull\"
             FROM pragma_table_info('source_cursors')
             WHERE name = 'last_commit_digest'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(manifest_column, ("TEXT".into(), 1));
    drop(connection);
    drop(store);
    let reopened = Store::open_new(home.path().join("state.db")).unwrap();
    assert_eq!(reopened.schema_version().unwrap(), 2);
}

#[test]
fn migrates_v1_generation_identities_before_opening_writer() {
    let home = tempfile::tempdir().unwrap();
    set_mode(home.path(), 0o700);
    let database = home.path().join("state.db");
    let store = Store::open_new(&database).unwrap();
    drop(store);

    let connection = Connection::open(&database).unwrap();
    connection
        .execute_batch(
            "DROP TABLE source_generation_identities;
             DELETE FROM schema_migrations WHERE version = 2;
             PRAGMA user_version = 1;
             PRAGMA wal_checkpoint(TRUNCATE);",
        )
        .unwrap();
    drop(connection);

    let migrated = Store::open_new(&database).unwrap();
    assert_eq!(migrated.schema_version().unwrap(), 2);
    assert!(
        migrated
            .table_exists("source_generation_identities")
            .unwrap()
    );
}

#[test]
fn rejects_the_reserved_legacy_database_without_changing_it() {
    let home = tempfile::tempdir().unwrap();
    let legacy = home.path().join("agbox.db");
    fs::write(&legacy, b"legacy sentinel").unwrap();

    let error = Store::open_new(&legacy).unwrap_err();

    assert!(matches!(error, StoreError::LegacyDatabaseReserved));
    assert_eq!(fs::read(&legacy).unwrap(), b"legacy sentinel");
}

#[test]
fn rejects_an_unsupported_schema_version_without_migrating_it() {
    let home = tempfile::tempdir().unwrap();
    set_mode(home.path(), 0o700);
    let database = home.path().join("state.db");
    let connection = Connection::open(&database).unwrap();
    connection.pragma_update(None, "user_version", 3).unwrap();
    drop(connection);
    set_mode(&database, 0o600);

    let error = Store::open_new(&database).unwrap_err();

    assert!(matches!(error, StoreError::UnsupportedSchema(3)));
    assert_eq!(read_user_version(&database), 3);
    assert!(!home.path().join("state.db-wal").exists());
    assert!(!home.path().join("state.db-shm").exists());
}

#[test]
fn reopens_a_database_whose_canonical_path_needs_uri_percent_encoding() {
    let home = tempfile::tempdir().unwrap();
    set_mode(home.path(), 0o700);
    let database = home.path().join("state ?#%.db");

    let store = Store::open_new(&database).unwrap();
    assert_eq!(store.schema_version().unwrap(), 2);
    drop(store);

    let reopened = Store::open_new(&database).unwrap();
    assert_eq!(reopened.schema_version().unwrap(), 2);
}

#[test]
fn rejects_an_earlier_pre_manifest_v1_without_mutating_it() {
    let home = tempfile::tempdir().unwrap();
    set_mode(home.path(), 0o700);
    let database = home.path().join("state.db");
    create_pre_manifest_v1(&database);
    let bytes_before = fs::read(&database).unwrap();

    let error = Store::open_new(&database).unwrap_err();

    assert!(matches!(error, StoreError::IncompatibleSchema));
    let debug = format!("{error:?}");
    let display = error.to_string();
    assert!(!debug.contains(home.path().to_string_lossy().as_ref()));
    assert!(!debug.contains("source_cursors"));
    assert!(!debug.contains("last_commit_digest"));
    assert!(!display.contains(home.path().to_string_lossy().as_ref()));
    assert!(!display.contains("source_cursors"));
    assert_eq!(fs::read(&database).unwrap(), bytes_before);
    assert!(!home.path().join("state.db-wal").exists());
    assert!(!home.path().join("state.db-shm").exists());
    assert_pre_manifest_v1_unchanged(&database);
}

#[test]
fn rejects_an_existing_v0_with_restored_sidecars_without_reinitializing_it() {
    let home = tempfile::tempdir().unwrap();
    set_mode(home.path(), 0o700);
    let database = home.path().join("state.db");
    restore_v0_with_crash_left_wal_snapshot(&database);
    let wal = home.path().join("state.db-wal");
    let shm = home.path().join("state.db-shm");
    let snapshots = [&database, &wal, &shm].map(|path| snapshot_file(path));

    let error = Store::open_new(&database).unwrap_err();

    assert!(matches!(error, StoreError::IncompatibleSchema));
    for (path, snapshot) in [&database, &wal, &shm].into_iter().zip(snapshots) {
        assert!(
            snapshot_file(path) == snapshot,
            "{} changed while rejecting existing v0",
            path.display()
        );
    }
}

#[cfg(feature = "test-support")]
#[tokio::test]
async fn runtime_rejects_an_earlier_pre_manifest_v1_before_sidecar_or_key_startup() {
    let home = tempfile::tempdir().unwrap();
    set_mode(home.path(), 0o700);
    let database = home.path().join("state.db");
    create_pre_manifest_v1(&database);
    let bytes_before = fs::read(&database).unwrap();

    let error = StoreRuntime::start_with_key_provider(
        &database,
        Arc::new(MemoryKeyProvider::fixed([31_u8; 32])),
    )
    .await
    .unwrap_err();

    assert!(matches!(error, StoreError::IncompatibleSchema));
    assert_eq!(fs::read(&database).unwrap(), bytes_before);
    assert!(!home.path().join("state.db-wal").exists());
    assert!(!home.path().join("state.db-shm").exists());
    assert_pre_manifest_v1_unchanged(&database);
}

#[cfg(feature = "test-support")]
#[tokio::test]
async fn runtime_rejects_incompatible_v1_without_touching_restored_live_wal_snapshot_or_keys() {
    let home = tempfile::tempdir().unwrap();
    set_mode(home.path(), 0o700);
    let database = home.path().join("state.db");
    create_pre_manifest_v1(&database);
    restore_crash_left_wal_snapshot(&database);
    let wal = home.path().join("state.db-wal");
    let shm = home.path().join("state.db-shm");
    let snapshots = [&database, &wal, &shm].map(|path| snapshot_file(path));
    let key_calls = Arc::new(AtomicUsize::new(0));

    let error = StoreRuntime::start_with_key_provider(
        &database,
        Arc::new(CountingKeyProvider {
            calls: Arc::clone(&key_calls),
        }),
    )
    .await
    .unwrap_err();

    assert!(matches!(error, StoreError::IncompatibleSchema));
    assert_eq!(key_calls.load(Ordering::SeqCst), 0);
    assert_eq!(
        format!("{error:?}"),
        "StoreError { kind: \"IncompatibleSchema\" }"
    );
    assert_eq!(
        error.to_string(),
        "database schema is incompatible with this runtime"
    );
    for (path, snapshot) in [&database, &wal, &shm].into_iter().zip(snapshots) {
        assert!(
            snapshot_file(path) == snapshot,
            "{} changed during incompatible-schema rejection",
            path.display()
        );
    }
}

#[test]
fn rejects_a_view_spoofing_a_required_v1_table() {
    let home = tempfile::tempdir().unwrap();
    set_mode(home.path(), 0o700);
    let database = home.path().join("state.db");
    create_v1_with_required_table_replaced_by_view(&database);
    let bytes_before = fs::read(&database).unwrap();

    let error = Store::open_new(&database).unwrap_err();

    assert!(matches!(error, StoreError::IncompatibleSchema));
    assert_eq!(fs::read(&database).unwrap(), bytes_before);
    assert!(!home.path().join("state.db-wal").exists());
    assert!(!home.path().join("state.db-shm").exists());
}

#[test]
fn disclosure_classes_are_required_and_checked_beside_stored_text() {
    let home = tempfile::tempdir().unwrap();
    set_mode(home.path(), 0o700);
    let database = home.path().join("state.db");
    let _store = Store::open_new(&database).unwrap();
    let connection = Connection::open(&database).unwrap();
    connection
        .execute(
            "INSERT INTO projects(
                project_id, repository_identity, encrypted_root_path, created_at, updated_at
             ) VALUES ('project', 'repository', X'00', 'now', 'now')",
            [],
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO work_items(work_id, project_id, status, created_at, updated_at)
             VALUES ('work', 'project', 'open', 'now', 'now')",
            [],
        )
        .unwrap();

    for statement in [
        "INSERT INTO content_refs(
            content_ref_id, project_id, content_hash, byte_length, media_type,
            local_locator, redacted_excerpt, truncated, privacy, disclosure_class
         ) VALUES (
            'bad-content', 'project', 'hash', 1, 'text/plain',
            NULL, 'excerpt', 0, 'private_local', 'unclassified'
         )",
        "INSERT INTO evidence_objects(
            evidence_id, project_id, owner_kind, owner_id, content_hash, media_type,
            privacy, byte_length, redacted_excerpt, disclosure_class, blob_state,
            created_at, expires_at, retired_at
         ) VALUES (
            'bad-evidence', 'project', 'event', 'owner', 'hash', 'text/plain',
            'private_local', 1, 'excerpt', 'unclassified', 'available',
            'now', NULL, NULL
         )",
        "INSERT INTO work_assertions(
            assertion_id, work_id, field, value, authority, privacy,
            disclosure_class, confidence_basis_points, created_at,
            supersedes_assertion_id
         ) VALUES (
            'bad-assertion', 'work', 'objective', 'value', 'human_intent',
            'private_local', 'unclassified', 10000, 'now', NULL
         )",
    ] {
        assert!(
            connection.execute(statement, []).is_err(),
            "unchecked disclosure class in {statement}"
        );
    }

    for (index, class) in [
        "human_intent",
        "agent_statement",
        "observed_state",
        "tool_result",
        "reasoning",
        "system_instruction",
        "developer_instruction",
        "derived_text",
    ]
    .into_iter()
    .enumerate()
    {
        connection
            .execute(
                "INSERT INTO content_refs(
                    content_ref_id, project_id, content_hash, byte_length, media_type,
                    local_locator, redacted_excerpt, truncated, privacy, disclosure_class
                 ) VALUES (
                    ?1, 'project', 'hash', 1, 'text/plain',
                    NULL, 'excerpt', 0, 'private_local', ?2
                 )",
                params![format!("content-{index}"), class],
            )
            .unwrap();
    }
}

#[test]
fn creates_owner_only_database_directory_and_sqlite_files() {
    let outer = tempfile::tempdir().unwrap();
    let home = outer.path().join("private-state");
    let database = home.join("state.db");

    let _store = Store::open_new(&database).unwrap();

    assert_mode(&home, 0o700);
    for name in ["state.db", "state.db-wal", "state.db-shm"] {
        let path = home.join(name);
        assert!(path.is_file(), "missing {}", path.display());
        assert_mode(&path, 0o600);
    }
}

#[test]
fn rejects_a_preexisting_non_private_parent_without_changing_its_mode() {
    let home = tempfile::tempdir().unwrap();
    set_mode(home.path(), 0o755);
    let database = home.path().join("state.db");

    let error = Store::open_new(&database).unwrap_err();

    assert!(matches!(error, StoreError::Io(_)));
    assert_mode(home.path(), 0o755);
    assert!(!database.exists());
}

#[test]
fn refuses_symlinks_for_database_and_sqlite_sidecars_without_following_them() {
    for name in ["state.db", "state.db-wal", "state.db-shm"] {
        let home = tempfile::tempdir().unwrap();
        set_mode(home.path(), 0o700);
        let target = home.path().join(format!("{name}.target"));
        let database = home.path().join("state.db");
        fs::write(&target, b"do not follow").unwrap();
        set_mode(&target, 0o600);
        symlink(&target, home.path().join(name)).unwrap();

        let error = Store::open_new(&database).unwrap_err();

        assert_eq!(fs::read(&target).unwrap(), b"do not follow");
        let debug = format!("{error:?}");
        assert!(
            !debug.contains(home.path().to_string_lossy().as_ref()),
            "error debug leaked the state path: {debug}"
        );
    }
}

fn create_pre_manifest_v1(path: &Path) {
    let current = include_str!("../src/schema/0001_initial.sql");
    let earlier = current.replace(
        concat!(
            "    -- Bounded whole-chunk identity for exact replay of only the latest commit.\n",
            "    last_commit_digest TEXT NOT NULL CHECK (length(last_commit_digest) = 64),\n",
        ),
        "",
    );
    assert_ne!(current, earlier);
    let connection = Connection::open(path).unwrap();
    connection.execute_batch(&earlier).unwrap();
    connection.pragma_update(None, "user_version", 1).unwrap();
    connection
        .execute(
            "INSERT INTO schema_migrations(version, applied_at) VALUES (1, 'before-manifest')",
            [],
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO projects(
                 project_id, repository_identity, encrypted_root_path, created_at, updated_at
             ) VALUES ('project_old', 'repository_old', X'00', 'created', 'updated')",
            [],
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO sources(
                 source_id, project_id, provider, root_class, encrypted_path,
                 file_identity, created_at, updated_at
             ) VALUES (
                 'source_old', 'project_old', 'codex', 'active', X'00',
                 'identity_old', 'created', 'updated'
             )",
            [],
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO source_generations(
                 source_id, generation, size_bytes, mtime, session_time,
                 schema_fingerprint, status
             ) VALUES ('source_old', 1, 42, 'mtime', NULL, NULL, 'active')",
            [],
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO source_cursors(
                 source_id, generation, cursor_offset, parser_state, updated_at
             ) VALUES ('source_old', 1, 42, X'7374617465', 'before-manifest')",
            [],
        )
        .unwrap();
    drop(connection);
    set_mode(path, 0o600);
}

fn restore_crash_left_wal_snapshot(path: &Path) {
    let connection = Connection::open(path).unwrap();
    let journal_mode: String = connection
        .pragma_query_value(None, "journal_mode", |row| row.get(0))
        .unwrap();
    assert_eq!(journal_mode, "delete");
    let journal_mode: String = connection
        .pragma_update_and_check(None, "journal_mode", "WAL", |row| row.get(0))
        .unwrap();
    assert_eq!(journal_mode, "wal");
    connection
        .pragma_update(None, "wal_autocheckpoint", 0)
        .unwrap();
    connection
        .execute(
            "INSERT INTO projects(
                 project_id, repository_identity, encrypted_root_path, created_at, updated_at
             ) VALUES ('wal_project', 'wal_repository', X'00', 'created', 'updated')",
            [],
        )
        .unwrap();
    assert!(path.with_file_name("state.db-wal").is_file());
    assert!(path.with_file_name("state.db-shm").is_file());
    for sidecar in [
        path,
        path.with_file_name("state.db-wal").as_path(),
        path.with_file_name("state.db-shm").as_path(),
    ] {
        set_mode(sidecar, 0o600);
    }
    let wal = path.with_file_name("state.db-wal");
    let shm = path.with_file_name("state.db-shm");
    let snapshots = [path, wal.as_path(), shm.as_path()].map(snapshot_file);
    drop(connection);
    for (file, (bytes, mode)) in [path, wal.as_path(), shm.as_path()]
        .into_iter()
        .zip(snapshots)
    {
        fs::write(file, bytes).unwrap();
        set_mode(file, mode);
    }
}

fn restore_v0_with_crash_left_wal_snapshot(path: &Path) {
    let connection = Connection::open(path).unwrap();
    let journal_mode: String = connection
        .pragma_update_and_check(None, "journal_mode", "WAL", |row| row.get(0))
        .unwrap();
    assert_eq!(journal_mode, "wal");
    connection
        .pragma_update(None, "wal_autocheckpoint", 0)
        .unwrap();
    connection
        .execute_batch(
            "CREATE TABLE unfinished(value TEXT);
             INSERT INTO unfinished(value) VALUES ('not-a-v1-schema');",
        )
        .unwrap();
    let wal = path.with_file_name("state.db-wal");
    let shm = path.with_file_name("state.db-shm");
    for file in [path, wal.as_path(), shm.as_path()] {
        set_mode(file, 0o600);
    }
    let snapshots = [path, wal.as_path(), shm.as_path()].map(snapshot_file);
    drop(connection);
    for (file, (bytes, mode)) in [path, wal.as_path(), shm.as_path()]
        .into_iter()
        .zip(snapshots)
    {
        fs::write(file, bytes).unwrap();
        set_mode(file, mode);
    }
}

fn create_v1_with_required_table_replaced_by_view(path: &Path) {
    let connection = Connection::open(path).unwrap();
    connection
        .execute_batch(include_str!("../src/schema/0001_initial.sql"))
        .unwrap();
    connection
        .execute_batch(
            "DROP INDEX action_facts_project_input;
             DROP TABLE action_facts;
             CREATE VIEW action_facts AS SELECT project_id FROM projects;",
        )
        .unwrap();
    connection.pragma_update(None, "user_version", 1).unwrap();
    connection
        .execute(
            "INSERT INTO schema_migrations(version, applied_at) VALUES (1, 'view-spoof')",
            [],
        )
        .unwrap();
    drop(connection);
    set_mode(path, 0o600);
}

fn snapshot_file(path: &Path) -> (Vec<u8>, u32) {
    (
        fs::read(path).unwrap(),
        fs::metadata(path).unwrap().permissions().mode() & 0o7777,
    )
}

fn assert_pre_manifest_v1_unchanged(path: &Path) {
    let connection = Connection::open(path).unwrap();
    let version: i64 = connection
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .unwrap();
    assert_eq!(version, 1);
    let columns: Vec<String> = connection
        .prepare("SELECT name FROM pragma_table_info('source_cursors') ORDER BY cid")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    assert_eq!(
        columns,
        [
            "source_id",
            "generation",
            "cursor_offset",
            "parser_state",
            "updated_at"
        ]
    );
    let cursor: (i64, Vec<u8>, String) = connection
        .query_row(
            "SELECT cursor_offset, parser_state, updated_at
             FROM source_cursors
             WHERE source_id = 'source_old' AND generation = 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(cursor, (42, b"state".to_vec(), "before-manifest".into()));
}

fn read_user_version(path: &Path) -> i64 {
    Connection::open(path)
        .unwrap()
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .unwrap()
}

fn set_mode(path: &Path, mode: u32) {
    let mut permissions = fs::metadata(path).unwrap().permissions();
    permissions.set_mode(mode);
    fs::set_permissions(path, permissions).unwrap();
}

fn assert_mode(path: &Path, expected: u32) {
    let actual = fs::metadata(path).unwrap().permissions().mode() & 0o7777;
    assert_eq!(actual, expected, "unexpected mode for {}", path.display());
}
