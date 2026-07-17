#![allow(clippy::unwrap_used)]

use std::{os::unix::fs::PermissionsExt, sync::Arc};

use std::path::Path;

use agbox_core::{
    ActivityEventV1, ContentRef, DisclosureClass, EvidenceId, PrivacyLabel, ProjectId, WorkId,
};
use agbox_store::{
    ContentRefWrite, EvidenceLink, EvidenceOwner, EvidenceWrite, IngestionChunk, MemoryKeyProvider,
    Store, StoreError, StoreRuntime, stable_content_ref_id,
};
use rusqlite::params;
use zeroize::Zeroizing;

fn private_tempdir() -> tempfile::TempDir {
    let directory = tempfile::tempdir().unwrap();
    std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    directory
}

fn seed_registered_source(database: &Path, source_id: &str, generation: u64) {
    let store = Store::open_new(database).unwrap();
    drop(store);
    let connection = rusqlite::Connection::open(database).unwrap();
    connection
        .pragma_update(None, "foreign_keys", "ON")
        .unwrap();
    connection
        .execute(
            "INSERT INTO projects(
                 project_id, repository_identity, encrypted_root_path, created_at, updated_at
             ) VALUES ('project_fixture', 'repository_fixture', X'00', 'now', 'now')",
            [],
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO sources(
                 source_id, project_id, provider, root_class, encrypted_path,
                 file_identity, created_at, updated_at
             ) VALUES (?1, 'project_fixture', 'codex', 'active', X'00',
                 'fixture-identity', 'now', 'now')",
            [source_id],
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO source_generations(
                 source_id, generation, size_bytes, mtime, session_time,
                 schema_fingerprint, status
             ) VALUES (?1, ?2, 4096, 'now', NULL, NULL, 'active')",
            params![source_id, i64::try_from(generation).unwrap()],
        )
        .unwrap();
}

async fn fixture_runtime(dir: &tempfile::TempDir, key_byte: u8) -> StoreRuntime {
    let database = dir.path().join("state.db");
    seed_registered_source(&database, "src_1", 1);
    StoreRuntime::start_with_key_provider(
        database,
        Arc::new(MemoryKeyProvider::fixed([key_byte; 32])),
    )
    .await
    .unwrap()
}

#[tokio::test]
async fn cursor_and_events_commit_together_and_retries_are_idempotent() {
    let dir = private_tempdir();
    let runtime = fixture_runtime(&dir, 17).await;
    let chunk = IngestionChunk::fixture("src_1", 1, 0, 128, 2);

    let first = runtime
        .writer()
        .commit_ingestion(chunk.clone())
        .await
        .unwrap();
    let second = runtime.writer().commit_ingestion(chunk).await.unwrap();

    assert_eq!(first.cursor_offset, 128);
    assert_eq!(first.inserted_events, 2);
    assert_eq!(second.cursor_offset, 128);
    assert_eq!(second.inserted_events, 0);
    assert_eq!(runtime.read().event_count().await.unwrap(), 2);
    assert_eq!(
        runtime
            .read()
            .cursor("src_1", 1)
            .await
            .unwrap()
            .unwrap()
            .offset,
        128
    );
}

#[tokio::test]
async fn stale_expected_cursor_rejects_the_whole_chunk() {
    let dir = private_tempdir();
    let runtime = fixture_runtime(&dir, 18).await;
    runtime
        .writer()
        .commit_ingestion(IngestionChunk::fixture("src_1", 1, 0, 64, 1))
        .await
        .unwrap();

    let mut stale = IngestionChunk::fixture("src_1", 1, 0, 128, 1);
    stale.evidence.push(EvidenceWrite {
        evidence_id: EvidenceId::for_test("ev_stale"),
        project_id: ProjectId::for_test("project_fixture"),
        owner: EvidenceOwner::Work(WorkId::for_test("work_fixture")),
        content_hash: "b3:evidence".into(),
        media_type: "text/plain".into(),
        privacy: PrivacyLabel::PrivateLocal,
        disclosure_class: DisclosureClass::ObservedState,
        redacted_excerpt: String::new(),
        expires_at: None,
        plaintext: Zeroizing::new(b"stale plaintext".to_vec()),
    });
    let error = runtime.writer().commit_ingestion(stale).await.unwrap_err();

    assert!(error.to_string().contains("cursor conflict"));
    assert_eq!(runtime.read().event_count().await.unwrap(), 1);
    assert!(!dir.path().join("evidence/ev_stale.agbx").exists());
}

