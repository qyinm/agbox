#![allow(clippy::unwrap_used)]

use agbox_adapters::{
    CodexAdapter, DecodeContext, DecodeDisposition, DecoderState, MemoryRecordSource,
    SourceAdapter, test_support::decode_fixture_file,
};
use agbox_core::{ActionOutcome, Actor, EventPayload, ProjectId};
use time::OffsetDateTime;

fn context() -> DecodeContext {
    DecodeContext {
        project_id: ProjectId::for_test("project_fixture"),
        project_root: Some("/fixture/project".into()),
        source_id: "source_fixture".to_owned(),
        observed_at: OffsetDateTime::UNIX_EPOCH,
        source_generation: 11,
        format: "codex-rollout-1".to_owned(),
    }
}

fn decode_one(json: &str, state: &DecoderState) -> agbox_adapters::DecodedRecord {
    decode_with_context(json, state, &context())
}

fn decode_with_context(
    json: &str,
    state: &DecoderState,
    context: &DecodeContext,
) -> agbox_adapters::DecodedRecord {
    CodexAdapter
        .decode(
            &MemoryRecordSource::new(json.as_bytes().to_vec()),
            context,
            state,
        )
        .unwrap()
}

fn agent_started(record: &agbox_adapters::DecodedRecord) -> usize {
    record
        .events()
        .iter()
        .filter(|event| matches!(event.payload(), EventPayload::AgentStarted { .. }))
        .count()
}

