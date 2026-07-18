#![allow(clippy::unwrap_used)]

use agbox_store::fts_literal_query;
use std::sync::Arc;

use agbox_core::{ProjectId, WorkId};
use agbox_store::{ForgetTarget, MemoryKeyProvider, Store, StoreError, StoreRuntime};
use rusqlite::params;
use time::OffsetDateTime;

fn private_tempdir() -> tempfile::TempDir {
    let directory = tempfile::tempdir().unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    directory
}

fn seed_project(connection: &rusqlite::Connection, project: &str, work: &str) {
    connection.execute("INSERT INTO projects(project_id,repository_identity,encrypted_root_path,created_at,updated_at) VALUES (?1,?2,X'00','now','now')", params![project, format!("repo_{project}")]).unwrap();
    connection.execute("INSERT INTO work_items(work_id,project_id,status,created_at,updated_at) VALUES (?1,?2,'active','now','now')", params![work, project]).unwrap();
}

#[test]
fn hostile_fts_syntax_becomes_bounded_literal_terms() {
    let expression = fts_literal_query("alpha OR beta* \"quoted\"").unwrap();
    assert_eq!(
        expression,
        "\"alpha\" AND \"OR\" AND \"beta*\" AND \"\"\"quoted\"\"\""
    );
    assert!(fts_literal_query("   ").is_err());
    assert!(fts_literal_query(&"a ".repeat(600)).is_err());
}

#[tokio::test]
async fn scoped_forget_rejects_a_guessed_foreign_work_id_and_project_forget_removes_content_refs() {
    let directory = private_tempdir();
    let database = directory.path().join("state.db");
    let store = Store::open_new(&database).unwrap();
    drop(store);
    let connection = rusqlite::Connection::open(&database).unwrap();
    connection
        .pragma_update(None, "foreign_keys", "ON")
        .unwrap();
    seed_project(&connection, "project_a", "work_a");
    seed_project(&connection, "project_b", "work_b");
    connection.execute("INSERT INTO content_refs(content_ref_id,project_id,content_hash,byte_length,media_type,local_locator,redacted_excerpt,truncated,privacy,disclosure_class) VALUES ('content_a','project_a','b3:test',0,'text/plain',NULL,NULL,0,'derived_local','derived_text')", []).unwrap();
    drop(connection);
    let runtime = StoreRuntime::start_with_key_provider(
        &database,
        Arc::new(MemoryKeyProvider::fixed([81; 32])),
    )
    .await
    .unwrap();
    let result = runtime
        .writer()
        .forget_in_project(
            &ProjectId::for_test("project_a"),
            ForgetTarget::Work(WorkId::for_test("work_b")),
            "human_cli",
            OffsetDateTime::now_utc(),
        )
        .await;
    assert!(matches!(result, Err(StoreError::InvalidReference)));
    let outcome = runtime
        .writer()
        .forget_in_project(
            &ProjectId::for_test("project_a"),
            ForgetTarget::Project(ProjectId::for_test("project_a")),
            "human_cli",
            OffsetDateTime::now_utc(),
        )
        .await
        .unwrap();
    assert!(outcome.deleted_rows > 0);
    runtime.shutdown().await.unwrap();
    let connection = rusqlite::Connection::open(&database).unwrap();
    let content: i64 = connection
        .query_row(
            "SELECT count(*) FROM content_refs WHERE project_id = 'project_a'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let foreign_work: i64 = connection
        .query_row(
            "SELECT count(*) FROM work_items WHERE work_id = 'work_b'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(content, 0);
    assert_eq!(foreign_work, 1);
}