#[tokio::test]
async fn same_next_cursor_requires_exact_parser_state_and_rows() {
    let dir = private_tempdir();
    let runtime = fixture_runtime(&dir, 19).await;
    let chunk = IngestionChunk::fixture("src_1", 1, 0, 128, 1);
    runtime
        .writer()
        .commit_ingestion(chunk.clone())
        .await
        .unwrap();

    let mut mismatched = chunk.clone();
    mismatched.next_cursor.parser_state = b"different-state".to_vec();
    let error = runtime
        .writer()
        .commit_ingestion(mismatched)
        .await
        .unwrap_err();

    assert!(matches!(error, StoreError::CursorConflict));
    assert_eq!(runtime.read().event_count().await.unwrap(), 1);
    assert_eq!(
        runtime
            .read()
            .cursor("src_1", 1)
            .await
            .unwrap()
            .unwrap()
            .parser_state,
        Vec::<u8>::new()
    );

    let mut mismatched = chunk;
    let original = &mismatched.events[0];
    let mut replacement = ActivityEventV1::fixture_message_draft();
    replacement.event_id = original.event_id().clone();
    replacement.semantic_key = original.semantic_key().clone();
    replacement.turn_id = Some("different-immutable-turn".into());
    mismatched.events[0] = ActivityEventV1::new(replacement).unwrap();
    let error = runtime
        .writer()
        .commit_ingestion(mismatched)
        .await
        .unwrap_err();
    assert!(matches!(error, StoreError::ImmutableConflict));
    assert_eq!(runtime.read().event_count().await.unwrap(), 1);
}

#[tokio::test]
async fn sqlite_failure_rolls_back_rows_and_cursor_after_encrypted_blob_persistence() {
    let dir = private_tempdir();
    let runtime = fixture_runtime(&dir, 20).await;
    let mut chunk = IngestionChunk::fixture("src_1", 1, 0, 128, 1);
    chunk.evidence.push(EvidenceWrite {
        evidence_id: EvidenceId::for_test("ev_atomic_failure"),
        project_id: ProjectId::for_test("project_fixture"),
        owner: EvidenceOwner::Work(WorkId::for_test("work_fixture")),
        content_hash: "b3:evidence".into(),
        media_type: "text/plain".into(),
        privacy: PrivacyLabel::PrivateLocal,
        disclosure_class: DisclosureClass::ObservedState,
        redacted_excerpt: "bounded preview".into(),
        expires_at: None,
        plaintext: Zeroizing::new(b"encrypted before transaction".to_vec()),
    });
    chunk.evidence_links.push(EvidenceLink {
        event_id: "evt_missing".into(),
        observation_id: "obs_missing".into(),
        evidence_id: "ev_atomic_failure".into(),
    });

    let error = runtime.writer().commit_ingestion(chunk).await.unwrap_err();

    assert!(matches!(error, StoreError::Sqlite(_)));
    assert_eq!(runtime.read().event_count().await.unwrap(), 0);
    assert!(runtime.read().cursor("src_1", 1).await.unwrap().is_none());
    let blob = std::fs::read(dir.path().join("evidence/ev_atomic_failure.agbx")).unwrap();
    assert!(blob.starts_with(b"AGBX\x01"));
    assert!(
        !blob
            .windows(b"encrypted before transaction".len())
            .any(|window| window == b"encrypted before transaction")
    );
}

