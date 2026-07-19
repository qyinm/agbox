#![allow(clippy::unwrap_used)]

use agbox_adapters::{
    ClaudeAdapter, DecodeContext, DecodeDisposition, DecodeError, DecoderState, MemoryRecordSource,
    SourceAdapter, test_support::decode_fixture_file,
};
use agbox_core::{ActionOutcome, EventPayload, PrivacyLabel, ProjectId};
use time::OffsetDateTime;

fn fixture(path: &str) -> String {
    format!(
        "{}/tests/fixtures/claude/{path}",
        env!("CARGO_MANIFEST_DIR")
    )
}

fn context() -> DecodeContext {
    DecodeContext {
        project_id: ProjectId::for_test("project_fixture"),
        project_root: Some("/fixture/project".into()),
        source_id: "source_graph_fixture".to_owned(),
        observed_at: OffsetDateTime::UNIX_EPOCH,
        source_generation: 7,
        format: "claude-transcript-2.1".to_owned(),
    }
}

fn try_decode(
    json: &str,
    state: &DecoderState,
) -> Result<agbox_adapters::DecodedRecord, DecodeError> {
    ClaudeAdapter.decode(
        &MemoryRecordSource::new(json.as_bytes().to_vec()),
        &context(),
        state,
    )
}

fn decode_one(json: &str, state: &DecoderState) -> agbox_adapters::DecodedRecord {
    try_decode(json, state).unwrap()
}

fn graph_events(records: &[agbox_adapters::DecodedRecord]) -> Vec<&agbox_core::ActivityEventV1> {
    records
        .iter()
        .flat_map(agbox_adapters::DecodedRecord::events)
        .collect()
}

fn relationship_count(record: &agbox_adapters::DecodedRecord) -> usize {
    record
        .events()
        .iter()
        .filter(|event| {
            matches!(
                event.payload(),
                EventPayload::DiagnosticObserved { level, .. }
                    if level == "relationship.sidechain"
            )
        })
        .count()
}

fn unresolved_pressure_state(free_bytes: usize, known_agents: &[String]) -> DecoderState {
    const STATE_BOUND: usize = 32 * 1024;
    let target = STATE_BOUND.checked_sub(free_bytes).unwrap();
    let encode = |links: &[serde_json::Value]| {
        serde_json::to_vec(&serde_json::json!({
            "unresolved_tools": links,
            "known_agents": known_agents,
            "finished_agents": [],
            "assistant_spawns": [],
            "last_human_turn": null,
            "context": {
                "cwd": null,
                "mode": null,
                "permission": null,
                "branch_hash": null,
            },
        }))
        .unwrap()
    };

    let mut links = Vec::new();
    for index in 0..128 {
        links.push(serde_json::json!({
            "tool_use_id": format!("pending-{index}-{}", "i".repeat(100)),
            "request_event_id": format!("evt-{index}-{}", "e".repeat(100)),
            "tool_name": "R".repeat(64),
            "input_hash": "a".repeat(128),
            "project_relative_path": null,
        }));
        if encode(&links).len() > target {
            let _ = links.pop();
            break;
        }
    }

    for index in 0..links.len() {
        for path_bytes in 1..=503 {
            links[index]["project_relative_path"] =
                serde_json::Value::String(format!("$PROJECT/{}", "p".repeat(path_bytes)));
            if encode(&links).len() > target {
                links[index]["project_relative_path"] = if path_bytes == 1 {
                    serde_json::Value::Null
                } else {
                    serde_json::Value::String(format!("$PROJECT/{}", "p".repeat(path_bytes - 1)))
                };
                break;
            }
        }
    }

    let bytes = encode(&links);
    assert_eq!(bytes.len(), target);
    let mut state = DecoderState::default();
    state.replace(bytes).unwrap();
    state
}

