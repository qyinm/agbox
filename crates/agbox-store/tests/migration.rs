#![allow(clippy::unwrap_used)]

use std::{
    fs,
    os::unix::fs::{PermissionsExt, symlink},
    path::Path,
};

use agbox_store::{Store, StoreError};
use rusqlite::{Connection, params};

#[test]
fn creates_v2_schema_without_touching_legacy_db() {
    let home = tempfile::tempdir().unwrap();
    set_mode(home.path(), 0o700);
    let legacy = home.path().join("agbox.db");
    std::fs::write(&legacy, b"legacy sentinel").unwrap();

    let store = Store::open_new(home.path().join("state.db")).unwrap();
    assert_eq!(store.schema_version().unwrap(), 1);
    assert_eq!(store.journal_mode().unwrap(), "wal");
    assert_eq!(std::fs::read(&legacy).unwrap(), b"legacy sentinel");
    for table in [
        "sources",
        "source_generations",
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
    let connection = Connection::open(home.path().join("state.db")).unwrap();
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
    connection.pragma_update(None, "user_version", 2).unwrap();
    drop(connection);
    set_mode(&database, 0o600);

    let error = Store::open_new(&database).unwrap_err();

    assert!(matches!(error, StoreError::UnsupportedSchema(2)));
    assert_eq!(read_user_version(&database), 2);
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
