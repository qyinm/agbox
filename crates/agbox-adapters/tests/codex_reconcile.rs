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
        2
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
        "PRIVATE_OTHER_AGENT",
        "PRIVATE_TOKEN",
        "PRIVATE_DELEGATED_PROMPT",
        "PRIVATE_REASONING",
        "PRIVATE_INTER_AGENT_MESSAGE",
        "PRIVATE_MESSAGE_REASONING",
        "PRIVATE_METADATA_PROMPT",
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
fn flattened_producer_communication_maps_parent_correlated_hash_only_message() {
    let decoded = decode_one(
        r#"{"timestamp":"2026-07-17T03:00:00Z","type":"inter_agent_communication","payload":{"id":"PRIVATE_MESSAGE_ID","author":"/root/PRIVATE_AUTHOR_/Users/alice","recipient":"/root/PRIVATE_RECIPIENT_/Users/alice","other_recipients":["/root/PRIVATE_OTHER"],"content":"PRIVATE_FLATTENED_MESSAGE token=PRIVATE_TOKEN /Users/alice","encrypted_content":"QUJDREVGR0hJSktMTU5PUFFSU1RVVldYWVo=","trigger_turn":true,"future":{"reasoning":"PRIVATE_ADDITIVE"}}}"#,
        &DecoderState::default(),
    );
    assert!(matches!(decoded.disposition(), DecodeDisposition::Known));
    let message = decoded
        .events()
        .iter()
        .find(|event| matches!(event.payload(), EventPayload::MessageCreated { .. }))
        .unwrap();
    assert_eq!(message.actor(), Actor::Agent);
    assert!(message.correlation_id().is_some());
    assert!(message.causation_id().is_some());
    assert_ne!(message.correlation_id(), message.causation_id());
    let EventPayload::MessageCreated { content } = message.payload() else {
        unreachable!();
    };
    assert_eq!(
        content.hash(),
        blake3::hash(b"PRIVATE_FLATTENED_MESSAGE token=PRIVATE_TOKEN /Users/alice")
            .to_hex()
            .as_str()
    );
    assert_eq!(
        content.byte_length(),
        "PRIVATE_FLATTENED_MESSAGE token=PRIVATE_TOKEN /Users/alice".len() as u64
    );
    assert!(content.redacted_excerpt().is_none());
    assert!(content.local_locator().is_none());
    assert!(decoded.evidence().is_empty());
    let serialized = serde_json::to_string(decoded.events()).unwrap();
    let state = String::from_utf8(decoded.next_state().as_bytes().to_vec()).unwrap();
    let debug = format!("{decoded:?}");
    for forbidden in [
        "PRIVATE_MESSAGE_ID",
        "PRIVATE_AUTHOR",
        "PRIVATE_RECIPIENT",
        "PRIVATE_OTHER",
        "PRIVATE_FLATTENED_MESSAGE",
        "PRIVATE_TOKEN",
        "PRIVATE_ADDITIVE",
        "/Users/alice",
        "QUJDREVGR0hJSktMTU5PUFFSU1RVVldYWVo=",
    ] {
        assert!(!serialized.contains(forbidden));
        assert!(!state.contains(forbidden));
        assert!(!debug.contains(forbidden));
    }
}

#[test]
fn flattened_encrypted_communication_hashes_ciphertext_without_retaining_it() {
    let ciphertext = "QUJDREVGR0hJSktMTU5PUFFSU1RVVldYWVo=";
    let decoded = decode_one(
        &format!(
            r#"{{"type":"inter_agent_communication","payload":{{"author":"parent","recipient":"child","content":"","encrypted_content":"{ciphertext}","trigger_turn":false}}}}"#
        ),
        &DecoderState::default(),
    );
    let content = decoded
        .events()
        .iter()
        .find_map(|event| match event.payload() {
            EventPayload::MessageCreated { content } => Some(content),
            _ => None,
        })
        .unwrap();
    assert_eq!(
        content.hash(),
        blake3::hash(ciphertext.as_bytes()).to_hex().as_str()
    );
    assert_eq!(content.byte_length(), ciphertext.len() as u64);
    assert_eq!(content.media_type(), "application/octet-stream");
    assert!(content.redacted_excerpt().is_none());
    assert!(decoded.evidence().is_empty());
    assert!(
        !serde_json::to_string(decoded.events())
            .unwrap()
            .contains(ciphertext)
    );
    assert!(!format!("{decoded:?}").contains(ciphertext));
}