#[test]
fn codex_subagent_activity_preserves_parent_relationships_without_raw_graph_data() {
    let records = decode_fixture_file("codex", "tests/fixtures/codex/subagents.jsonl").unwrap();
    let events = records
        .iter()
        .flat_map(agbox_adapters::DecodedRecord::events)
        .collect::<Vec<_>>();
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event.payload(), EventPayload::AgentStarted { .. }))
            .count(),
        3
    );
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event.payload(), EventPayload::AgentFinished { .. }))
            .count(),
        1
    );
    let graph_events = events
        .iter()
        .filter(|event| {
            matches!(
                event.payload(),
                EventPayload::AgentStarted { .. }
                    | EventPayload::AgentFinished { .. }
                    | EventPayload::MessageCreated { .. }
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(
        graph_events
            .iter()
            .filter(|event| {
                event.actor() == Actor::Agent
                    && matches!(event.payload(), EventPayload::MessageCreated { .. })
            })
            .count(),
        1
    );
    assert!(
        graph_events
            .iter()
            .all(|event| event.causation_id().is_some())
    );
    assert!(
        graph_events
            .iter()
            .all(|event| event.correlation_id().is_some())
    );

    let event_json = serde_json::to_string(&events).unwrap();
    let evidence_json = format!(
        "{:?}",
        records
            .iter()
            .flat_map(agbox_adapters::DecodedRecord::evidence)
            .collect::<Vec<_>>()
    );
    let state =
        String::from_utf8(records.last().unwrap().next_state().as_bytes().to_vec()).unwrap();
    let debug = format!("{records:?}");
    for forbidden in [
        "PRIVATE_AGENT",
        "SECOND_AGENT",
        "THIRD_AGENT",
        "PRIVATE_PARENT",
        "PRIVATE_FORK",
        "PRIVATE_SENDER",
        "PRIVATE_TOKEN",
        "PRIVATE_DELEGATED_PROMPT",
        "PRIVATE_REASONING",
        "PRIVATE_INTER_AGENT_MESSAGE",
        "PRIVATE_MESSAGE_REASONING",
        "PRIVATE_AGENT_RESULT",
        "PRIVATE_FINISH_REASONING",
        "/Users/alice",
        "QUJDREVGR0hJSktMTU5PUFFSU1RVVldYWVo=",
    ] {
        assert!(!event_json.contains(forbidden), "event leaked {forbidden}");
        assert!(
            !evidence_json.contains(forbidden),
            "evidence leaked {forbidden}"
        );
        assert!(!state.contains(forbidden), "state leaked {forbidden}");
        assert!(!debug.contains(forbidden), "Debug leaked {forbidden}");
    }
}

#[test]
fn duplicate_response_and_event_views_share_one_semantic_fact() {
    let records = decode_fixture_file("codex", "tests/fixtures/codex/duplicates.jsonl").unwrap();
    let finished = records
        .iter()
        .flat_map(agbox_adapters::DecodedRecord::events)
        .filter_map(|event| match event.payload() {
            EventPayload::ActionFinished {
                native_action_id,
                outcome,
                ..
            } => Some((native_action_id, outcome)),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(finished.len(), 1);
    assert_eq!(*finished[0].1, ActionOutcome::Succeeded);
    let state: serde_json::Value =
        serde_json::from_slice(records.last().unwrap().next_state().as_bytes()).unwrap();
    assert_eq!(state["completed_semantic_keys"][0][1], 3);
}

#[test]
fn malformed_graph_record_advances_verified_envelope_and_recovers() {
    let records = decode_fixture_file("codex", "tests/fixtures/codex/malformed.jsonl").unwrap();
    assert!(matches!(
        records[1].disposition(),
        DecodeDisposition::Malformed { .. }
    ));
    assert!(records[1].events().is_empty());
    let progressed: serde_json::Value =
        serde_json::from_slice(records[1].next_state().as_bytes()).unwrap();
    assert_eq!(progressed["last_ordinal"], 1);
    assert_eq!(agent_started(&records[2]), 1);
}

#[test]
fn lifecycle_replay_and_contradictory_outcome_are_deduplicated() {
    let mut state = DecoderState::default();
    let records = [
        r#"{"type":"event_msg","payload":{"type":"sub_agent_activity","agent_id":"agent-replay","parent_thread_id":"parent-replay","status":"started"}}"#,
        r#"{"type":"event_msg","payload":{"type":"sub_agent_activity","agent_id":"agent-replay","parent_thread_id":"parent-replay","status":"running"}}"#,
        r#"{"type":"event_msg","payload":{"type":"sub_agent_activity","agent_id":"agent-replay","parent_thread_id":"parent-replay","status":"completed"}}"#,
        r#"{"type":"event_msg","payload":{"type":"sub_agent_activity","agent_id":"agent-replay","parent_thread_id":"parent-replay","status":"failed"}}"#,
    ]
    .into_iter()
    .map(|json| {
        let decoded = decode_one(json, &state);
        state = decoded.next_state().clone();
        decoded
    })
    .collect::<Vec<_>>();
    assert_eq!(records.iter().map(agent_started).sum::<usize>(), 1);
    assert_eq!(
        records
            .iter()
            .flat_map(agbox_adapters::DecodedRecord::events)
            .filter(|event| matches!(event.payload(), EventPayload::AgentFinished { .. }))
            .count(),
        1
    );
}

#[test]
fn distinct_agent_and_metadata_identities_remain_separate() {
    let decoded = decode_one(
        r#"{"type":"response_item","payload":{"type":"inter_agent_communication_metadata","agentId":"same-looking-agent","metadata":{"agent_id":"different-agent","status":"started"},"forked_from_id":"parent"}}"#,
        &DecoderState::default(),
    );
    let ids = decoded
        .events()
        .iter()
        .filter_map(|event| match event.payload() {
            EventPayload::AgentStarted { native_agent_id } => Some(native_agent_id),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(ids.len(), 2);
    assert_ne!(ids[0], ids[1]);
    assert!(ids.iter().all(|id| id.starts_with("codex_graph_")));
}

#[test]
fn graph_identities_are_provider_session_and_domain_separated() {
    let record = r#"{"type":"event_msg","payload":{"type":"sub_agent_activity","agent_id":"same-raw-value","parent_thread_id":"same-raw-value","status":"started"}}"#;
    let first = decode_one(record, &DecoderState::default());
    let event = first
        .events()
        .iter()
        .find(|event| matches!(event.payload(), EventPayload::AgentStarted { .. }))
        .unwrap();
    let EventPayload::AgentStarted { native_agent_id } = event.payload() else {
        unreachable!();
    };
    assert_ne!(event.causation_id(), Some(native_agent_id.as_str()));
    assert!(!native_agent_id.contains("same-raw-value"));

    let mut other_context = context();
    other_context.source_id = "source_other_session".to_owned();
    let other = decode_with_context(record, &DecoderState::default(), &other_context);
    let other_id = other
        .events()
        .iter()
        .find_map(|event| match event.payload() {
            EventPayload::AgentStarted { native_agent_id } => Some(native_agent_id),
            _ => None,
        })
        .unwrap();
    assert_ne!(native_agent_id, other_id);
}

#[test]
fn lifecycle_window_is_bounded_and_evicted_identity_can_restart() {
    let mut state = DecoderState::default();
    let mut first_id = None;
    for index in 0..129 {
        let decoded = decode_one(
            &format!(
                r#"{{"type":"event_msg","payload":{{"type":"sub_agent_activity","agent_id":"window-agent-{index}","parent_thread_id":"window-parent","status":"started"}}}}"#
            ),
            &state,
        );
        if index == 0 {
            first_id = decoded
                .events()
                .iter()
                .find_map(|event| match event.payload() {
                    EventPayload::AgentStarted { native_agent_id } => Some(native_agent_id.clone()),
                    _ => None,
                });
        }
        state = decoded.next_state().clone();
    }
    assert!(state.as_bytes().len() <= agbox_adapters::MAX_DECODER_STATE_BYTES);
    let state_json: serde_json::Value = serde_json::from_slice(state.as_bytes()).unwrap();
    assert_eq!(state_json["agent_lifecycle"].as_array().unwrap().len(), 128);
    assert!(
        !state_json["agent_lifecycle"]
            .as_array()
            .unwrap()
            .iter()
            .any(|entry| entry[0] == first_id.as_ref().unwrap().as_str())
    );

    let stale_finish = decode_one(
        r#"{"type":"event_msg","payload":{"type":"sub_agent_activity","agent_id":"window-agent-0","parent_thread_id":"window-parent","status":"failed"}}"#,
        &state,
    );
    assert!(
        !stale_finish
            .events()
            .iter()
            .any(|event| matches!(event.payload(), EventPayload::AgentFinished { .. }))
    );
    let restarted = decode_one(
        r#"{"type":"event_msg","payload":{"type":"sub_agent_activity","agent_id":"window-agent-0","parent_thread_id":"window-parent","status":"started"}}"#,
        stale_finish.next_state(),
    );
    assert_eq!(agent_started(&restarted), 1);
}

#[test]
fn two_identities_in_one_record_survive_final_state_staging() {
    let mut state = DecoderState::default();
    for index in 0..127 {
        state = decode_one(
            &format!(
                r#"{{"type":"event_msg","payload":{{"type":"sub_agent_activity","agent_id":"pressure-{index}","parent_thread_id":"parent","status":"started"}}}}"#
            ),
            &state,
        )
        .next_state()
        .clone();
    }
    let decoded = decode_one(
        r#"{"type":"response_item","payload":{"type":"inter_agent_communication_metadata","agentId":"new-one","metadata":{"agent_id":"new-two","status":"started"},"forked_from_id":"parent"}}"#,
        &state,
    );
    assert_eq!(agent_started(&decoded), 2);
    let state_json: serde_json::Value =
        serde_json::from_slice(decoded.next_state().as_bytes()).unwrap();
    assert_eq!(state_json["agent_lifecycle"].as_array().unwrap().len(), 128);
}

#[test]
fn malformed_null_duplicate_and_heterogeneous_graph_fields_recover_by_ordinal() {
    let bad_records = [
        r#"{"timestamp":"2026-07-17T03:00:01Z","ordinal":1,"type":"event_msg","payload":{"type":"sub_agent_activity","agent_id":null,"parent_thread_id":"parent","status":"started"}}"#,
        r#"{"timestamp":"2026-07-17T03:00:01Z","ordinal":1,"type":"event_msg","payload":{"type":"sub_agent_activity","agent_id":"agent","parent_thread_id":"parent","status":null}}"#,
        r#"{"timestamp":"2026-07-17T03:00:01Z","ordinal":1,"type":"event_msg","payload":{"type":"sub_agent_activity","agent_id":"agent","parent_thread_id":"parent","status":{"future":true}}}"#,
        r#"{"timestamp":"2026-07-17T03:00:01Z","ordinal":1,"type":"event_msg","payload":{"type":"sub_agent_activity","agent_id":"agent","parent_thread_id":"parent","status":"started","status":"failed"}}"#,
        r#"{"timestamp":"2026-07-17T03:00:01Z","ordinal":1,"type":"response_item","payload":{"type":"inter_agent_communication_metadata","agentId":"agent","metadata":{"agent_id":{"private":"PRIVATE_HETEROGENEOUS"},"status":"started"},"forked_from_id":"parent"}}"#,
        r#"{"timestamp":"2026-07-17T03:00:01Z","ordinal":1,"type":"event_msg","payload":{"type":"sub_agent_activity","agent_id":"agent","parent_thread_id":"one","forked_from_id":"two","status":"started"}}"#,
    ];
    for bad in bad_records {
        let first = decode_one(
            r#"{"timestamp":"2026-07-17T03:00:00Z","ordinal":0,"type":"session_meta","payload":{"history_mode":"paginated"}}"#,
            &DecoderState::default(),
        );
        let isolated = decode_one(bad, first.next_state());
        assert!(isolated.events().is_empty());
        let progressed: serde_json::Value =
            serde_json::from_slice(isolated.next_state().as_bytes()).unwrap();
        assert_eq!(progressed["last_ordinal"], 1);
        assert!(!format!("{isolated:?}").contains("PRIVATE_HETEROGENEOUS"));
        let recovered = decode_one(
            r#"{"timestamp":"2026-07-17T03:00:02Z","ordinal":2,"type":"event_msg","payload":{"type":"sub_agent_activity","agent_id":"recovered","parent_thread_id":"parent","status":"started"}}"#,
            isolated.next_state(),
        );
        assert_eq!(agent_started(&recovered), 1);
    }
}

#[test]
fn unknown_graph_variant_does_not_inspect_irrelevant_fields() {
    let first = decode_one(
        r#"{"timestamp":"2026-07-17T03:00:00Z","ordinal":0,"type":"session_meta","payload":{"history_mode":"paginated"}}"#,
        &DecoderState::default(),
    );
    let unknown = decode_one(
        r#"{"timestamp":"2026-07-17T03:00:01Z","ordinal":1,"type":"event_msg","payload":{"type":"future_graph_variant","agent_id":{"token":"PRIVATE_UNKNOWN_AGENT"},"parent_thread_id":["PRIVATE_UNKNOWN_PARENT"],"status":{"reasoning":"PRIVATE_UNKNOWN_REASONING"}}}"#,
        first.next_state(),
    );
    assert!(matches!(
        unknown.disposition(),
        DecodeDisposition::UnknownType { .. }
    ));
    assert!(unknown.events().is_empty());
    assert!(unknown.evidence().is_empty());
    let state = String::from_utf8(unknown.next_state().as_bytes().to_vec()).unwrap();
    let debug = format!("{unknown:?}");
    for forbidden in [
        "PRIVATE_UNKNOWN_AGENT",
        "PRIVATE_UNKNOWN_PARENT",
        "PRIVATE_UNKNOWN_REASONING",
    ] {
        assert!(!state.contains(forbidden));
        assert!(!debug.contains(forbidden));
    }
}

#[test]
#[allow(clippy::too_many_lines)]
fn agent_pressure_preserves_maximum_live_result_correlation_and_continuation() {
    let mut state = decode_one(
        r#"{"timestamp":"2026-07-17T02:00:00Z","type":"session_meta","payload":{}}"#,
        &DecoderState::default(),
    )
    .next_state()
    .clone();
    let call_id = |class: &str, index: usize| {
        let prefix = format!("call-{class}-{index}-");
        format!("{prefix}{}", "x".repeat(128 - prefix.len()))
    };

    for index in 0..64 {
        let call_id = call_id("pending", index);
        let request = decode_one(
            &format!(
                r#"{{"type":"response_item","payload":{{"type":"function_call","name":"maximum_width_tool_name","arguments":"{{}}","call_id":"{call_id}"}}}}"#
            ),
            &state,
        );
        let staged = decode_one(
            &format!(
                r#"{{"type":"response_item","payload":{{"type":"function_call_output","call_id":"{call_id}","status":"completed","output":"pending-{index}"}}}}"#
            ),
            request.next_state(),
        );
        state = staged.next_state().clone();
    }
    for index in 0..64 {
        let call_id = call_id("unresolved", index);
        state = decode_one(
            &format!(
                r#"{{"type":"response_item","payload":{{"type":"function_call","name":"maximum_width_tool_name","arguments":"{{}}","call_id":"{call_id}"}}}}"#
            ),
            &state,
        )
        .next_state()
        .clone();
    }
    for index in 0..129 {
        state = decode_one(
            &format!(
                r#"{{"type":"event_msg","payload":{{"type":"sub_agent_activity","agent_id":"pressure-agent-{index}","parent_thread_id":"pressure-parent","status":"started"}}}}"#
            ),
            &state,
        )
        .next_state()
        .clone();
    }
    let pressured: serde_json::Value = serde_json::from_slice(state.as_bytes()).unwrap();
    assert_eq!(pressured["pending_results"].as_array().unwrap().len(), 64);
    assert_eq!(pressured["unresolved_calls"].as_array().unwrap().len(), 64);
    assert_eq!(pressured["agent_lifecycle"].as_array().unwrap().len(), 128);
    assert!(state.as_bytes().len() <= agbox_adapters::MAX_DECODER_STATE_BYTES);

    let terminal = decode_one(
        r#"{"type":"event_msg","payload":{"type":"task_complete"}}"#,
        &state,
    );
    let mut finished = terminal
        .events()
        .iter()
        .filter(|event| matches!(event.payload(), EventPayload::ActionFinished { .. }))
        .count();
    state = terminal.next_state().clone();
    let terminal_state: serde_json::Value = serde_json::from_slice(state.as_bytes()).unwrap();
    assert!(!terminal_state["continuation"].is_null());
    while let Some(page) = CodexAdapter
        .decode_continuation(&context(), &state)
        .unwrap()
    {
        finished += page
            .events()
            .iter()
            .filter(|event| matches!(event.payload(), EventPayload::ActionFinished { .. }))
            .count();
        state = page.next_state().clone();
    }
    assert_eq!(finished, 64);

    for index in 0..64 {
        let call_id = call_id("unresolved", index);
        let completed = decode_one(
            &format!(
                r#"{{"type":"event_msg","payload":{{"type":"exec_command_end","call_id":"{call_id}","exit_code":0,"stdout":"done"}}}}"#
            ),
            &state,
        );
        assert_eq!(
            completed
                .events()
                .iter()
                .filter(|event| matches!(event.payload(), EventPayload::ActionFinished { .. }))
                .count(),
            1
        );
        assert!(
            completed
                .events()
                .iter()
                .find(|event| matches!(event.payload(), EventPayload::ActionFinished { .. }))
                .unwrap()
                .causation_id()
                .is_some()
        );
        state = completed.next_state().clone();
    }
    let drained: serde_json::Value = serde_json::from_slice(state.as_bytes()).unwrap();
    assert!(drained["pending_results"].as_array().unwrap().is_empty());
    assert!(drained["unresolved_calls"].as_array().unwrap().is_empty());
    assert!(state.as_bytes().len() <= agbox_adapters::MAX_DECODER_STATE_BYTES);
}