#[tokio::test]
async fn project_mismatch_is_rejected_before_evidence_persistence() {
    let dir = private_tempdir();
    let runtime = fixture_runtime(&dir, 21).await;
    let mut chunk = IngestionChunk::fixture("src_1", 1, 0, 128, 0);
    chunk.evidence.push(EvidenceWrite {
        evidence_id: EvidenceId::for_test("ev_wrong_project"),
        project_id: ProjectId::for_test("project_other"),
        owner: EvidenceOwner::Work(WorkId::for_test("work_fixture")),
        content_hash: "b3:evidence".into(),
        media_type: "text/plain".into(),
        privacy: PrivacyLabel::PrivateLocal,
        disclosure_class: DisclosureClass::ObservedState,
        redacted_excerpt: String::new(),
        expires_at: None,
        plaintext: Zeroizing::new(b"must not be written".to_vec()),
    });

    let error = runtime.writer().commit_ingestion(chunk).await.unwrap_err();

    assert!(matches!(error, StoreError::ProjectMismatch));
    assert!(!dir.path().join("evidence/ev_wrong_project.agbx").exists());
    assert!(runtime.read().cursor("src_1", 1).await.unwrap().is_none());
}

#[test]
fn sensitive_debug_output_is_sanitized() {
    let mut chunk = IngestionChunk::fixture("src_1", 1, 0, 128, 0);
    chunk.expected_cursor.parser_state = b"PARSER_STATE_SECRET_9271".to_vec();
    chunk.evidence.push(EvidenceWrite {
        evidence_id: EvidenceId::for_test("ev_debug"),
        project_id: ProjectId::for_test("project_fixture"),
        owner: EvidenceOwner::Work(WorkId::for_test("work_fixture")),
        content_hash: "b3:evidence".into(),
        media_type: "text/plain".into(),
        privacy: PrivacyLabel::RestrictedLocal,
        disclosure_class: DisclosureClass::HumanIntent,
        redacted_excerpt: "EXCERPT_SECRET_9271".into(),
        expires_at: None,
        plaintext: Zeroizing::new(b"PLAINTEXT_SECRET_9271".to_vec()),
    });

    let debug = format!("{chunk:?}");
    assert!(!debug.contains("PARSER_STATE_SECRET_9271"));
    assert!(!debug.contains("EXCERPT_SECRET_9271"));
    assert!(!debug.contains("PLAINTEXT_SECRET_9271"));
}

#[test]
fn validates_cardinality_parser_state_and_project_scoped_content_ref_ids() {
    let too_many_events = IngestionChunk::fixture(
        "src_1",
        1,
        0,
        128,
        agbox_core::limits::MAX_EVENTS_PER_RECORD + 1,
    );
    assert!(matches!(
        too_many_events.validate(),
        Err(StoreError::InvalidBatch)
    ));

    let mut oversized_state = IngestionChunk::fixture("src_1", 1, 0, 128, 0);
    oversized_state.next_cursor.parser_state =
        vec![0; agbox_core::limits::MAX_DECODER_STATE_BYTES + 1];
    assert!(matches!(
        oversized_state.validate(),
        Err(StoreError::InvalidBatch)
    ));

    let content = ContentRef::bounded(
        "b3:content".into(),
        7,
        "text/plain",
        None,
        DisclosureClass::DerivedText,
        None,
    )
    .unwrap();
    let project = ProjectId::for_test("project_fixture");
    let other_project = ProjectId::for_test("project_other");
    let stable = stable_content_ref_id(&project, &content).unwrap();
    assert_ne!(
        stable,
        stable_content_ref_id(&other_project, &content).unwrap()
    );

    let mut chunk = IngestionChunk::fixture("src_1", 1, 0, 128, 0);
    chunk.content_refs.push(ContentRefWrite {
        content_ref_id: stable,
        project_id: project,
        content,
        privacy: PrivacyLabel::DerivedLocal,
    });
    assert!(chunk.validate().is_ok());
    chunk.content_refs[0].content_ref_id = "cref_wrong".into();
    assert!(matches!(
        chunk.validate(),
        Err(StoreError::InvalidContentRefId)
    ));
}

