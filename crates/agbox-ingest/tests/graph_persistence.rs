#![allow(clippy::unwrap_used)]

use agbox_core::Provider;
use agbox_ingest::{graph_write_batch, reducer_events_after, test_support::FixtureRuntime};
use agbox_store::{MAX_EVENT_PAGE_BYTES, MAX_EVENT_PAGE_ROWS, StoreError};
use agbox_workgraph::{GraphMutation, ReducedFact};

#[tokio::test]
async fn graph_translation_and_writer_retry_are_exact_once() {
    let fixture = FixtureRuntime::codex_records(2).await;
    fixture.drain().await.unwrap();
    let events = fixture
        .read_store()
        .events_after(0, 1_000, 4 * 1024 * 1024)
        .await
        .unwrap();
    let event = &events[0].event;
    let project_id = event.project_id().clone();
    let session_id = event.session_id().clone();
    let evidence = event.event_id().clone();
    let mutation = GraphMutation {
        facts: vec![
            ReducedFact::AgentRunStarted {
                project_id: project_id.clone(),
                session_id: session_id.clone(),
                provider: Provider::Codex,
                native_agent_id: "run-1".into(),
                observed_at: event.observed_at(),
                evidence: evidence.clone(),
            },
            ReducedFact::Artifact {
                project_id: project_id.clone(),
                session_id: session_id.clone(),
                path_hash: "b3:path".into(),
                project_relative_path: Some("src/lib.rs".into()),
                operation: "update".into(),
                content_hash: Some("b3:file".into()),
                observed_at: event.observed_at(),
                evidence: evidence.clone(),
            },
            ReducedFact::ActionRequested {
                project_id: project_id.clone(),
                session_id: session_id.clone(),
                native_action_id: "cargo-test".into(),
                tool_name: "shell".into(),
                input_hash: "b3:command".into(),
                redacted_input: Some("cargo test".into()),
                evidence: evidence.clone(),
            },
            ReducedFact::Verification {
                project_id,
                session_id,
                native_action_id: "cargo-test".into(),
                command: Some("cargo test".into()),
                succeeded: true,
                basis: "structured_tool_result",
                observed_at: event.observed_at(),
                evidence: evidence.clone(),
            },
        ],
        expected_event_seq: 0,
        through_event_seq: Some(events[0].event_seq),
        through_event_id: Some(evidence),
    };
    let batch = graph_write_batch(mutation).unwrap();

    fixture.writer().apply_graph(batch.clone()).await.unwrap();
    let first = fixture.read_store().graph_counts_for_test().await.unwrap();
    fixture.writer().apply_graph(batch.clone()).await.unwrap();
    let second = fixture.read_store().graph_counts_for_test().await.unwrap();

    assert_eq!(first, second);
    assert_eq!(first.projects, 1);
    assert_eq!(first.runs, 1);
    assert_eq!(first.actions, 1);
    assert_eq!(first.artifacts, 1);
    assert_eq!(first.verifications, 1);
    assert!(first.evidence > 0);
    assert!(first.evidence_joins > 0);

    let mut altered = batch;
    altered.actions[0].tool_name = "different-tool".into();
    let error = fixture.writer().apply_graph(altered).await.unwrap_err();
    assert!(matches!(error, StoreError::ReducerWatermarkConflict));
}

#[tokio::test]
async fn observed_finish_on_same_page_never_creates_verification() {
    let fixture = FixtureRuntime::codex_records(1).await;
    fixture.drain().await.unwrap();
    let events = fixture
        .read_store()
        .events_after(0, 1_000, 4 * 1024 * 1024)
        .await
        .unwrap();
    let committed = &events[0];
    let mutation = GraphMutation {
        facts: vec![
            ReducedFact::ActionRequested {
                project_id: committed.event.project_id().clone(),
                session_id: committed.event.session_id().clone(),
                native_action_id: "same-page".into(),
                tool_name: "shell".into(),
                input_hash: "b3:same-page".into(),
                redacted_input: Some("cargo check".into()),
                evidence: committed.event.event_id().clone(),
            },
            ReducedFact::ActionFinishedObserved {
                project_id: committed.event.project_id().clone(),
                session_id: committed.event.session_id().clone(),
                native_action_id: "same-page".into(),
                succeeded: true,
                observed_at: committed.event.observed_at(),
                evidence: committed.event.event_id().clone(),
            },
        ],
        expected_event_seq: 0,
        through_event_seq: Some(committed.event_seq),
        through_event_id: Some(committed.event.event_id().clone()),
    };
    fixture
        .writer()
        .apply_graph(graph_write_batch(mutation).unwrap())
        .await
        .unwrap();

    let counts = fixture.read_store().graph_counts_for_test().await.unwrap();
    assert_eq!(counts.actions, 1);
    assert_eq!(counts.verifications, 0);
}

