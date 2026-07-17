#![allow(clippy::unwrap_used)]

use std::{os::unix::fs::PermissionsExt, sync::Arc};

use std::path::Path;

use agbox_core::{
    ActivityEventV1, ContentRef, DisclosureClass, EventId, EvidenceId, PrivacyLabel, ProjectId,
    WorkId,
};
use agbox_store::{
    ContentRefWrite, EvidenceLink, EvidenceOwner, EvidenceWrite, IngestionChunk, MemoryKeyProvider,
    SchemaFingerprintUpdate, Store, StoreError, StoreRuntime, stable_content_ref_id,
};
use rusqlite::params;
use zeroize::Zeroizing;

fn private_tempdir() -> tempfile::TempDir {
    let directory = tempfile::tempdir().unwrap();
    std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    directory
}

fn seed_registered_source(database: &Path, source_id: &str, generation: u64) {
    seed_source_for_project(database, source_id, generation, "project_fixture");
}

fn seed_source_for_project(database: &Path, source_id: &str, generation: u64, project_id: &str) {
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
             ) VALUES (?1, ?2, X'00', 'now', 'now')
             ON CONFLICT(project_id) DO NOTHING",
            params![project_id, format!("repository_{project_id}")],
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO sources(
                 source_id, project_id, provider, root_class, encrypted_path,
                 file_identity, created_at, updated_at
             ) VALUES (?1, ?2, 'codex', 'active', X'00',
                 'fixture-identity', 'now', 'now')",
            params![source_id, project_id],
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

fn evidence_chunk(evidence_id: &str) -> IngestionChunk {
    let mut chunk = IngestionChunk::fixture("src_1", 1, 0, 128, 1);
    let owner = chunk.events[0].event_id().clone();
    chunk.evidence.push(EvidenceWrite {
        evidence_id: EvidenceId::for_test(evidence_id),
        project_id: ProjectId::for_test("project_fixture"),
        owner: EvidenceOwner::Event(owner),
        content_hash: format!("b3:{evidence_id}"),
        media_type: "text/plain".into(),
        privacy: PrivacyLabel::PrivateLocal,
        disclosure_class: DisclosureClass::ObservedState,
        redacted_excerpt: "bounded preview".into(),
        expires_at: None,
        plaintext: Zeroizing::new(format!("plaintext-{evidence_id}").into_bytes()),
    });
    chunk
}