#[tokio::test]
async fn concurrent_typed_reads_return_all_pool_checkouts() {
    let dir = private_tempdir();
    let runtime = fixture_runtime(&dir, 22).await;
    runtime
        .writer()
        .commit_ingestion(IngestionChunk::fixture("src_1", 1, 0, 128, 2))
        .await
        .unwrap();

    let mut tasks = Vec::new();
    for _ in 0..32 {
        let read = runtime.read().clone();
        tasks.push(tokio::spawn(async move { read.event_count().await }));
    }
    for task in tasks {
        assert_eq!(task.await.unwrap().unwrap(), 2);
    }
    assert_eq!(runtime.read().event_count().await.unwrap(), 2);
}

#[tokio::test]
async fn runtime_never_opens_the_reserved_legacy_database() {
    let dir = private_tempdir();
    let legacy = dir.path().join("agbox.db");
    std::fs::write(&legacy, b"legacy sentinel").unwrap();

    let error = StoreRuntime::start_with_key_provider(
        &legacy,
        Arc::new(MemoryKeyProvider::fixed([23_u8; 32])),
    )
    .await
    .unwrap_err();

    assert!(matches!(error, StoreError::LegacyDatabaseReserved));
    assert_eq!(std::fs::read(legacy).unwrap(), b"legacy sentinel");
}

#[tokio::test]
async fn explicit_shutdown_drains_and_stops_cloned_writer_handles() {
    let dir = private_tempdir();
    let runtime = fixture_runtime(&dir, 24).await;
    let writer = runtime.writer().clone();
    writer
        .commit_ingestion(IngestionChunk::fixture("src_1", 1, 0, 128, 1))
        .await
        .unwrap();

    runtime.shutdown().await.unwrap();

    let error = writer
        .commit_ingestion(IngestionChunk::fixture("src_1", 1, 128, 256, 1))
        .await
        .unwrap_err();
    assert!(matches!(error, StoreError::WriterStopped));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancelled_shutdown_before_enqueue_falls_back_to_drain_and_join() {
    let dir = private_tempdir();
    let database = dir.path().join("state.db");
    let runtime = fixture_runtime(&dir, 25).await;
    let writer = runtime.writer().clone();
    let lock = rusqlite::Connection::open(&database).unwrap();
    lock.execute_batch("BEGIN IMMEDIATE").unwrap();

    let mut commits = Vec::new();
    for _ in 0..33 {
        let queued_writer = writer.clone();
        commits.push(tokio::spawn(async move {
            queued_writer
                .commit_ingestion(IngestionChunk::fixture("src_1", 1, 0, 128, 1))
                .await
        }));
    }
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        while writer.available_capacity_for_test() != 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();

    let (release_send, release_receive) = std::sync::mpsc::channel();
    let release_thread = std::thread::spawn(move || {
        release_receive.recv().unwrap();
        lock.execute_batch("COMMIT").unwrap();
    });
    let (entered_send, entered_receive) = tokio::sync::oneshot::channel();
    let shutdown = tokio::spawn(async move {
        let _ = entered_send.send(());
        runtime.shutdown().await
    });
    entered_receive.await.unwrap();
    tokio::task::yield_now().await;
    assert_eq!(writer.available_capacity_for_test(), 0);

    shutdown.abort();
    release_send.send(()).unwrap();
    assert!(shutdown.await.unwrap_err().is_cancelled());
    release_thread.join().unwrap();
    for commit in commits {
        commit.await.unwrap().unwrap();
    }
    let error = writer
        .commit_ingestion(IngestionChunk::fixture("src_1", 1, 128, 256, 1))
        .await
        .unwrap_err();
    assert!(matches!(error, StoreError::WriterStopped));
}