#[tokio::test]
async fn observed_finish_across_pages_never_creates_verification_and_stale_watermark_is_rejected() {
    let fixture = FixtureRuntime::codex_records(3).await;
    fixture.drain().await.unwrap();
    let events = fixture
        .read_store()
        .events_after(0, 1_000, 4 * 1024 * 1024)
        .await
        .unwrap();
    let first = &events[0];
    let second = &events[1];
    let request = GraphMutation {
        facts: vec![ReducedFact::ActionRequested {
            project_id: first.event.project_id().clone(),
            session_id: first.event.session_id().clone(),
            native_action_id: "cross-slice".into(),
            tool_name: "shell".into(),
            input_hash: "b3:cross".into(),
            redacted_input: Some("cargo check".into()),
            evidence: first.event.event_id().clone(),
        }],
        expected_event_seq: 0,
        through_event_seq: Some(first.event_seq),
        through_event_id: Some(first.event.event_id().clone()),
    };
    fixture
        .writer()
        .apply_graph(graph_write_batch(request).unwrap())
        .await
        .unwrap();
    let finish = GraphMutation {
        facts: vec![ReducedFact::ActionFinishedObserved {
            project_id: second.event.project_id().clone(),
            session_id: second.event.session_id().clone(),
            native_action_id: "cross-slice".into(),
            succeeded: true,
            observed_at: second.event.observed_at(),
            evidence: second.event.event_id().clone(),
        }],
        expected_event_seq: first.event_seq,
        through_event_seq: Some(second.event_seq),
        through_event_id: Some(second.event.event_id().clone()),
    };
    fixture
        .writer()
        .apply_graph(graph_write_batch(finish).unwrap())
        .await
        .unwrap();
    assert_eq!(
        fixture
            .read_store()
            .graph_counts_for_test()
            .await
            .unwrap()
            .verifications,
        0
    );

    let stale = GraphMutation {
        facts: Vec::new(),
        expected_event_seq: 0,
        through_event_seq: Some(events[2].event_seq),
        through_event_id: Some(events[2].event.event_id().clone()),
    };
    let error = fixture
        .writer()
        .apply_graph(graph_write_batch(stale).unwrap())
        .await
        .unwrap_err();
    assert!(matches!(error, StoreError::ReducerWatermarkConflict));
}

#[tokio::test]
async fn unmatched_finish_stays_observed_without_verification() {
    let fixture = FixtureRuntime::codex_records(1).await;
    fixture.drain().await.unwrap();
    let events = fixture
        .read_store()
        .events_after(0, 1_000, 4 * 1024 * 1024)
        .await
        .unwrap();
    let committed = &events[0];
    let mutation = GraphMutation {
        facts: vec![ReducedFact::ActionFinishedObserved {
            project_id: committed.event.project_id().clone(),
            session_id: committed.event.session_id().clone(),
            native_action_id: "missing".into(),
            succeeded: false,
            observed_at: committed.event.observed_at(),
            evidence: committed.event.event_id().clone(),
        }],
        expected_event_seq: 0,
        through_event_seq: Some(committed.event_seq),
        through_event_id: Some(committed.event.event_id().clone()),
    };
    fixture
        .writer()
        .apply_graph(graph_write_batch(mutation).unwrap())
        .await
        .unwrap();
    let counts = fixture.read_store().graph_counts_for_test().await.unwrap();
    assert_eq!(counts.actions, 0);
    assert_eq!(counts.verifications, 0);
}

#[tokio::test]
async fn event_pages_use_local_sequence_and_enforce_row_and_byte_caps() {
    let records = (0..=MAX_EVENT_PAGE_ROWS).map(|index| {
        format!(
            r#"{{"type":"event_msg","ordinal":{},"payload":{{"type":"user_message","message":"message-{}"}}}}"#,
            index + 1,
            index + 1
        )
    });
    let fixture = FixtureRuntime::records(records).await;
    fixture.drain().await.unwrap();

    let first = fixture
        .read_store()
        .events_after(0, usize::MAX, usize::MAX)
        .await
        .unwrap();
    assert_eq!(first.len(), MAX_EVENT_PAGE_ROWS);
    assert!(
        first
            .windows(2)
            .all(|pair| pair[0].event_seq < pair[1].event_seq)
    );
    let semantic_bytes = first
        .iter()
        .map(|event| serde_json::to_vec(&event.event).unwrap().len() + size_of::<u64>())
        .sum::<usize>();
    assert!(semantic_bytes <= MAX_EVENT_PAGE_BYTES);

    let remainder = reducer_events_after(
        fixture.read_store(),
        first.last().unwrap().event_seq,
        usize::MAX,
        usize::MAX,
    )
    .await
    .unwrap();
    assert_eq!(remainder.len(), 1);
    assert!(remainder[0].event_seq > first.last().unwrap().event_seq);

    let first_size = serde_json::to_vec(&first[0].event).unwrap().len() + size_of::<u64>();
    let byte_limited = fixture
        .read_store()
        .events_after(0, MAX_EVENT_PAGE_ROWS, first_size)
        .await
        .unwrap();
    assert_eq!(byte_limited.len(), 1);
}