fn seed_cross_project_targets(database: &Path) {
    seed_source_for_project(database, "src_other", 1, "project_other");
    let connection = rusqlite::Connection::open(database).unwrap();
    connection
        .pragma_update(None, "foreign_keys", "ON")
        .unwrap();
    connection
        .execute(
            "INSERT INTO work_items(work_id, project_id, status, created_at, updated_at)
             VALUES ('work_other', 'project_other', 'active', 'now', 'now')",
            [],
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO activity_events(
                 event_id, semantic_key, schema_version, occurred_at, observed_at,
                 project_id, session_id, turn_id, actor, correlation_id,
                 causation_id, source_json, payload_json, privacy
             ) VALUES (
                 'evt_other', 'sem_other', 1, 'now', 'now', 'project_other',
                 'session_other', NULL, 'agent', NULL, NULL, '{}', '{}', 'derived_local'
             )",
            [],
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO source_observations(
                 observation_id, source_id, generation, byte_start, byte_end,
                 record_hash, native_record_type, decode_status,
                 schema_fingerprint, observed_at
             ) VALUES (
                 'obs_other', 'src_other', 1, 0, 1, 'hash-other', 'message',
                 'known', 'fingerprint-other', 'now'
             )",
            [],
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO evidence_objects(
                 evidence_id, project_id, owner_kind, owner_id, content_hash,
                 media_type, privacy, byte_length, redacted_excerpt,
                 disclosure_class, blob_state, created_at, expires_at, retired_at
             ) VALUES (
                 'ev_other', 'project_other', 'work', 'work_other', 'hash-other',
                 'text/plain', 'private_local', 1, '', 'observed_state',
                 'available', 'now', NULL, NULL
             )",
            [],
        )
        .unwrap();
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
    let owner_event = stale.events[0].event_id().clone();
    stale.evidence.push(EvidenceWrite {
        evidence_id: EvidenceId::for_test("ev_stale"),
        project_id: ProjectId::for_test("project_fixture"),
        owner: EvidenceOwner::Event(owner_event),
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
async fn same_next_cursor_rejects_any_whole_chunk_identity_change() {
    let dir = private_tempdir();
    let runtime = fixture_runtime(&dir, 26).await;
    let original = IngestionChunk::fixture("src_1", 1, 0, 128, 2);
    runtime
        .writer()
        .commit_ingestion(original.clone())
        .await
        .unwrap();

    let mut empty = original.clone();
    empty.observations.clear();
    empty.events.clear();
    let mut subset = original.clone();
    subset.events.pop();
    let superset = IngestionChunk::fixture("src_1", 1, 0, 128, 3);
    let mut changed_expected_offset = original.clone();
    changed_expected_offset.expected_cursor.offset = 1;
    let mut changed_expected_state = original;
    changed_expected_state.expected_cursor.parser_state = b"different-expected-state".to_vec();

    for (label, changed) in [
        ("empty", empty),
        ("subset", subset),
        ("superset", superset),
        ("expected offset", changed_expected_offset),
        ("expected parser state", changed_expected_state),
    ] {
        let error = runtime
            .writer()
            .commit_ingestion(changed)
            .await
            .unwrap_err();
        assert!(
            matches!(error, StoreError::ImmutableConflict),
            "{label}: {error:?}"
        );
    }
    assert_eq!(runtime.read().event_count().await.unwrap(), 2);
}

#[tokio::test]
async fn shared_fingerprint_updates_do_not_break_an_earlier_source_retry() {
    let dir = private_tempdir();
    let database = dir.path().join("state.db");
    seed_registered_source(&database, "src_1", 1);
    seed_registered_source(&database, "src_2", 1);
    let runtime = StoreRuntime::start_with_key_provider(
        &database,
        Arc::new(MemoryKeyProvider::fixed([27_u8; 32])),
    )
    .await
    .unwrap();
    let mut source_a = IngestionChunk::fixture("src_1", 1, 0, 128, 1);
    source_a.fingerprints.push(SchemaFingerprintUpdate {
        provider: "codex".into(),
        format: "jsonl".into(),
        fingerprint: "shared-fingerprint".into(),
        observed_at: time::OffsetDateTime::UNIX_EPOCH,
    });
    let mut source_b = IngestionChunk::fixture("src_2", 1, 0, 128, 1);
    source_b.fingerprints.push(SchemaFingerprintUpdate {
        provider: "codex".into(),
        format: "jsonl".into(),
        fingerprint: "shared-fingerprint".into(),
        observed_at: time::OffsetDateTime::UNIX_EPOCH + time::Duration::seconds(1),
    });

    runtime
        .writer()
        .commit_ingestion(source_a.clone())
        .await
        .unwrap();
    runtime.writer().commit_ingestion(source_b).await.unwrap();
    let replay = runtime.writer().commit_ingestion(source_a).await.unwrap();
    assert_eq!(replay.inserted_events, 0);

    let connection = rusqlite::Connection::open(&database).unwrap();
    let count: i64 = connection
        .query_row(
            "SELECT count FROM schema_fingerprints
             WHERE provider = 'codex' AND format = 'jsonl'
               AND fingerprint = 'shared-fingerprint'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(count, 2);
}

#[tokio::test]
async fn sqlite_failure_rolls_back_rows_and_cursor_after_encrypted_blob_persistence() {
    let dir = private_tempdir();
    let runtime = fixture_runtime(&dir, 20).await;
    let mut chunk = IngestionChunk::fixture("src_1", 1, 0, 128, 1);
    let owner_event = chunk.events[0].event_id().clone();
    chunk.evidence.push(EvidenceWrite {
        evidence_id: EvidenceId::for_test("ev_atomic_failure"),
        project_id: ProjectId::for_test("project_fixture"),
        owner: EvidenceOwner::Event(owner_event),
        content_hash: "b3:evidence".into(),
        media_type: "text/plain".into(),
        privacy: PrivacyLabel::PrivateLocal,
        disclosure_class: DisclosureClass::ObservedState,
        redacted_excerpt: "bounded preview".into(),
        expires_at: None,
        plaintext: Zeroizing::new(b"encrypted before transaction".to_vec()),
    });
    let connection = rusqlite::Connection::open(dir.path().join("state.db")).unwrap();
    connection
        .execute(
            "INSERT INTO source_observations(
                 observation_id, source_id, generation, byte_start, byte_end,
                 record_hash, native_record_type, decode_status,
                 schema_fingerprint, observed_at
             ) VALUES (?1, 'src_1', 1, 0, 128, 'different-hash', 'message',
                 'known', 'fixture-fingerprint', '1970-01-01T00:00:00Z')",
            [chunk.observations[0].observation_id()],
        )
        .unwrap();
    drop(connection);

    let error = runtime.writer().commit_ingestion(chunk).await.unwrap_err();

    assert!(matches!(error, StoreError::ImmutableConflict));
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

#[tokio::test]
async fn evidence_owners_and_links_cannot_cross_project_or_source_boundaries() {
    let dir = private_tempdir();
    let database = dir.path().join("state.db");
    seed_registered_source(&database, "src_1", 1);
    seed_cross_project_targets(&database);
    let runtime = StoreRuntime::start_with_key_provider(
        &database,
        Arc::new(MemoryKeyProvider::fixed([28_u8; 32])),
    )
    .await
    .unwrap();

    let mut owner_other = evidence_chunk("ev_owner_other");
    owner_other.evidence[0].owner = EvidenceOwner::Event(EventId::parse_wire("evt_other").unwrap());
    let mut owner_missing = evidence_chunk("ev_owner_missing");
    owner_missing.evidence[0].owner =
        EvidenceOwner::Event(EventId::parse_wire("evt_missing").unwrap());
    let mut work_other = evidence_chunk("ev_work_other");
    work_other.evidence[0].owner = EvidenceOwner::Work(WorkId::for_test("work_other"));
    let evidence_id_collision = evidence_chunk("ev_other");

    let mut event_link = evidence_chunk("ev_event_link");
    event_link.evidence_links.push(EvidenceLink {
        event_id: "evt_other".into(),
        observation_id: event_link.observations[0].observation_id().into(),
        evidence_id: "ev_event_link".into(),
    });
    let mut evidence_link = evidence_chunk("ev_evidence_link");
    evidence_link.evidence_links.push(EvidenceLink {
        event_id: evidence_link.events[0].event_id().as_str().into(),
        observation_id: evidence_link.observations[0].observation_id().into(),
        evidence_id: "ev_other".into(),
    });
    let mut observation_link = evidence_chunk("ev_observation_link");
    observation_link.evidence_links.push(EvidenceLink {
        event_id: observation_link.events[0].event_id().as_str().into(),
        observation_id: "obs_other".into(),
        evidence_id: "ev_observation_link".into(),
    });

    for (evidence_id, chunk) in [
        ("ev_owner_other", owner_other),
        ("ev_owner_missing", owner_missing),
        ("ev_work_other", work_other),
        ("ev_other", evidence_id_collision),
        ("ev_event_link", event_link),
        ("ev_evidence_link", evidence_link),
        ("ev_observation_link", observation_link),
    ] {
        let error = runtime.writer().commit_ingestion(chunk).await.unwrap_err();
        assert!(
            error.to_string().contains("reference") || matches!(error, StoreError::ProjectMismatch),
            "{evidence_id}: {error:?}"
        );
        assert!(
            !dir.path()
                .join(format!("evidence/{evidence_id}.agbx"))
                .exists()
        );
    }
    assert!(runtime.read().cursor("src_1", 1).await.unwrap().is_none());
}

#[tokio::test]
async fn evidence_owner_and_link_targets_may_all_be_created_in_the_same_chunk() {
    let dir = private_tempdir();
    let runtime = fixture_runtime(&dir, 29).await;
    let mut chunk = evidence_chunk("ev_same_chunk");
    chunk.evidence_links.push(EvidenceLink {
        event_id: chunk.events[0].event_id().as_str().into(),
        observation_id: chunk.observations[0].observation_id().into(),
        evidence_id: "ev_same_chunk".into(),
    });

    let receipt = runtime.writer().commit_ingestion(chunk).await.unwrap();

    assert_eq!(receipt.inserted_events, 1);
    assert_eq!(receipt.cursor_offset, 128);
    assert!(dir.path().join("evidence/ev_same_chunk.agbx").is_file());
}

#[tokio::test]
async fn evidence_plaintext_is_verified_by_the_vault_not_the_database_digest() {
    let dir = private_tempdir();
    let runtime = fixture_runtime(&dir, 30).await;
    let chunk = evidence_chunk("ev_plaintext_retry");
    runtime
        .writer()
        .commit_ingestion(chunk.clone())
        .await
        .unwrap();
    runtime
        .writer()
        .commit_ingestion(chunk.clone())
        .await
        .unwrap();

    let mut changed_plaintext = chunk;
    let last = changed_plaintext.evidence[0].plaintext.len() - 1;
    changed_plaintext.evidence[0].plaintext[last] ^= 1;
    let error = runtime
        .writer()
        .commit_ingestion(changed_plaintext)
        .await
        .unwrap_err();

    assert!(matches!(error, StoreError::Evidence(_)));
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

#[test]
fn semantic_bytes_count_owner_labels_expiry_and_content_ref_privacy() {
    let base = evidence_chunk("ev_semantic_delta");
    let base_bytes = base.measured_semantic_bytes().unwrap();
    let owner_id_len = base.events[0].event_id().as_str().len();

    let mut work_owner = base.clone();
    work_owner.evidence[0].owner = EvidenceOwner::Work(WorkId::for_test(&"w".repeat(owner_id_len)));
    assert_eq!(
        base_bytes,
        work_owner.measured_semantic_bytes().unwrap() + 1,
        "event owner_kind must count one more byte than work"
    );

    let mut restricted = base.clone();
    restricted.evidence[0].privacy = PrivacyLabel::RestrictedLocal;
    assert_eq!(
        restricted.measured_semantic_bytes().unwrap(),
        base_bytes + 3,
        "restricted_local versus private_local"
    );

    let mut human_intent = base.clone();
    human_intent.evidence[0].disclosure_class = DisclosureClass::HumanIntent;
    assert_eq!(
        human_intent.measured_semantic_bytes().unwrap() + 2,
        base_bytes,
        "human_intent versus observed_state"
    );

    let mut expiring = base;
    expiring.evidence[0].expires_at = Some(time::OffsetDateTime::UNIX_EPOCH);
    assert_eq!(
        expiring.measured_semantic_bytes().unwrap(),
        base_bytes + "1970-01-01T00:00:00Z".len(),
        "expiry timestamp bytes"
    );

    let content = ContentRef::bounded(
        "b3:privacy-delta".into(),
        7,
        "text/plain",
        None,
        DisclosureClass::DerivedText,
        None,
    )
    .unwrap();
    let project = ProjectId::for_test("project_fixture");
    let content_ref_id = stable_content_ref_id(&project, &content).unwrap();
    let mut private_ref = IngestionChunk::fixture("src_1", 1, 0, 128, 0);
    private_ref.content_refs.push(ContentRefWrite {
        content_ref_id,
        project_id: project,
        content,
        privacy: PrivacyLabel::PrivateLocal,
    });
    let mut restricted_ref = private_ref.clone();
    restricted_ref.content_refs[0].privacy = PrivacyLabel::RestrictedLocal;
    assert_eq!(
        restricted_ref.measured_semantic_bytes().unwrap(),
        private_ref.measured_semantic_bytes().unwrap() + 3,
        "content-ref privacy label bytes"
    );
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