#[test]
fn flattened_metadata_stages_one_start_finish_and_suppresses_replay() {
    let mut state = DecoderState::default();
    let records = [
        r#"{"type":"inter_agent_communication_metadata","payload":{"agent_thread_id":"PRIVATE_FLAT_AGENT","parent_thread_id":"PRIVATE_FLAT_PARENT","kind":"started","trigger_turn":true}}"#,
        r#"{"type":"inter_agent_communication_metadata","payload":{"agent_thread_id":"PRIVATE_FLAT_AGENT","parent_thread_id":"PRIVATE_FLAT_PARENT","kind":"started","trigger_turn":true}}"#,
        r#"{"type":"inter_agent_communication_metadata","payload":{"agent_thread_id":"PRIVATE_FLAT_AGENT","parent_thread_id":"PRIVATE_FLAT_PARENT","kind":"completed","trigger_turn":false}}"#,
        r#"{"type":"inter_agent_communication_metadata","payload":{"agent_thread_id":"PRIVATE_FLAT_AGENT","parent_thread_id":"PRIVATE_FLAT_PARENT","kind":"failed","trigger_turn":false}}"#,
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
    assert!(
        records
            .iter()
            .flat_map(agbox_adapters::DecodedRecord::events)
            .all(|event| event.causation_id().is_some() && event.correlation_id().is_some())
    );
    let combined = format!("{records:?}{}", String::from_utf8_lossy(state.as_bytes()));
    assert!(!combined.contains("PRIVATE_FLAT_AGENT"));
    assert!(!combined.contains("PRIVATE_FLAT_PARENT"));
}

#[test]
fn real_sub_agent_activity_paths_emit_opaque_parent_correlated_lifecycle() {
    let started = decode_one(
        r#"{"type":"event_msg","payload":{"type":"sub_agent_activity","event_id":"call_PRIVATE_SPAWN","occurred_at_ms":1783686676512,"agent_thread_id":"PRIVATE_REAL_THREAD","agent_path":"/root/PRIVATE_REAL_PATH","kind":"started"}}"#,
        &DecoderState::default(),
    );
    let event = started
        .events()
        .iter()
        .find(|event| matches!(event.payload(), EventPayload::AgentStarted { .. }))
        .unwrap();
    assert!(event.correlation_id().is_some());
    assert!(event.causation_id().is_some());
    let EventPayload::AgentStarted { native_agent_id } = event.payload() else {
        unreachable!();
    };
    assert!(native_agent_id.starts_with("codex_graph_"));
    let serialized = serde_json::to_string(started.events()).unwrap();
    let state = String::from_utf8(started.next_state().as_bytes().to_vec()).unwrap();
    let debug = format!("{started:?}");
    for forbidden in [
        "PRIVATE_SPAWN",
        "PRIVATE_REAL_THREAD",
        "PRIVATE_REAL_PATH",
        "/root/",
    ] {
        assert!(!serialized.contains(forbidden));
        assert!(!state.contains(forbidden));
        assert!(!debug.contains(forbidden));
    }
}

#[test]
fn real_trigger_only_metadata_is_known_but_does_not_invent_lifecycle() {
    for trigger_turn in [true, false] {
        let decoded = decode_one(
            &format!(
                r#"{{"type":"inter_agent_communication_metadata","payload":{{"trigger_turn":{trigger_turn}}}}}"#
            ),
            &DecoderState::default(),
        );
        assert!(matches!(decoded.disposition(), DecodeDisposition::Known));
        assert!(decoded.events().is_empty());
        assert!(decoded.evidence().is_empty());
    }
}

#[test]
fn flattened_malformed_semantics_quarantine_then_valid_ordinal_recovers() {
    let first = decode_one(
        r#"{"timestamp":"2026-07-17T03:00:00Z","ordinal":0,"type":"session_meta","payload":{"history_mode":"paginated"}}"#,
        &DecoderState::default(),
    );
    let malformed = decode_one(
        r#"{"timestamp":"2026-07-17T03:00:01Z","ordinal":1,"type":"inter_agent_communication","payload":{"author":"parent","recipient":"one","recipient":{"secret":"PRIVATE_HETEROGENEOUS_RECIPIENT"},"content":"PRIVATE_MALFORMED_MESSAGE","trigger_turn":true}}"#,
        first.next_state(),
    );
    assert!(matches!(
        malformed.disposition(),
        DecodeDisposition::Malformed { .. }
    ));
    assert!(malformed.events().is_empty());
    let progressed: serde_json::Value =
        serde_json::from_slice(malformed.next_state().as_bytes()).unwrap();
    assert_eq!(progressed["last_ordinal"], 1);
    assert!(!format!("{malformed:?}").contains("PRIVATE_HETEROGENEOUS_RECIPIENT"));
    assert!(!format!("{malformed:?}").contains("PRIVATE_MALFORMED_MESSAGE"));

    let recovered = decode_one(
        r#"{"timestamp":"2026-07-17T03:00:02Z","ordinal":2,"type":"inter_agent_communication","payload":{"author":"parent","recipient":"child","content":"valid recovery","trigger_turn":true}}"#,
        malformed.next_state(),
    );
    assert!(matches!(recovered.disposition(), DecodeDisposition::Known));
    assert!(
        recovered
            .events()
            .iter()
            .any(|event| matches!(event.payload(), EventPayload::MessageCreated { .. }))
    );
}

#[test]
fn flattened_additive_fields_only_change_schema_fingerprint() {
    let baseline = decode_one(
        r#"{"type":"inter_agent_communication","payload":{"author":"parent","recipient":"child","content":"message","trigger_turn":true}}"#,
        &DecoderState::default(),
    );
    let additive = decode_one(
        r#"{"type":"inter_agent_communication","payload":{"author":"parent","recipient":"child","content":"message","trigger_turn":true,"future":{"reasoning":"PRIVATE_FUTURE_REASONING","path":"/Users/alice/private"}}}"#,
        &DecoderState::default(),
    );
    assert_ne!(
        baseline.observation().schema_fingerprint(),
        additive.observation().schema_fingerprint()
    );
    let message_projection = |record: &agbox_adapters::DecodedRecord| {
        record
            .events()
            .iter()
            .find_map(|event| match event.payload() {
                EventPayload::MessageCreated { content } => Some((
                    event.correlation_id().map(str::to_owned),
                    event.causation_id().map(str::to_owned),
                    content.hash().to_owned(),
                    content.byte_length(),
                )),
                _ => None,
            })
    };
    assert_eq!(message_projection(&baseline), message_projection(&additive));
    let additive_debug = format!("{additive:?}");
    assert!(!additive_debug.contains("PRIVATE_FUTURE_REASONING"));
    assert!(!additive_debug.contains("/Users/alice"));
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
        r#"{"timestamp":"2026-07-17T03:00:01Z","ordinal":1,"type":"inter_agent_communication","payload":{"author":null,"recipient":"child","content":"PRIVATE_NULL_AUTHOR","trigger_turn":true}}"#,
        r#"{"timestamp":"2026-07-17T03:00:01Z","ordinal":1,"type":"inter_agent_communication_metadata","payload":{"agent_thread_id":null,"parent_thread_id":"parent","kind":"started","trigger_turn":true}}"#,
        r#"{"timestamp":"2026-07-17T03:00:01Z","ordinal":1,"type":"inter_agent_communication_metadata","payload":{"agent_thread_id":"agent","parent_thread_id":"parent","kind":{"future":true},"trigger_turn":true}}"#,
        r#"{"timestamp":"2026-07-17T03:00:01Z","ordinal":1,"type":"inter_agent_communication_metadata","payload":{"agent_thread_id":"agent","parent_thread_id":"parent","kind":"started","kind":"completed","trigger_turn":true}}"#,
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
        assert!(!format!("{isolated:?}").contains("PRIVATE_NULL_AUTHOR"));
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
        r#"{"timestamp":"2026-07-17T03:00:01Z","ordinal":1,"type":"future_inter_agent_communication","payload":{"author":{"token":"PRIVATE_UNKNOWN_AGENT"},"recipient":["PRIVATE_UNKNOWN_PARENT"],"content":{"reasoning":"PRIVATE_UNKNOWN_REASONING"}}}"#,
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