#[tokio::test]
async fn production_graph_page_boundary_resumes_after_restart_and_drains_multiple_pages() {
    let mut records = vec![
        r#"{"ordinal":0,"type":"session_meta","payload":{"id":"codex-page","cwd":"/fixture/project","history_mode":"paginated"}}"#
            .to_owned(),
    ];
    records.extend((1..=998).map(|ordinal| {
        format!(
            r#"{{"ordinal":{ordinal},"type":"event_msg","payload":{{"type":"user_message","message":"message-{ordinal}"}}}}"#
        )
    }));
    records.push(
        r#"{"ordinal":999,"type":"response_item","payload":{"type":"function_call","name":"exec_command","arguments":"{}","call_id":"call-page"}}"#
            .to_owned(),
    );
    records.push(
        r#"{"ordinal":1000,"type":"event_msg","payload":{"type":"item_completed","item":{"type":"command_execution","call_id":"call-page","status":"completed","output":"done"}}}"#
            .to_owned(),
    );
    let fixture = FixtureRuntime::records(records).await;
    fixture.drain().await.unwrap();
    let event_count = fixture.read_store().event_count().await.unwrap();
    assert!(event_count > u64::try_from(MAX_EVENT_PAGE_ROWS).unwrap());

    let first_runtime = agbox_ingest::IngestionCoordinator::new(
        fixture.read_store().clone(),
        fixture.writer().clone(),
        1,
    );
    let first = first_runtime.reduce_next_graph_page().await.unwrap();
    assert!(first.applied);
    assert_eq!(first.scanned_events, MAX_EVENT_PAGE_ROWS);
    let first_counts = fixture.read_store().graph_counts_for_test().await.unwrap();
    assert_eq!(first_counts.actions, 1);
    assert_eq!(first_counts.verifications, 0);
    drop(first_runtime);

    let restarted_runtime = agbox_ingest::IngestionCoordinator::new(
        fixture.read_store().clone(),
        fixture.writer().clone(),
        1,
    );
    let second = restarted_runtime.reduce_next_graph_page().await.unwrap();
    assert!(second.applied);
    assert!(second.scanned_events > 0);
    let counts = fixture.read_store().graph_counts_for_test().await.unwrap();
    assert_eq!(counts.actions, 1);
    assert_eq!(counts.verifications, 1);
    let watermark = fixture
        .read_store()
        .reducer_watermark(agbox_ingest::GRAPH_REDUCER_NAME)
        .await
        .unwrap();
    assert_eq!(watermark.through_event_seq, event_count);
    assert!(watermark.through_event_id.is_some());

    let idle = restarted_runtime.reduce_next_graph_page().await.unwrap();
    assert!(!idle.applied);
    assert_eq!(idle.scanned_events, 0);
    assert_eq!(idle.through_event_seq, event_count);
}

