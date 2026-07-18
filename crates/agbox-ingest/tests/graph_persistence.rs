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
async fn cross_slice_finish_joins_request_and_stale_watermark_is_rejected() {
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
        1
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