#[test]
fn claude_graph_uses_opaque_joinable_ids_and_authoritative_terminal_lifecycle() {
    let records = decode_fixture_file("claude", fixture("sidechain.jsonl")).unwrap();
    let events = graph_events(&records);
    let parent_request = records[0]
        .events()
        .iter()
        .find(|event| matches!(event.payload(), EventPayload::ActionRequested { .. }))
        .unwrap();
    let started = records[1]
        .events()
        .iter()
        .filter(|event| matches!(event.payload(), EventPayload::AgentStarted { .. }))
        .collect::<Vec<_>>();

    assert_eq!(started.len(), 2);
    assert!(started.iter().all(|event| {
        matches!(
            event.payload(),
            EventPayload::AgentStarted { native_agent_id }
                if native_agent_id.starts_with("claude_graph_")
                    && !native_agent_id.contains("PRIVATE")
        )
    }));
    assert_ne!(
        started[0].payload(),
        started[1].payload(),
        "distinct agentId and attributionAgent stay distinct after normalization"
    );
    assert!(
        started
            .iter()
            .all(|event| event.correlation_id() == Some(parent_request.event_id().as_str()))
    );
    assert!(
        records[1]
            .events()
            .iter()
            .all(|event| event.causation_id() == parent_request.turn_id())
    );
    assert_eq!(relationship_count(&records[1]), 1);

    let child_message = records[1]
        .events()
        .iter()
        .find(|event| matches!(event.payload(), EventPayload::MessageCreated { .. }))
        .unwrap();
    assert!(
        records[2]
            .events()
            .iter()
            .all(|event| event.causation_id() == child_message.turn_id())
    );
    assert_ne!(child_message.event_id(), parent_request.event_id());
    assert_ne!(child_message.turn_id(), parent_request.turn_id());

    assert!(events.iter().any(|event| matches!(
        event.payload(),
        EventPayload::ContextCompacted {
            summary_hash: Some(_)
        }
    )));
    assert!(events.iter().any(|event| matches!(
        event.payload(),
        EventPayload::TurnFinished {
            outcome: ActionOutcome::Succeeded
        }
    )));
    assert!(
        !records[3..5]
            .iter()
            .flat_map(agbox_adapters::DecodedRecord::events)
            .any(|event| matches!(event.payload(), EventPayload::AgentFinished { .. }))
    );
    let finished = events
        .iter()
        .filter_map(|event| match event.payload() {
            EventPayload::AgentFinished {
                native_agent_id,
                outcome,
            } => Some((native_agent_id, outcome)),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(finished.len(), 1);
    assert_eq!(*finished[0].1, ActionOutcome::Succeeded);
    assert!(finished[0].0.starts_with("claude_graph_"));
}

#[test]
fn every_new_graph_field_is_absent_raw_from_events_evidence_state_and_debug() {
    let records = decode_fixture_file("claude", fixture("sidechain.jsonl")).unwrap();
    let serialized = serde_json::to_string(&graph_events(&records)).unwrap();
    let debug = format!("{records:?}");
    let evidence = records
        .iter()
        .flat_map(agbox_adapters::DecodedRecord::evidence)
        .flat_map(|evidence| evidence.plaintext.iter().copied())
        .collect::<Vec<_>>();
    let state = records.last().unwrap().next_state().as_bytes();
    for forbidden in [
        "spawn-token=PRIVATE_CREDENTIAL_",
        "/Users/alice",
        "image-base64-thinking",
        "agent-token=PRIVATE_AGENT_",
        "attribution-thinking-PRIVATE_AGENT",
        "PRIVATE_TOKEN",
        "PRIVATE_BASE64",
        "PRIVATE_ASSISTANT_THINKING",
        "PRIVATE_THINKING",
        "authentication_failed",
    ] {
        assert!(!serialized.contains(forbidden));
        assert!(!debug.contains(forbidden));
        assert!(
            !evidence
                .windows(forbidden.len())
                .any(|value| value == forbidden.as_bytes())
        );
        assert!(
            !state
                .windows(forbidden.len())
                .any(|value| value == forbidden.as_bytes())
        );
    }
}

#[test]
fn system_summaries_and_real_assistant_errors_are_private_bounded_diagnostics() {
    let records = decode_fixture_file("claude", fixture("sidechain.jsonl")).unwrap();
    for record in [&records[4], &records[7], &records[8]] {
        let diagnostic = record
            .events()
            .iter()
            .find(|event| {
                matches!(
                    event.payload(),
                    EventPayload::DiagnosticObserved { level, .. }
                        if level != "relationship.sidechain"
                )
            })
            .unwrap();
        assert_eq!(diagnostic.privacy(), PrivacyLabel::PrivateLocal);
    }
    for record in &records[9..12] {
        assert!(!record.events().iter().any(|event| matches!(
            event.payload(),
            EventPayload::DiagnosticObserved { level, .. } if level == "error"
        )));
    }

    let duplicate = try_decode(
        r#"{"type":"assistant","uuid":"dup-error","sessionId":"session-graph","timestamp":"2026-07-17T03:00:12Z","error":"one","error":"two","message":{"content":[]}}"#,
        records.last().unwrap().next_state(),
    )
    .unwrap_err();
    assert!(matches!(duplicate, DecodeError::Malformed(_)));
}

#[test]
fn malformed_identity_and_unknown_irrelevant_identity_fields_are_isolated() {
    let malformed = decode_fixture_file("claude", fixture("malformed.jsonl")).unwrap();
    assert_eq!(malformed.len(), 3);
    for record in &malformed[..2] {
        assert!(matches!(
            record.disposition(),
            DecodeDisposition::Malformed { .. }
        ));
        assert_eq!(
            record.disposition().class(),
            Some("missing_required_identity")
        );
        assert!(record.events().is_empty());
        assert!(record.evidence().is_empty());
        assert_eq!(record.next_state(), &DecoderState::default());
    }
    assert!(!malformed[2].events().is_empty());

    for session in [None, Some("null"), Some(r#"{"nested":"invalid"}"#)] {
        let field = session.map_or(String::new(), |value| format!(r#","sessionId":{value}"#));
        let error = try_decode(
            &format!(
                r#"{{"type":"assistant","uuid":"session-hard-error","timestamp":"2026-07-17T04:00:02Z"{field},"message":{{"content":[]}}}}"#
            ),
            &DecoderState::default(),
        )
        .unwrap_err();
        assert!(matches!(
            error,
            DecodeError::MissingIdentity("sessionId") | DecodeError::Malformed(_)
        ));
    }

    let unknown = decode_fixture_file("claude", fixture("unknown.jsonl")).unwrap();
    assert!(matches!(
        unknown[0].disposition(),
        DecodeDisposition::UnknownType { .. }
    ));
    assert!(unknown[0].events().is_empty());
    assert!(unknown[0].evidence().is_empty());
    assert!(!unknown[1].events().is_empty());
}

#[test]
fn additive_fields_only_change_the_known_record_schema_fingerprint() {
    let records = decode_fixture_file("claude", fixture("unknown.jsonl")).unwrap();
    let baseline = &records[1];
    let additive = &records[2];
    assert!(matches!(baseline.disposition(), DecodeDisposition::Known));
    assert!(matches!(additive.disposition(), DecodeDisposition::Known));
    assert_ne!(
        baseline.observation().schema_fingerprint(),
        additive.observation().schema_fingerprint()
    );
    assert_eq!(baseline.events().len(), additive.events().len());
    for (left, right) in baseline.events().iter().zip(additive.events()) {
        assert_eq!(left.payload(), right.payload());
        assert_eq!(left.actor(), right.actor());
        assert_eq!(left.privacy(), right.privacy());
        assert_eq!(left.correlation_id(), right.correlation_id());
        assert_eq!(left.causation_id(), right.causation_id());
    }
}

#[test]
fn sidechain_true_has_one_explicit_relationship_without_order_flattening() {
    let parent = decode_one(
        r#"{"type":"assistant","uuid":"paired-parent","sessionId":"session-pair","timestamp":"2026-07-17T07:00:00Z","message":{"content":[{"type":"tool_use","id":"pair-tool","name":"Task","input":{}}]}}"#,
        &DecoderState::default(),
    );
    let true_record = decode_one(
        r#"{"type":"assistant","uuid":"paired-child","parentUuid":"paired-parent","sessionId":"session-pair","timestamp":"2026-07-17T07:00:01Z","isSidechain":true,"agentId":"paired-agent","sourceToolAssistantUUID":"paired-parent","message":{"content":"same"}}"#,
        parent.next_state(),
    );
    let false_record = decode_one(
        r#"{"type":"assistant","uuid":"paired-child","parentUuid":"paired-parent","sessionId":"session-pair","timestamp":"2026-07-17T07:00:01Z","isSidechain":false,"agentId":"paired-agent","sourceToolAssistantUUID":"paired-parent","message":{"content":"same"}}"#,
        parent.next_state(),
    );

    assert_eq!(relationship_count(&true_record), 1);
    assert_eq!(relationship_count(&false_record), 0);
    assert_eq!(true_record.events().len(), false_record.events().len() + 1);
    assert!(
        true_record
            .events()
            .iter()
            .all(|event| event.turn_id() != parent.events()[0].turn_id())
    );
}

#[test]
fn ambiguous_spawn_requests_fail_closed_without_misjoining_agent_start() {
    let parent = decode_one(
        r#"{"type":"assistant","uuid":"ambiguous-parent","sessionId":"session-ambiguous","timestamp":"2026-07-17T08:00:00Z","message":{"content":[{"type":"tool_use","id":"task-one","name":"Task","input":{}},{"type":"tool_use","id":"task-two","name":"Task","input":{}}]}}"#,
        &DecoderState::default(),
    );
    let child = decode_one(
        r#"{"type":"assistant","uuid":"ambiguous-child","parentUuid":"ambiguous-parent","sessionId":"session-ambiguous","timestamp":"2026-07-17T08:00:01Z","agentId":"ambiguous-agent","sourceToolAssistantUUID":"ambiguous-parent","message":{"content":"done"}}"#,
        parent.next_state(),
    );
    let started = child
        .events()
        .iter()
        .find(|event| matches!(event.payload(), EventPayload::AgentStarted { .. }))
        .unwrap();
    assert_eq!(started.correlation_id(), None);
}

#[test]
fn agent_history_pressure_never_evicts_pending_tool_correlations() {
    let mut state = DecoderState::default();
    for index in 0..128 {
        let request = decode_one(
            &format!(
                r#"{{"type":"assistant","uuid":"tool-parent-{index}","sessionId":"session-pressure","timestamp":"2026-07-17T09:00:00Z","message":{{"content":[{{"type":"tool_use","id":"pending-{index}","name":"Read","input":{{"path":"src/{index}.rs"}}}}]}}}}"#
            ),
            &state,
        );
        state = request.next_state().clone();
    }
    for index in 0..129 {
        let padding = "a".repeat(112);
        let agent = decode_one(
            &format!(
                r#"{{"type":"assistant","uuid":"pressure-child-{index}","sessionId":"session-pressure","timestamp":"2026-07-17T09:00:01Z","agentId":"{padding}-{index}","message":{{"content":[]}}}}"#
            ),
            &state,
        );
        state = agent.next_state().clone();
        assert!(state.as_bytes().len() <= 32 * 1024);
    }

    let mut correlated = 0;
    for index in 0..128 {
        let result = decode_one(
            &format!(
                r#"{{"type":"user","uuid":"pressure-result-{index}","sessionId":"session-pressure","timestamp":"2026-07-17T09:00:02Z","message":{{"content":[{{"type":"tool_result","tool_use_id":"pending-{index}","content":"ok","is_error":false}}]}}}}"#
            ),
            &state,
        );
        correlated += result
            .events()
            .iter()
            .filter(|event| matches!(event.payload(), EventPayload::ActionFinished { .. }))
            .count();
        state = result.next_state().clone();
    }
    assert_eq!(correlated, 128);

    let padding = "a".repeat(112);
    let resighted = decode_one(
        &format!(
            r#"{{"type":"assistant","uuid":"pressure-resighted","sessionId":"session-pressure","timestamp":"2026-07-17T09:00:03Z","agentId":"{padding}-0","message":{{"content":[]}}}}"#
        ),
        &state,
    );
    assert!(
        resighted
            .events()
            .iter()
            .any(|event| matches!(event.payload(), EventPayload::AgentStarted { .. })),
        "an identity evicted from bounded history is intentionally first-sight again"
    );
}

#[test]
fn known_agents_and_finished_agents_are_opaque_historical_bounded_sets() {
    let mut state = DecoderState::default();
    for index in 0..129 {
        let decoded = decode_one(
            &format!(
                r#"{{"type":"assistant","uuid":"child-{index}","parentUuid":"parent-{index}","sessionId":"session-agents","timestamp":"2026-07-17T06:00:00Z","isSidechain":true,"agentId":"agent-token=PRIVATE-{index}","message":{{"content":"done"}}}}"#
            ),
            &state,
        );
        assert!(decoded.events().iter().any(|event| matches!(
            event.payload(),
            EventPayload::AgentStarted { native_agent_id }
                if native_agent_id.starts_with("claude_graph_")
                    && !native_agent_id.contains("PRIVATE")
        )));
        state = decoded.next_state().clone();
        assert!(state.as_bytes().len() <= 32 * 1024);
    }

    let value: serde_json::Value = serde_json::from_slice(state.as_bytes()).unwrap();
    let agents = value["known_agents"].as_array().unwrap();
    assert!(agents.len() <= 128);
    assert!(
        agents
            .iter()
            .all(|agent| agent.as_str().unwrap().starts_with("claude_graph_"))
    );
    assert!(!String::from_utf8_lossy(state.as_bytes()).contains("PRIVATE"));

    let terminal = decode_one(
        r#"{"type":"result","subtype":"success","uuid":"finished-128","sessionId":"session-agents","timestamp":"2026-07-17T06:00:01Z","agentId":"agent-token=PRIVATE-128"}"#,
        &state,
    );
    assert!(terminal.events().iter().any(|event| matches!(
        event.payload(),
        EventPayload::AgentFinished {
            outcome: ActionOutcome::Succeeded,
            ..
        }
    )));
    let replay = decode_one(
        r#"{"type":"result","subtype":"failed","uuid":"finished-replay","sessionId":"session-agents","timestamp":"2026-07-17T06:00:02Z","agentId":"agent-token=PRIVATE-128"}"#,
        terminal.next_state(),
    );
    assert!(
        !replay
            .events()
            .iter()
            .any(|event| matches!(event.payload(), EventPayload::AgentFinished { .. }))
    );
    let terminal_state: serde_json::Value =
        serde_json::from_slice(replay.next_state().as_bytes()).unwrap();
    assert!(
        terminal_state["known_agents"]
            .as_array()
            .unwrap()
            .iter()
            .any(|agent| *agent == terminal_state["finished_agents"][0]["agent_id"])
    );
}

#[test]
fn retained_finished_marker_suppresses_restart_after_known_history_eviction() {
    let started = decode_one(
        r#"{"type":"assistant","uuid":"lifecycle-a-start","sessionId":"session-lifecycle-retained","timestamp":"2026-07-17T10:00:00Z","agentId":"lifecycle-a","message":{"content":[]}}"#,
        &DecoderState::default(),
    );
    let agent_id = started
        .events()
        .iter()
        .find_map(|event| match event.payload() {
            EventPayload::AgentStarted { native_agent_id } => Some(native_agent_id.clone()),
            _ => None,
        })
        .unwrap();
    let finished = decode_one(
        r#"{"type":"result","subtype":"success","uuid":"lifecycle-a-finish","sessionId":"session-lifecycle-retained","timestamp":"2026-07-17T10:00:01Z","agentId":"lifecycle-a"}"#,
        started.next_state(),
    );

    let mut state = finished.next_state().clone();
    for index in 0..128 {
        let decoded = decode_one(
            &format!(
                r#"{{"type":"assistant","uuid":"lifecycle-pressure-{index}","sessionId":"session-lifecycle-retained","timestamp":"2026-07-17T10:00:02Z","agentId":"lifecycle-pressure-{index}","message":{{"content":[]}}}}"#
            ),
            &state,
        );
        state = decoded.next_state().clone();
    }

    let bounded: serde_json::Value = serde_json::from_slice(state.as_bytes()).unwrap();
    assert!(
        !bounded["known_agents"]
            .as_array()
            .unwrap()
            .iter()
            .any(|known| known == &agent_id)
    );
    assert!(
        bounded["finished_agents"]
            .as_array()
            .unwrap()
            .iter()
            .any(|finished| finished["agent_id"] == agent_id)
    );

    let resighted = decode_one(
        r#"{"type":"assistant","uuid":"lifecycle-a-resighted","sessionId":"session-lifecycle-retained","timestamp":"2026-07-17T10:00:03Z","agentId":"lifecycle-a","message":{"content":[]}}"#,
        &state,
    );
    assert!(
        !resighted
            .events()
            .iter()
            .any(|event| matches!(event.payload(), EventPayload::AgentStarted { .. }))
    );
    let replayed_finish = decode_one(
        r#"{"type":"result","subtype":"failed","uuid":"lifecycle-a-replayed-finish","sessionId":"session-lifecycle-retained","timestamp":"2026-07-17T10:00:04Z","agentId":"lifecycle-a"}"#,
        resighted.next_state(),
    );
    assert!(
        !replayed_finish
            .events()
            .iter()
            .any(|event| matches!(event.payload(), EventPayload::AgentFinished { .. }))
    );
}

#[test]
fn unresolved_tool_pressure_does_not_repeat_unretained_agent_starts() {
    let mut state = unresolved_pressure_state(0, &[]);

    for ordinal in 0..2 {
        let decoded = decode_one(
            &format!(
                r#"{{"type":"assistant","uuid":"unretained-agent-{ordinal}","sessionId":"session-unretained-agent","timestamp":"2026-07-17T11:00:00Z","agentId":"unretained-agent","message":{{"content":[]}}}}"#
            ),
            &state,
        );
        assert!(
            !decoded
                .events()
                .iter()
                .any(|event| matches!(event.payload(), EventPayload::AgentStarted { .. }))
        );
        state = decoded.next_state().clone();
    }
}

#[test]
fn staged_start_is_filtered_when_later_context_pressure_evicts_its_marker() {
    let pressure = unresolved_pressure_state(64, &[]);
    let plain = decode_one(
        r#"{"type":"assistant","uuid":"context-stage-plain","sessionId":"session-context-stage","timestamp":"2026-07-17T11:10:00Z","agentId":"context-stage-agent","message":{"content":[]}}"#,
        &pressure,
    );
    assert!(
        plain
            .events()
            .iter()
            .any(|event| matches!(event.payload(), EventPayload::AgentStarted { .. }))
    );

    let with_context = decode_one(
        r#"{"type":"assistant","uuid":"context-stage-mutating","sessionId":"session-context-stage","timestamp":"2026-07-17T11:10:01Z","agentId":"context-stage-agent","cwd":"/fixture/project","message":{"content":[]}}"#,
        &pressure,
    );
    assert!(
        !with_context
            .events()
            .iter()
            .any(|event| matches!(event.payload(), EventPayload::AgentStarted { .. }))
    );
    let final_state: serde_json::Value =
        serde_json::from_slice(with_context.next_state().as_bytes()).unwrap();
    assert!(final_state["known_agents"].as_array().unwrap().is_empty());

    let following = decode_one(
        r#"{"type":"assistant","uuid":"context-stage-following","sessionId":"session-context-stage","timestamp":"2026-07-17T11:10:02Z","agentId":"context-stage-agent","message":{"content":[]}}"#,
        with_context.next_state(),
    );
    let starts = with_context
        .events()
        .iter()
        .chain(following.events())
        .filter(|event| matches!(event.payload(), EventPayload::AgentStarted { .. }))
        .count();
    assert!(starts <= 1);
}

#[test]
fn staged_start_is_filtered_when_tool_and_spawn_state_evict_its_marker() {
    let pressure = unresolved_pressure_state(64, &[]);
    let request = decode_one(
        r#"{"type":"assistant","uuid":"tool-stage-request","sessionId":"session-tool-stage","timestamp":"2026-07-17T11:20:00Z","agentId":"tool-stage-agent","message":{"content":[{"type":"tool_use","id":"tool-stage-id","name":"Task","input":{"path":"src/lib.rs"}}]}}"#,
        &pressure,
    );
    assert!(
        request
            .events()
            .iter()
            .any(|event| matches!(event.payload(), EventPayload::ActionRequested { .. }))
    );
    assert!(
        !request
            .events()
            .iter()
            .any(|event| matches!(event.payload(), EventPayload::AgentStarted { .. }))
    );
    let final_state: serde_json::Value =
        serde_json::from_slice(request.next_state().as_bytes()).unwrap();
    assert!(final_state["known_agents"].as_array().unwrap().is_empty());
    assert_eq!(final_state["assistant_spawns"].as_array().unwrap().len(), 1);

    let following = decode_one(
        r#"{"type":"assistant","uuid":"tool-stage-following","sessionId":"session-tool-stage","timestamp":"2026-07-17T11:20:01Z","agentId":"tool-stage-agent","message":{"content":[]}}"#,
        request.next_state(),
    );
    let starts = request
        .events()
        .iter()
        .chain(following.events())
        .filter(|event| matches!(event.payload(), EventPayload::AgentStarted { .. }))
        .count();
    assert!(starts <= 1);
}

#[test]
fn staged_multi_agent_starts_only_emit_final_retained_markers() {
    let baseline = decode_one(
        r#"{"type":"assistant","uuid":"multi-stage-baseline","sessionId":"session-multi-stage","timestamp":"2026-07-17T11:30:00Z","agentId":"multi-stage-first","attributionAgent":"multi-stage-second","message":{"content":[]}}"#,
        &DecoderState::default(),
    );
    let expected_ids = baseline
        .events()
        .iter()
        .filter_map(|event| match event.payload() {
            EventPayload::AgentStarted { native_agent_id } => Some(native_agent_id.clone()),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(expected_ids.len(), 2);

    let pressure = unresolved_pressure_state(64, &[]);
    let pressured = decode_one(
        r#"{"type":"assistant","uuid":"multi-stage-pressured","sessionId":"session-multi-stage","timestamp":"2026-07-17T11:30:01Z","agentId":"multi-stage-first","attributionAgent":"multi-stage-second","message":{"content":[]}}"#,
        &pressure,
    );
    let emitted_ids = pressured
        .events()
        .iter()
        .filter_map(|event| match event.payload() {
            EventPayload::AgentStarted { native_agent_id } => Some(native_agent_id.clone()),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(emitted_ids, vec![expected_ids[1].clone()]);

    let following_survivor = decode_one(
        r#"{"type":"assistant","uuid":"multi-stage-following-survivor","sessionId":"session-multi-stage","timestamp":"2026-07-17T11:30:02Z","agentId":"multi-stage-second","message":{"content":[]}}"#,
        pressured.next_state(),
    );
    assert!(
        !following_survivor
            .events()
            .iter()
            .any(|event| matches!(event.payload(), EventPayload::AgentStarted { .. }))
    );
    let following_filtered = decode_one(
        r#"{"type":"assistant","uuid":"multi-stage-following-filtered","sessionId":"session-multi-stage","timestamp":"2026-07-17T11:30:03Z","agentId":"multi-stage-first","message":{"content":[]}}"#,
        following_survivor.next_state(),
    );
    assert!(
        following_filtered
            .events()
            .iter()
            .filter(|event| matches!(event.payload(), EventPayload::AgentStarted { .. }))
            .count()
            <= 1
    );
}

#[test]
fn staged_terminal_events_require_final_matching_finished_markers() {
    let baseline = decode_one(
        r#"{"type":"assistant","uuid":"terminal-stage-baseline","sessionId":"session-terminal-stage","timestamp":"2026-07-17T11:40:00Z","agentId":"terminal-stage-first","attributionAgent":"terminal-stage-second","message":{"content":[]}}"#,
        &DecoderState::default(),
    );
    let known_agents = baseline
        .events()
        .iter()
        .filter_map(|event| match event.payload() {
            EventPayload::AgentStarted { native_agent_id } => Some(native_agent_id.clone()),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(known_agents.len(), 2);
    let pressure = unresolved_pressure_state(50, &known_agents);

    let terminal = decode_one(
        r#"{"type":"result","subtype":"success","uuid":"terminal-stage-result","sessionId":"session-terminal-stage","timestamp":"2026-07-17T11:40:01Z","agentId":"terminal-stage-first","attributionAgent":"terminal-stage-second"}"#,
        &pressure,
    );
    let final_state: serde_json::Value =
        serde_json::from_slice(terminal.next_state().as_bytes()).unwrap();
    let final_finished = final_state["finished_agents"].as_array().unwrap();
    let emitted_finished = terminal
        .events()
        .iter()
        .filter_map(|event| match event.payload() {
            EventPayload::AgentFinished {
                native_agent_id,
                outcome,
            } => Some((native_agent_id, outcome)),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert!(!emitted_finished.is_empty());
    for (agent_id, outcome) in emitted_finished {
        assert!(final_finished.iter().any(|finished| {
            finished["agent_id"] == *agent_id
                && finished["outcome"] == serde_json::to_value(outcome).unwrap()
        }));
    }

    let replay = decode_one(
        r#"{"type":"result","subtype":"failed","uuid":"terminal-stage-replay","sessionId":"session-terminal-stage","timestamp":"2026-07-17T11:40:02Z","agentId":"terminal-stage-first","attributionAgent":"terminal-stage-second"}"#,
        terminal.next_state(),
    );
    assert!(
        !replay
            .events()
            .iter()
            .any(|event| matches!(event.payload(), EventPayload::AgentFinished { .. }))
    );
}

#[test]
fn lifecycle_restarts_only_after_both_bounded_history_markers_are_evicted() {
    let initial_start = decode_one(
        r#"{"type":"assistant","uuid":"window-a-start","sessionId":"session-lifecycle-window","timestamp":"2026-07-17T12:00:00Z","agentId":"window-a","message":{"content":[]}}"#,
        &DecoderState::default(),
    );
    let initial_agent_id = initial_start
        .events()
        .iter()
        .find_map(|event| match event.payload() {
            EventPayload::AgentStarted { native_agent_id } => Some(native_agent_id.clone()),
            _ => None,
        })
        .unwrap();
    let initial_finish = decode_one(
        r#"{"type":"result","subtype":"success","uuid":"window-a-finish","sessionId":"session-lifecycle-window","timestamp":"2026-07-17T12:00:01Z","agentId":"window-a"}"#,
        initial_start.next_state(),
    );
    let mut state = initial_finish.next_state().clone();

    for index in 0..129 {
        let start = decode_one(
            &format!(
                r#"{{"type":"assistant","uuid":"window-pressure-start-{index}","sessionId":"session-lifecycle-window","timestamp":"2026-07-17T12:00:02Z","agentId":"window-pressure-{index}","message":{{"content":[]}}}}"#
            ),
            &state,
        );
        let finish = decode_one(
            &format!(
                r#"{{"type":"result","subtype":"success","uuid":"window-pressure-finish-{index}","sessionId":"session-lifecycle-window","timestamp":"2026-07-17T12:00:03Z","agentId":"window-pressure-{index}"}}"#
            ),
            start.next_state(),
        );
        state = finish.next_state().clone();
    }

    let evicted: serde_json::Value = serde_json::from_slice(state.as_bytes()).unwrap();
    assert!(
        !evicted["known_agents"]
            .as_array()
            .unwrap()
            .iter()
            .any(|known| known == &initial_agent_id)
    );
    assert!(
        !evicted["finished_agents"]
            .as_array()
            .unwrap()
            .iter()
            .any(|finished| finished["agent_id"] == initial_agent_id)
    );

    let finish_without_resight = decode_one(
        r#"{"type":"result","subtype":"failed","uuid":"window-a-stale-finish","sessionId":"session-lifecycle-window","timestamp":"2026-07-17T12:00:04Z","agentId":"window-a"}"#,
        &state,
    );
    assert!(
        !finish_without_resight
            .events()
            .iter()
            .any(|event| matches!(event.payload(), EventPayload::AgentFinished { .. }))
    );

    let restarted = decode_one(
        r#"{"type":"assistant","uuid":"window-a-restart","sessionId":"session-lifecycle-window","timestamp":"2026-07-17T12:00:05Z","agentId":"window-a","message":{"content":[]}}"#,
        finish_without_resight.next_state(),
    );
    assert!(
        restarted
            .events()
            .iter()
            .any(|event| matches!(event.payload(), EventPayload::AgentStarted { .. }))
    );
    let refinished = decode_one(
        r#"{"type":"result","subtype":"success","uuid":"window-a-refinish","sessionId":"session-lifecycle-window","timestamp":"2026-07-17T12:00:06Z","agentId":"window-a"}"#,
        restarted.next_state(),
    );
    assert!(
        refinished
            .events()
            .iter()
            .any(|event| matches!(event.payload(), EventPayload::AgentFinished { .. }))
    );
}

#[test]
fn finished_history_eviction_allows_one_bounded_window_reclose() {
    let mut state = DecoderState::default();
    for index in 0..127 {
        let started = decode_one(
            &format!(
                r#"{{"type":"assistant","uuid":"reclose-older-start-{index}","sessionId":"session-reclose-window","timestamp":"2026-07-17T12:30:00Z","agentId":"reclose-older-{index}","message":{{"content":[]}}}}"#
            ),
            &state,
        );
        state = started.next_state().clone();
    }
    let target_start = decode_one(
        r#"{"type":"assistant","uuid":"reclose-target-start","sessionId":"session-reclose-window","timestamp":"2026-07-17T12:30:01Z","agentId":"reclose-target","message":{"content":[]}}"#,
        &state,
    );
    let target_agent_id = target_start
        .events()
        .iter()
        .find_map(|event| match event.payload() {
            EventPayload::AgentStarted { native_agent_id } => Some(native_agent_id.clone()),
            _ => None,
        })
        .unwrap();
    let target_finish = decode_one(
        r#"{"type":"result","subtype":"success","uuid":"reclose-target-finish","sessionId":"session-reclose-window","timestamp":"2026-07-17T12:30:02Z","agentId":"reclose-target"}"#,
        target_start.next_state(),
    );
    state = target_finish.next_state().clone();

    for index in 0..127 {
        let finished = decode_one(
            &format!(
                r#"{{"type":"result","subtype":"success","uuid":"reclose-older-finish-{index}","sessionId":"session-reclose-window","timestamp":"2026-07-17T12:30:03Z","agentId":"reclose-older-{index}"}}"#
            ),
            &state,
        );
        state = finished.next_state().clone();
    }
    let pressure_start = decode_one(
        r#"{"type":"assistant","uuid":"reclose-pressure-start","sessionId":"session-reclose-window","timestamp":"2026-07-17T12:30:04Z","agentId":"reclose-pressure","message":{"content":[]}}"#,
        &state,
    );
    let pressure_finish = decode_one(
        r#"{"type":"result","subtype":"success","uuid":"reclose-pressure-finish","sessionId":"session-reclose-window","timestamp":"2026-07-17T12:30:05Z","agentId":"reclose-pressure"}"#,
        pressure_start.next_state(),
    );

    let evicted: serde_json::Value =
        serde_json::from_slice(pressure_finish.next_state().as_bytes()).unwrap();
    assert!(
        evicted["known_agents"]
            .as_array()
            .unwrap()
            .iter()
            .any(|known| known == &target_agent_id)
    );
    assert!(
        !evicted["finished_agents"]
            .as_array()
            .unwrap()
            .iter()
            .any(|finished| finished["agent_id"] == target_agent_id)
    );

    let reclosed = decode_one(
        r#"{"type":"result","subtype":"failed","uuid":"reclose-target-again","sessionId":"session-reclose-window","timestamp":"2026-07-17T12:30:06Z","agentId":"reclose-target"}"#,
        pressure_finish.next_state(),
    );
    assert!(
        reclosed
            .events()
            .iter()
            .any(|event| matches!(event.payload(), EventPayload::AgentFinished { .. }))
    );
    let replay = decode_one(
        r#"{"type":"result","subtype":"cancelled","uuid":"reclose-target-replay","sessionId":"session-reclose-window","timestamp":"2026-07-17T12:30:07Z","agentId":"reclose-target"}"#,
        reclosed.next_state(),
    );
    assert!(
        !replay
            .events()
            .iter()
            .any(|event| matches!(event.payload(), EventPayload::AgentFinished { .. }))
    );
}

#[test]
fn assistant_error_validation_drains_both_locations_before_emitting() {
    let pending = decode_one(
        r#"{"type":"assistant","uuid":"error-validation-request","sessionId":"session-error-validation","timestamp":"2026-07-17T13:00:00Z","message":{"content":[{"type":"tool_use","id":"rollback-tool","name":"Read","input":{"path":"src/lib.rs"}}]}}"#,
        &DecoderState::default(),
    );
    let oversized_nested = "x".repeat(129);
    let invalid_records = [
        r#"{"type":"assistant","uuid":"error-duplicate-nested","sessionId":"session-error-validation","timestamp":"2026-07-17T13:00:01Z","error":"top","message":{"error":"nested-one","error":"nested-two","content":[]}}"#.to_owned(),
        r#"{"type":"assistant","uuid":"error-container-nested","sessionId":"session-error-validation","timestamp":"2026-07-17T13:00:02Z","error":"top","message":{"error":{"code":"nested"},"content":[]}}"#.to_owned(),
        format!(
            r#"{{"type":"assistant","uuid":"error-oversized-nested","sessionId":"session-error-validation","timestamp":"2026-07-17T13:00:03Z","error":"top","message":{{"error":"{oversized_nested}","content":[]}}}}"#
        ),
    ];

    for invalid in invalid_records {
        let error = try_decode(&invalid, pending.next_state()).unwrap_err();
        assert!(matches!(error, DecodeError::Malformed(_)));

        let recovery = decode_one(
            r#"{"type":"user","uuid":"error-validation-recovery","sessionId":"session-error-validation","timestamp":"2026-07-17T13:00:04Z","message":{"content":[{"type":"tool_result","tool_use_id":"rollback-tool","content":"ok","is_error":false}]}}"#,
            pending.next_state(),
        );
        assert!(
            recovery
                .events()
                .iter()
                .any(|event| matches!(event.payload(), EventPayload::ActionFinished { .. }))
        );
    }
}