#[tokio::test]
async fn session_context_on_an_earlier_page_is_applied_to_a_later_run() {
    let fixture = FixtureRuntime::codex_records(2).await;
    fixture.drain().await.unwrap();
    let events = fixture
        .read_store()
        .events_after(0, 2, MAX_EVENT_PAGE_BYTES)
        .await
        .unwrap();
    let first = &events[0];
    let second = &events[1];
    let context = GraphMutation {
        facts: vec![ReducedFact::SessionContext {
            project_id: first.event.project_id().clone(),
            session_id: first.event.session_id().clone(),
            provider: Provider::Codex,
            branch_hash: Some("b3:branch-main".into()),
            observed_at: first.event.observed_at(),
            evidence: first.event.event_id().clone(),
        }],
        expected_event_seq: 0,
        through_event_seq: Some(first.event_seq),
        through_event_id: Some(first.event.event_id().clone()),
    };
    fixture
        .writer()
        .apply_graph(graph_write_batch(context).unwrap())
        .await
        .unwrap();

    let run = GraphMutation {
        facts: vec![ReducedFact::AgentRunStarted {
            project_id: second.event.project_id().clone(),
            session_id: second.event.session_id().clone(),
            provider: Provider::Codex,
            native_agent_id: "later-run".into(),
            observed_at: second.event.observed_at(),
            evidence: second.event.event_id().clone(),
        }],
        expected_event_seq: first.event_seq,
        through_event_seq: Some(second.event_seq),
        through_event_id: Some(second.event.event_id().clone()),
    };
    let batch = graph_write_batch(run).unwrap();
    let run_id = batch.runs[0].run_id.clone();
    fixture.writer().apply_graph(batch).await.unwrap();

    assert_eq!(
        fixture
            .read_store()
            .agent_run_branch_for_test(run_id)
            .await
            .unwrap()
            .as_deref(),
        Some("b3:branch-main")
    );
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn session_context_is_isolated_by_provider_for_the_same_project_and_session() {
    let fixture = FixtureRuntime::codex_records(4).await;
    fixture.drain().await.unwrap();
    let events = fixture
        .read_store()
        .events_after(0, 4, MAX_EVENT_PAGE_BYTES)
        .await
        .unwrap();

    let codex_context = GraphMutation {
        facts: vec![ReducedFact::SessionContext {
            project_id: events[0].event.project_id().clone(),
            session_id: events[0].event.session_id().clone(),
            provider: Provider::Codex,
            branch_hash: Some("b3:codex-branch".into()),
            observed_at: events[0].event.observed_at(),
            evidence: events[0].event.event_id().clone(),
        }],
        expected_event_seq: 0,
        through_event_seq: Some(events[0].event_seq),
        through_event_id: Some(events[0].event.event_id().clone()),
    };
    let codex_context_batch = graph_write_batch(codex_context).unwrap();
    let codex_context_id = codex_context_batch.contexts[0].context_run_id.clone();
    fixture
        .writer()
        .apply_graph(codex_context_batch)
        .await
        .unwrap();

    let claude_context = GraphMutation {
        facts: vec![ReducedFact::SessionContext {
            project_id: events[1].event.project_id().clone(),
            session_id: events[1].event.session_id().clone(),
            provider: Provider::Claude,
            branch_hash: Some("b3:claude-branch".into()),
            observed_at: events[1].event.observed_at(),
            evidence: events[1].event.event_id().clone(),
        }],
        expected_event_seq: events[0].event_seq,
        through_event_seq: Some(events[1].event_seq),
        through_event_id: Some(events[1].event.event_id().clone()),
    };
    let claude_context_batch = graph_write_batch(claude_context).unwrap();
    let claude_context_id = claude_context_batch.contexts[0].context_run_id.clone();
    assert_ne!(codex_context_id, claude_context_id);
    fixture
        .writer()
        .apply_graph(claude_context_batch)
        .await
        .unwrap();

    let codex_run = GraphMutation {
        facts: vec![ReducedFact::AgentRunStarted {
            project_id: events[2].event.project_id().clone(),
            session_id: events[2].event.session_id().clone(),
            provider: Provider::Codex,
            native_agent_id: "codex-run".into(),
            observed_at: events[2].event.observed_at(),
            evidence: events[2].event.event_id().clone(),
        }],
        expected_event_seq: events[1].event_seq,
        through_event_seq: Some(events[2].event_seq),
        through_event_id: Some(events[2].event.event_id().clone()),
    };
    let codex_run_batch = graph_write_batch(codex_run).unwrap();
    let codex_run_id = codex_run_batch.runs[0].run_id.clone();
    fixture.writer().apply_graph(codex_run_batch).await.unwrap();

    let claude_run = GraphMutation {
        facts: vec![ReducedFact::AgentRunStarted {
            project_id: events[3].event.project_id().clone(),
            session_id: events[3].event.session_id().clone(),
            provider: Provider::Claude,
            native_agent_id: "claude-run".into(),
            observed_at: events[3].event.observed_at(),
            evidence: events[3].event.event_id().clone(),
        }],
        expected_event_seq: events[2].event_seq,
        through_event_seq: Some(events[3].event_seq),
        through_event_id: Some(events[3].event.event_id().clone()),
    };
    let claude_run_batch = graph_write_batch(claude_run).unwrap();
    let claude_run_id = claude_run_batch.runs[0].run_id.clone();
    fixture
        .writer()
        .apply_graph(claude_run_batch)
        .await
        .unwrap();

    assert_eq!(
        fixture
            .read_store()
            .agent_run_branch_for_test(codex_run_id)
            .await
            .unwrap()
            .as_deref(),
        Some("b3:codex-branch")
    );
    assert_eq!(
        fixture
            .read_store()
            .agent_run_branch_for_test(claude_run_id)
            .await
            .unwrap()
            .as_deref(),
        Some("b3:claude-branch")
    );
}
