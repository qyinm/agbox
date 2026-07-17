#![allow(clippy::unwrap_used)]

use agbox_adapters::{
    ClaudeAdapter, DecodeContext, DecodeDisposition, DecoderState, MemoryRecordSource,
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

fn decode_one(json: &str, state: &DecoderState) -> agbox_adapters::DecodedRecord {
    ClaudeAdapter
        .decode(
            &MemoryRecordSource::new(json.as_bytes().to_vec()),
            &context(),
            state,
        )
        .unwrap()
}

#[test]
fn claude_parent_and_subagent_links_are_not_flattened() {
    let records = decode_fixture_file("claude", fixture("sidechain.jsonl")).unwrap();
    let events = records
        .iter()
        .flat_map(agbox_adapters::DecodedRecord::events)
        .collect::<Vec<_>>();

    let started = events
        .iter()
        .find(|event| {
            matches!(
                event.payload(),
                EventPayload::AgentStarted { native_agent_id } if native_agent_id == "agent-a"
            )
        })
        .unwrap();
    assert_eq!(started.correlation_id(), Some("spawn-assistant-a"));
    assert_eq!(started.causation_id(), Some("parent-a1"));
    assert_eq!(started.turn_id(), Some("child-a1"));

    for record in &records[1..4] {
        let expected_parent = match record.observation().source().native_record_id().unwrap() {
            "child-a1" => "parent-a1",
            "child-compact" => "child-a1",
            "child-finish" => "child-compact",
            other => panic!("unexpected child record {other}"),
        };
        assert!(!record.events().is_empty());
        assert!(
            record
                .events()
                .iter()
                .all(|event| event.causation_id() == Some(expected_parent))
        );
    }

    assert!(events.iter().any(|event| matches!(
        event.payload(),
        EventPayload::ContextCompacted {
            summary_hash: Some(_)
        }
    )));
    assert!(events.iter().any(|event| matches!(
        event.payload(),
        EventPayload::AgentFinished {
            native_agent_id,
            outcome: ActionOutcome::Succeeded
        } if native_agent_id == "agent-a"
    )));
    assert!(events.iter().any(|event| matches!(
        event.payload(),
        EventPayload::TurnFinished {
            outcome: ActionOutcome::Succeeded
        }
    )));
    assert!(events.iter().any(|event| matches!(
        event.payload(),
        EventPayload::AgentFinished {
            native_agent_id,
            outcome: ActionOutcome::Failed
        } if native_agent_id == "agent-a"
    )));

    let child_message = records[1]
        .events()
        .iter()
        .find(|event| matches!(event.payload(), EventPayload::MessageCreated { .. }))
        .unwrap();
    let parent_request = records[0]
        .events()
        .iter()
        .find(|event| matches!(event.payload(), EventPayload::ActionRequested { .. }))
        .unwrap();
    assert_ne!(child_message.event_id(), parent_request.event_id());
    assert_ne!(child_message.turn_id(), parent_request.turn_id());
}

#[test]
fn system_summaries_and_assistant_errors_are_private_bounded_diagnostics() {
    let records = decode_fixture_file("claude", fixture("sidechain.jsonl")).unwrap();
    for record in &records[4..] {
        let diagnostic = record
            .events()
            .iter()
            .find(|event| matches!(event.payload(), EventPayload::DiagnosticObserved { .. }))
            .unwrap();
        assert_eq!(diagnostic.privacy(), PrivacyLabel::PrivateLocal);
    }

    let serialized = serde_json::to_string(
        &records
            .iter()
            .flat_map(agbox_adapters::DecodedRecord::events)
            .collect::<Vec<_>>(),
    )
    .unwrap();
    let debug = format!("{records:?}");
    let evidence = records
        .iter()
        .flat_map(agbox_adapters::DecodedRecord::evidence)
        .flat_map(|evidence| evidence.plaintext.iter().copied())
        .collect::<Vec<_>>();
    let state = records.last().unwrap().next_state().as_bytes();
    for forbidden in [
        "/Users/alice/private",
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
fn malformed_and_unknown_records_are_visible_and_isolated() {
    let malformed = decode_fixture_file("claude", fixture("malformed.jsonl")).unwrap();
    assert_eq!(malformed.len(), 2);
    assert!(matches!(
        malformed[0].disposition(),
        DecodeDisposition::Malformed { .. }
    ));
    assert_eq!(
        malformed[0].disposition().class(),
        Some("missing_required_identity")
    );
    assert!(malformed[0].events().is_empty());
    assert!(malformed[0].evidence().is_empty());
    assert!(!malformed[1].events().is_empty());

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
fn known_agents_are_historical_bounded_and_evict_at_128() {
    let mut state = DecoderState::default();
    for index in 0..129 {
        let decoded = decode_one(
            &format!(
                r#"{{"type":"assistant","uuid":"child-{index}","parentUuid":"parent-{index}","sessionId":"session-agents","timestamp":"2026-07-17T06:00:00Z","isSidechain":true,"agentId":"agent-{index}","sourceToolAssistantUUID":"spawn-{index}","message":{{"content":"done"}}}}"#
            ),
            &state,
        );
        assert!(decoded.events().iter().any(|event| matches!(
            event.payload(),
            EventPayload::AgentStarted { native_agent_id } if native_agent_id == &format!("agent-{index}")
        )));
        state = decoded.next_state().clone();
        assert!(state.as_bytes().len() <= 32 * 1024);
    }

    let value: serde_json::Value = serde_json::from_slice(state.as_bytes()).unwrap();
    let agents = value["known_agents"].as_array().unwrap();
    assert_eq!(agents.len(), 128);
    assert_eq!(agents.first().unwrap(), "agent-1");
    assert_eq!(agents.last().unwrap(), "agent-128");

    let terminal = decode_one(
        r#"{"type":"system","subtype":"turn_duration","uuid":"finished-128","sessionId":"session-agents","timestamp":"2026-07-17T06:00:01Z","agentId":"agent-128","durationMs":1}"#,
        &state,
    );
    assert!(terminal.events().iter().any(|event| matches!(
        event.payload(),
        EventPayload::AgentFinished { native_agent_id, .. } if native_agent_id == "agent-128"
    )));
    let terminal_state: serde_json::Value =
        serde_json::from_slice(terminal.next_state().as_bytes()).unwrap();
    assert_eq!(
        terminal_state["known_agents"].as_array().unwrap().len(),
        128
    );
    assert!(
        terminal_state["known_agents"]
            .as_array()
            .unwrap()
            .iter()
            .any(|agent| agent == "agent-128")
    );
}
