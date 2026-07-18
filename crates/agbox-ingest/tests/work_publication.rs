#![allow(clippy::unwrap_used)]

use agbox_core::{ContractId, EventId, ProjectId, Provider, SessionId, WorkId};
use agbox_ingest::{
    IngestionCoordinator, WorkPublicationRequest, graph_write_batch, test_support::FixtureRuntime,
    work_write_batch,
};
use agbox_store::{WorkCandidateQuery, WorkContractRow, WorkWriteBatch};
use agbox_workgraph::{
    CorrelationDecision, CorrelationInput, Correlator, GraphMutation, ProvisionalContractBuilder,
    ReducedFact, WorkCandidate,
};
use rusqlite::{Connection, params};
use time::{OffsetDateTime, format_description::well_known::Rfc3339, macros::datetime};

fn work_id(value: &str) -> WorkId {
    WorkId::parse_wire(value).unwrap()
}

fn contract_id(value: &str) -> ContractId {
    ContractId::parse_wire(value).unwrap()
}

#[test]
fn tied_candidates_translate_to_split_provenance_edges_without_execution_semantics() {
    let project_id = ProjectId::for_test("project-tie");
    let evidence = EventId::parse_wire("evt-tie").unwrap();
    let input = CorrelationInput::new(project_id.clone(), Provider::Codex, vec![evidence.clone()])
        .repository("repo")
        .branch_hash("main")
        .artifact_hashes(["b3:shared"])
        .candidate(
            WorkCandidate::new(work_id("work-tie-a"), project_id.clone())
                .repository("repo")
                .branch_hash("main")
                .artifact_hashes(["b3:shared"]),
        )
        .candidate(
            WorkCandidate::new(work_id("work-tie-b"), project_id.clone())
                .repository("repo")
                .branch_hash("main")
                .artifact_hashes(["b3:shared"]),
        );
    let correlation = Correlator.correlate(&input);
    assert!(matches!(correlation.decision, CorrelationDecision::Create));
    assert_eq!(correlation.proposals.len(), 2);
    let new_work = work_id("work-tie-new");
    let mutation = GraphMutation {
        facts: vec![ReducedFact::HumanObjective {
            project_id,
            content_hash: "b3:tie-objective".into(),
            redacted_text: Some("Split ambiguous work".into()),
            observed_at: datetime!(2026-07-19 12:00 UTC),
            evidence: evidence.clone(),
        }],
        expected_event_seq: 0,
        through_event_seq: Some(1),
        through_event_id: Some(evidence),
    };
    let contract = ProvisionalContractBuilder::new("deterministic-v1")
        .for_work(new_work.clone())
        .build(None, &mutation.facts)
        .unwrap();
    let batch = work_write_batch(&mutation, new_work, &correlation, &contract).unwrap();
    assert_eq!(batch.edges.len(), 2);
    assert!(batch.edges.iter().all(|edge| edge.kind == "continues"));
}

