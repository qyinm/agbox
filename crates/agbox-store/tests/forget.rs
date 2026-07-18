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

fn seed_contract(connection: &rusqlite::Connection, project: &str, work: &str) {
    let contract = serde_json::json!({
        "contract_id":"contract_test", "work_id":work, "revision":1, "project_id":project,
        "objective":"old objective", "status":"active", "summary":"old summary",
        "completed_steps":[], "next_actions":[], "blockers":[], "constraints":[], "completion_criteria":[], "artifacts":[], "verification":[],
        "evidence_refs":["evt_test"], "field_evidence":{"status":["evt_test"]}, "evidence_truncated":false,
        "confidence_basis_points":10000, "created_at":"2026-01-01T00:00:00Z", "extractor_version":"fixture",
        "fact_set_digest":"fixture", "material_content_hash":"b3:fixture", "projection_state":{}
    });
    connection.execute("INSERT INTO work_contract_revisions(contract_id,work_id,revision,contract_json,extractor_version,created_at) VALUES ('contract_test',?1,1,?2,'fixture','2026-01-01T00:00:00Z')", params![work, contract.to_string()]).unwrap();
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
    let unicode = fts_literal_query(&format!("{} tail", "한".repeat(40))).unwrap();
    let first_term = unicode.split('"').nth(1).unwrap();
    assert!(first_term.len() <= 64);
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

#[tokio::test]
async fn human_correction_preserves_the_prior_revision_and_rejects_cross_project() {
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
    connection.execute("INSERT INTO activity_events(event_id,semantic_key,schema_version,occurred_at,observed_at,project_id,session_id,actor,source_json,payload_json,privacy) VALUES ('evt_test','correction',1,'2026-01-01T00:00:00Z','2026-01-01T00:00:00Z','project_a','session_test','human','{}','{\"type\":\"session.started\",\"context\":null}','private_local')", []).unwrap();
    seed_contract(&connection, "project_a", "work_a");
    let before: String = connection.query_row("SELECT contract_json FROM work_contract_revisions WHERE work_id='work_a' AND revision=1", [], |row| row.get(0)).unwrap();
    drop(connection);
    let runtime = StoreRuntime::start_with_key_provider(
        &database,
        Arc::new(MemoryKeyProvider::fixed([82; 32])),
    )
    .await
    .unwrap();
    assert!(matches!(
        runtime
            .writer()
            .apply_human_correction(
                ProjectId::for_test("project_b"),
                WorkId::for_test("work_a"),
                "summary".into(),
                "foreign".into(),
                OffsetDateTime::now_utc()
            )
            .await,
        Err(StoreError::InvalidReference)
    ));
    let receipt = runtime
        .writer()
        .apply_human_correction(
            ProjectId::for_test("project_a"),
            WorkId::for_test("work_a"),
            "summary".into(),
            "new summary".into(),
            OffsetDateTime::now_utc(),
        )
        .await
        .unwrap();
    assert_eq!(receipt.revision, 2);
    runtime.shutdown().await.unwrap();
    let connection = rusqlite::Connection::open(&database).unwrap();
    let count: i64 = connection
        .query_row(
            "SELECT count(*) FROM work_contract_revisions WHERE work_id='work_a'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let prior: String = connection.query_row("SELECT contract_json FROM work_contract_revisions WHERE work_id='work_a' AND revision=1", [], |row| row.get(0)).unwrap();
    let current: String = connection.query_row("SELECT contract_json FROM work_contract_revisions WHERE work_id='work_a' AND revision=2", [], |row| row.get(0)).unwrap();
    assert_eq!(count, 2);
    assert_eq!(prior, before);
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&current).unwrap()["summary"],
        "new summary"
    );
}