#[tokio::test]
async fn provisional_revision_fts_and_visibility_commit_atomically_and_replay_exactly_once() {
    let fixture = FixtureRuntime::codex_records(1).await;
    fixture.drain().await.unwrap();
    let stored = fixture
        .read_store()
        .events_after(0, 1, 1024 * 1024)
        .await
        .unwrap();
    let event = &stored[0];
    let project_id = event.event.project_id().clone();
    let batch = WorkWriteBatch {
        visibility_name: "work-visibility-v1".into(),
        expected_event_seq: 0,
        next_event_seq: event.event_seq,
        next_event_id: event.event.event_id().clone(),
        project_id: project_id.clone(),
        work_id: work_id("work-publication"),
        status: "active".into(),
        observed_at: datetime!(2026-07-19 12:00 UTC),
        evidence_event_ids: vec![event.event.event_id().clone()],
        artifact_ids: Vec::new(),
        edges: Vec::new(),
        contract: WorkContractRow {
            contract_id: contract_id("contract-publication"),
            revision: 1,
            contract_json: format!(
                r#"{{"work_id":"work-publication","project_id":"{}","revision":1,"objective":"Ship parser","summary":"Parser work","completed_steps":[],"next_actions":["run tests"],"blockers":[],"artifacts":["src/parser.rs"],"verification":[],"material_content_hash":"b3:one"}}"#,
                project_id.as_str()
            ),
            extractor_version: "deterministic-v1".into(),
            objective: Some("Ship parser".into()),
            summary: "Parser work".into(),
            completed_steps: Vec::new(),
            next_actions: vec!["run tests".into()],
            blockers: Vec::new(),
            artifacts: vec!["src/parser.rs".into()],
            verification: Vec::new(),
        },
    };

    let first = fixture.writer().apply_work(batch.clone()).await.unwrap();
    let replay = fixture.writer().apply_work(batch).await.unwrap();
    assert!(!first.replayed);
    assert!(replay.replayed);

    let connection = Connection::open(fixture.database_path()).unwrap();
    let counts: (i64, i64, i64, i64) = connection
        .query_row(
            "SELECT
                (SELECT count(*) FROM work_items WHERE work_id = ?1),
                (SELECT count(*) FROM work_contract_revisions WHERE work_id = ?1),
                (SELECT count(*) FROM work_search WHERE work_id = ?1),
                (SELECT through_event_seq FROM reducer_watermarks WHERE reducer_name = ?2)",
            params!["work-publication", "work-visibility-v1"],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .unwrap();
    assert_eq!(counts, (1, 1, 1, i64::try_from(event.event_seq).unwrap()));
    let hit: i64 = connection
        .query_row(
            "SELECT count(*) FROM work_search
             WHERE work_search MATCH 'parser' AND project_id = ?1",
            [event.event.project_id().as_str()],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(hit, 1);
}

#[tokio::test]
async fn request_only_publication_preserves_observation_time_for_recency() {
    let fixture = FixtureRuntime::codex_records(1).await;
    fixture.drain().await.unwrap();
    let event = &fixture
        .read_store()
        .events_after(0, 1, 1024 * 1024)
        .await
        .unwrap()[0];
    let observed_at = event.event.observed_at();
    let mutation = GraphMutation {
        facts: vec![ReducedFact::ActionRequested {
            project_id: event.event.project_id().clone(),
            session_id: event.event.session_id().clone(),
            native_action_id: "request-only".into(),
            tool_name: "shell".into(),
            input_hash: "b3:request-only".into(),
            redacted_input: Some("cargo test".into()),
            observed_at,
            evidence: event.event.event_id().clone(),
        }],
        expected_event_seq: 0,
        through_event_seq: Some(event.event_seq),
        through_event_id: Some(event.event.event_id().clone()),
    };
    fixture
        .writer()
        .apply_graph(graph_write_batch(mutation.clone()).unwrap())
        .await
        .unwrap();
    let coordinator =
        IngestionCoordinator::new(fixture.read_store().clone(), fixture.writer().clone(), 1);
    let report = coordinator
        .publish_work(mutation, WorkPublicationRequest::new(Provider::Codex))
        .await
        .unwrap();

    let connection = Connection::open(fixture.database_path()).unwrap();
    let updated_at: String = connection
        .query_row(
            "SELECT updated_at FROM work_items WHERE work_id = ?1",
            [report.work_id.as_str()],
            |row| row.get(0),
        )
        .unwrap();
    let persisted = OffsetDateTime::parse(&updated_at, &Rfc3339).unwrap();
    assert_eq!(persisted, observed_at);

    let candidates = fixture
        .writer()
        .load_work_candidates(WorkCandidateQuery {
            project_id: event.event.project_id().clone(),
            explicit_work_id: None,
            continuation_work_id: None,
            artifact_hashes: Vec::new(),
            command_hashes: vec!["b3:request-only".into()],
            observed_at: observed_at + time::Duration::minutes(2),
        })
        .await
        .unwrap();
    assert_eq!(candidates.candidates.len(), 1);
    assert_eq!(candidates.candidates[0].work_id, report.work_id);
    assert_eq!(candidates.candidates[0].minutes_since_activity, 2);
}

#[tokio::test]
async fn invalid_evidence_leaves_no_partial_work_rows_or_visibility_watermark() {
    let fixture = FixtureRuntime::codex_records(1).await;
    fixture.drain().await.unwrap();
    let stored = fixture
        .read_store()
        .events_after(0, 1, 1024 * 1024)
        .await
        .unwrap();
    let event = &stored[0];
    let batch = WorkWriteBatch {
        visibility_name: "failed-work-visibility-v1".into(),
        expected_event_seq: 0,
        next_event_seq: event.event_seq,
        next_event_id: event.event.event_id().clone(),
        project_id: ProjectId::for_test("different-project"),
        work_id: work_id("work-must-rollback"),
        status: "active".into(),
        observed_at: datetime!(2026-07-19 12:00 UTC),
        evidence_event_ids: vec![event.event.event_id().clone()],
        artifact_ids: Vec::new(),
        edges: Vec::new(),
        contract: WorkContractRow {
            contract_id: contract_id("contract-must-rollback"),
            revision: 1,
            contract_json: r#"{"work_id":"work-must-rollback","project_id":"different-project","revision":1,"material_content_hash":"b3:rollback"}"#.into(),
            extractor_version: "deterministic-v1".into(),
            objective: None,
            summary: String::new(),
            completed_steps: Vec::new(),
            next_actions: Vec::new(),
            blockers: Vec::new(),
            artifacts: Vec::new(),
            verification: Vec::new(),
        },
    };

    fixture.writer().apply_work(batch).await.unwrap_err();

    let connection = Connection::open(fixture.database_path()).unwrap();
    let counts: (i64, i64, i64) = connection
        .query_row(
            "SELECT
                (SELECT count(*) FROM work_items WHERE work_id = 'work-must-rollback'),
                (SELECT count(*) FROM work_contract_revisions
                    WHERE work_id = 'work-must-rollback'),
                (SELECT count(*) FROM reducer_watermarks
                    WHERE reducer_name = 'failed-work-visibility-v1')",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(counts, (0, 0, 0));
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn same_project_artifact_continuity_crosses_agents_and_writes_next_revision() {
    let fixture = FixtureRuntime::codex_records(2).await;
    fixture.drain().await.unwrap();
    let events = fixture
        .read_store()
        .events_after(0, 2, 1024 * 1024)
        .await
        .unwrap();
    let repository_hash: String = Connection::open(fixture.database_path())
        .unwrap()
        .query_row(
            "SELECT repository_identity FROM projects LIMIT 1",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let coordinator =
        IngestionCoordinator::new(fixture.read_store().clone(), fixture.writer().clone(), 1);
    let first = &events[0];
    let first_mutation = GraphMutation {
        facts: vec![
            ReducedFact::SessionContext {
                project_id: first.event.project_id().clone(),
                session_id: first.event.session_id().clone(),
                provider: Provider::Codex,
                branch_hash: Some("b3:main".into()),
                observed_at: first.event.observed_at(),
                evidence: first.event.event_id().clone(),
            },
            ReducedFact::Artifact {
                project_id: first.event.project_id().clone(),
                session_id: first.event.session_id().clone(),
                path_hash: "b3:shared".into(),
                project_relative_path: Some("src/lib.rs".into()),
                operation: "update".into(),
                content_hash: Some("b3:first".into()),
                observed_at: first.event.observed_at(),
                evidence: first.event.event_id().clone(),
            },
            ReducedFact::HumanObjective {
                project_id: first.event.project_id().clone(),
                content_hash: "b3:objective".into(),
                redacted_text: Some("Continue shared parser work".into()),
                observed_at: first.event.observed_at(),
                evidence: first.event.event_id().clone(),
            },
        ],
        expected_event_seq: 0,
        through_event_seq: Some(first.event_seq),
        through_event_id: Some(first.event.event_id().clone()),
    };
    fixture
        .writer()
        .apply_graph(graph_write_batch(first_mutation.clone()).unwrap())
        .await
        .unwrap();
    let mut first_request = WorkPublicationRequest::new(Provider::Codex);
    first_request.repository_hash = Some(repository_hash.clone());
    first_request.branch_hash = Some("b3:main".into());
    let first_report = coordinator
        .publish_work(first_mutation.clone(), first_request.clone())
        .await
        .unwrap();
    assert_eq!(first_report.revision, 1);
    let replay_report = coordinator
        .publish_work(first_mutation, first_request)
        .await
        .unwrap();
    assert!(replay_report.receipt.replayed);
    assert!(!replay_report.receipt.revision_inserted);
    assert_eq!(replay_report.work_id, first_report.work_id);
    assert_eq!(replay_report.revision, 1);

    let second = &events[1];
    let second_session = SessionId::parse_wire("claude-distinct-session").unwrap();
    let second_mutation = GraphMutation {
        facts: vec![
            ReducedFact::SessionContext {
                project_id: second.event.project_id().clone(),
                session_id: second_session.clone(),
                provider: Provider::Claude,
                branch_hash: Some("b3:main".into()),
                observed_at: second.event.observed_at(),
                evidence: second.event.event_id().clone(),
            },
            ReducedFact::Artifact {
                project_id: second.event.project_id().clone(),
                session_id: second_session,
                path_hash: "b3:shared".into(),
                project_relative_path: Some("src/lib.rs".into()),
                operation: "update".into(),
                content_hash: Some("b3:second".into()),
                observed_at: second.event.observed_at(),
                evidence: second.event.event_id().clone(),
            },
        ],
        expected_event_seq: first.event_seq,
        through_event_seq: Some(second.event_seq),
        through_event_id: Some(second.event.event_id().clone()),
    };
    fixture
        .writer()
        .apply_graph(graph_write_batch(second_mutation.clone()).unwrap())
        .await
        .unwrap();
    let placeholder_work_id: String = Connection::open(fixture.database_path())
        .unwrap()
        .query_row(
            "SELECT artifacts.work_id
             FROM artifacts
             WHERE artifacts.path_hash = 'b3:shared'
               AND NOT EXISTS(
                   SELECT 1 FROM work_contract_revisions
                   WHERE work_contract_revisions.work_id = artifacts.work_id
               )
             ORDER BY artifacts.observed_at DESC
             LIMIT 1",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_ne!(placeholder_work_id, first_report.work_id.as_str());
    let explicit_placeholder = fixture
        .writer()
        .load_work_candidates(WorkCandidateQuery {
            project_id: second.event.project_id().clone(),
            explicit_work_id: Some(WorkId::parse_wire(&placeholder_work_id).unwrap()),
            continuation_work_id: None,
            artifact_hashes: Vec::new(),
            command_hashes: Vec::new(),
            observed_at: second.event.observed_at(),
        })
        .await
        .unwrap();
    assert_eq!(
        explicit_placeholder.candidates[0].work_id.as_str(),
        placeholder_work_id
    );
    let candidates = fixture
        .writer()
        .load_work_candidates(WorkCandidateQuery {
            project_id: second.event.project_id().clone(),
            explicit_work_id: None,
            continuation_work_id: None,
            artifact_hashes: vec!["b3:shared".into()],
            command_hashes: Vec::new(),
            observed_at: second.event.observed_at(),
        })
        .await
        .unwrap();
    assert_eq!(candidates.candidates.len(), 1);
    assert_eq!(candidates.candidates[0].work_id, first_report.work_id);
    assert!(
        candidates
            .candidates
            .iter()
            .all(|candidate| candidate.work_id.as_str() != placeholder_work_id)
    );
    let mut second_request = WorkPublicationRequest::new(Provider::Claude);
    second_request.repository_hash = Some(repository_hash);
    second_request.branch_hash = Some("b3:main".into());
    let second_report = coordinator
        .publish_work(second_mutation, second_request)
        .await
        .unwrap();

    assert_eq!(second_report.work_id, first_report.work_id);
    assert_eq!(second_report.revision, 2);
    assert!(matches!(
        second_report.correlation.decision,
        CorrelationDecision::Continue { .. }
    ));
    let connection = Connection::open(fixture.database_path()).unwrap();
    let revision_count: i64 = connection
        .query_row(
            "SELECT count(*) FROM work_contract_revisions WHERE work_id = ?1",
            [first_report.work_id.as_str()],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(revision_count, 2);
}

#[tokio::test]
async fn semantic_similarity_without_evidence_backed_overlap_creates_new_work() {
    let fixture = FixtureRuntime::codex_records(2).await;
    fixture.drain().await.unwrap();
    let events = fixture
        .read_store()
        .events_after(0, 2, 1024 * 1024)
        .await
        .unwrap();
    let coordinator =
        IngestionCoordinator::new(fixture.read_store().clone(), fixture.writer().clone(), 1);
    let first = &events[0];
    let first_mutation = GraphMutation {
        facts: vec![ReducedFact::HumanObjective {
            project_id: first.event.project_id().clone(),
            content_hash: "b3:first".into(),
            redacted_text: Some("First objective".into()),
            observed_at: first.event.observed_at(),
            evidence: first.event.event_id().clone(),
        }],
        expected_event_seq: 0,
        through_event_seq: Some(first.event_seq),
        through_event_id: Some(first.event.event_id().clone()),
    };
    fixture
        .writer()
        .apply_graph(graph_write_batch(first_mutation.clone()).unwrap())
        .await
        .unwrap();
    let first_report = coordinator
        .publish_work(first_mutation, WorkPublicationRequest::new(Provider::Codex))
        .await
        .unwrap();

    let second = &events[1];
    let second_mutation = GraphMutation {
        facts: vec![ReducedFact::HumanObjective {
            project_id: second.event.project_id().clone(),
            content_hash: "b3:second".into(),
            redacted_text: Some("Second objective".into()),
            observed_at: second.event.observed_at(),
            evidence: second.event.event_id().clone(),
        }],
        expected_event_seq: first.event_seq,
        through_event_seq: Some(second.event_seq),
        through_event_id: Some(second.event.event_id().clone()),
    };
    fixture
        .writer()
        .apply_graph(graph_write_batch(second_mutation.clone()).unwrap())
        .await
        .unwrap();
    let mut request = WorkPublicationRequest::new(Provider::Claude);
    request.semantic_similarity_basis_points = 9_800;
    let second_report = coordinator
        .publish_work(second_mutation, request)
        .await
        .unwrap();

    assert_ne!(second_report.work_id, first_report.work_id);
    assert!(matches!(
        second_report.correlation.decision,
        CorrelationDecision::Create
    ));
}

#[tokio::test]
async fn explicit_same_project_work_id_wins_without_overlap() {
    let fixture = FixtureRuntime::codex_records(2).await;
    fixture.drain().await.unwrap();
    let events = fixture
        .read_store()
        .events_after(0, 2, 1024 * 1024)
        .await
        .unwrap();
    let coordinator =
        IngestionCoordinator::new(fixture.read_store().clone(), fixture.writer().clone(), 1);
    let first = &events[0];
    let first_mutation = GraphMutation {
        facts: vec![ReducedFact::HumanObjective {
            project_id: first.event.project_id().clone(),
            content_hash: "b3:explicit-first".into(),
            redacted_text: Some("Initial explicit work".into()),
            observed_at: first.event.observed_at(),
            evidence: first.event.event_id().clone(),
        }],
        expected_event_seq: 0,
        through_event_seq: Some(first.event_seq),
        through_event_id: Some(first.event.event_id().clone()),
    };
    fixture
        .writer()
        .apply_graph(graph_write_batch(first_mutation.clone()).unwrap())
        .await
        .unwrap();
    let first_report = coordinator
        .publish_work(first_mutation, WorkPublicationRequest::new(Provider::Codex))
        .await
        .unwrap();

    let second = &events[1];
    let second_mutation = GraphMutation {
        facts: vec![ReducedFact::HumanConstraint {
            project_id: second.event.project_id().clone(),
            content_hash: "b3:explicit-second".into(),
            redacted_text: Some("Keep the same work item".into()),
            observed_at: second.event.observed_at(),
            evidence: second.event.event_id().clone(),
        }],
        expected_event_seq: first.event_seq,
        through_event_seq: Some(second.event_seq),
        through_event_id: Some(second.event.event_id().clone()),
    };
    fixture
        .writer()
        .apply_graph(graph_write_batch(second_mutation.clone()).unwrap())
        .await
        .unwrap();
    let mut request = WorkPublicationRequest::new(Provider::Claude);
    request.explicit_work_id = Some(first_report.work_id.clone());
    let report = coordinator
        .publish_work(second_mutation, request)
        .await
        .unwrap();

    assert_eq!(report.work_id, first_report.work_id);
    assert_eq!(report.revision, 2);
    assert!(matches!(
        report.correlation.decision,
        CorrelationDecision::Continue { .. }
    ));
}

#[tokio::test]
async fn candidate_loading_is_priority_ordered_bounded_and_observably_truncated() {
    let fixture = FixtureRuntime::codex_records(1).await;
    fixture.drain().await.unwrap();
    let events = fixture
        .read_store()
        .events_after(0, 1, 1024 * 1024)
        .await
        .unwrap();
    let event = &events[0];
    for index in 0..65 {
        let work = format!("work-candidate-{index:03}");
        let contract = format!("contract-candidate-{index:03}");
        fixture
            .writer()
            .apply_work(WorkWriteBatch {
                visibility_name: format!("candidate-fixture-{index:03}"),
                expected_event_seq: 0,
                next_event_seq: event.event_seq,
                next_event_id: event.event.event_id().clone(),
                project_id: event.event.project_id().clone(),
                work_id: work_id(&work),
                status: "active".into(),
                observed_at: event.event.observed_at(),
                evidence_event_ids: vec![event.event.event_id().clone()],
                artifact_ids: Vec::new(),
                edges: Vec::new(),
                contract: WorkContractRow {
                    contract_id: contract_id(&contract),
                    revision: 1,
                    contract_json: format!(
                        r#"{{"work_id":"{work}","project_id":"{}","revision":1,"material_content_hash":"b3:{index:03}"}}"#,
                        event.event.project_id().as_str()
                    ),
                    extractor_version: "deterministic-v1".into(),
                    objective: None,
                    summary: String::new(),
                    completed_steps: Vec::new(),
                    next_actions: Vec::new(),
                    blockers: Vec::new(),
                    artifacts: Vec::new(),
                    verification: Vec::new(),
                },
            })
            .await
            .unwrap();
    }

    let page = fixture
        .writer()
        .load_work_candidates(WorkCandidateQuery {
            project_id: event.event.project_id().clone(),
            explicit_work_id: Some(work_id("work-candidate-000")),
            continuation_work_id: None,
            artifact_hashes: Vec::new(),
            command_hashes: Vec::new(),
            observed_at: event.event.observed_at(),
        })
        .await
        .unwrap();
    assert_eq!(page.candidates.len(), 64);
    assert!(page.truncated);
    assert_eq!(page.candidates[0].work_id, work_id("work-candidate-000"));
}
