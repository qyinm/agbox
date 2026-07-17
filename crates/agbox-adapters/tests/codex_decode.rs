#![allow(clippy::unwrap_used)]

use std::path::Path;

use agbox_adapters::{
    CodexAdapter, DecodeContext, DecodeDisposition, DecoderState, MemoryRecordSource, RootClass,
    SourceAdapter, adapters, test_support::decode_fixture_file,
};
use agbox_core::{EventPayload, ProjectId, Provider};
use time::OffsetDateTime;

fn context() -> DecodeContext {
    DecodeContext {
        project_id: ProjectId::for_test("project_fixture"),
        project_root: Some("/fixture/project".into()),
        source_id: "source_fixture".to_owned(),
        observed_at: OffsetDateTime::UNIX_EPOCH,
        source_generation: 7,
        format: "codex-rollout-1".to_owned(),
    }
}

fn decode_one(
    json: &str,
    state: &DecoderState,
) -> Result<agbox_adapters::DecodedRecord, agbox_adapters::DecodeError> {
    CodexAdapter.decode(
        &MemoryRecordSource::new(json.as_bytes().to_vec()),
        &context(),
        state,
    )
}

fn decode_lines(fixture: &str) -> Vec<agbox_adapters::DecodedRecord> {
    let mut state = DecoderState::default();
    fixture
        .lines()
        .map(|line| {
            let decoded = decode_one(line, &state).unwrap();
            state = decoded.next_state().clone();
            decoded
        })
        .collect()
}

fn finished_count(record: &agbox_adapters::DecodedRecord) -> usize {
    record
        .events()
        .iter()
        .filter(|event| matches!(event.payload(), EventPayload::ActionFinished { .. }))
        .count()
}

#[test]
fn legacy_rollout_uses_response_inputs_and_terminal_event_results() {
    let records = decode_fixture_file("codex", "tests/fixtures/codex/legacy.jsonl").unwrap();
    let events = records
        .iter()
        .flat_map(agbox_adapters::DecodedRecord::events)
        .collect::<Vec<_>>();
    assert!(events.iter().any(|event| matches!(
        event.payload(),
        EventPayload::ActionRequested { native_action_id, .. } if native_action_id == "call-1"
    )));
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(
                event.payload(),
                EventPayload::ActionFinished { native_action_id, .. }
                    if native_action_id == "call-1"
            ))
            .count(),
        1
    );
}

#[test]
fn paginated_rollout_prefers_item_completed_and_emits_artifact_change() {
    let records = decode_fixture_file("codex", "tests/fixtures/codex/paginated.jsonl").unwrap();
    let events = records
        .iter()
        .flat_map(agbox_adapters::DecodedRecord::events)
        .collect::<Vec<_>>();
    assert!(
        events
            .iter()
            .any(|event| matches!(event.payload(), EventPayload::ArtifactChanged { .. }))
    );
    assert!(
        events
            .iter()
            .any(|event| matches!(event.payload(), EventPayload::ContextCompacted { .. }))
    );
}

#[test]
fn registry_roots_matcher_and_dates_are_codex_specific() {
    assert_eq!(
        adapters()
            .iter()
            .map(|adapter| adapter.provider())
            .collect::<Vec<_>>(),
        vec![Provider::Claude, Provider::Codex]
    );
    let roots = CodexAdapter.roots(Path::new("/home/test"));
    assert_eq!(roots.len(), 2);
    assert_eq!(roots[0].class, RootClass::Active);
    assert!(roots[0].path.ends_with(".codex/sessions"));
    assert_eq!(roots[1].class, RootClass::Archive);
    assert!(roots[1].path.ends_with(".codex/archived_sessions"));
    assert!(CodexAdapter.matches(&roots[0], Path::new("2026/07/17/rollout.jsonl")));
    assert!(!CodexAdapter.matches(&roots[0], Path::new("../rollout.jsonl")));
    assert!(!CodexAdapter.matches(&roots[0], Path::new("rollout.json")));
    assert!(
        CodexAdapter
            .trusted_session_time(
                &roots[0],
                Path::new("2026/07/17/rollout.jsonl"),
                OffsetDateTime::UNIX_EPOCH,
            )
            .is_some()
    );
    assert!(
        CodexAdapter
            .trusted_session_time(
                &roots[0],
                Path::new("backup/2026/07/17/rollout.jsonl"),
                OffsetDateTime::UNIX_EPOCH,
            )
            .is_none()
    );
    assert!(
        CodexAdapter
            .trusted_session_time(
                &roots[1],
                Path::new("rollout-2026-07-17.jsonl"),
                OffsetDateTime::UNIX_EPOCH,
            )
            .is_some()
    );
    assert!(
        CodexAdapter
            .trusted_session_time(
                &roots[1],
                Path::new("backup/rollout-2026-07-17.jsonl"),
                OffsetDateTime::UNIX_EPOCH,
            )
            .is_none()
    );
}

#[test]
fn explicit_paginated_mode_cannot_be_downgraded() {
    let first = decode_one(
        r#"{"timestamp":"2026-07-17T03:00:00Z","ordinal":0,"type":"session_meta","payload":{"history_mode":"paginated"}}"#,
        &DecoderState::default(),
    )
    .unwrap();
    let second = decode_one(
        r#"{"timestamp":"2026-07-17T03:00:01Z","ordinal":1,"type":"session_meta","payload":{"history_mode":"legacy"}}"#,
        first.next_state(),
    )
    .unwrap();
    let state: serde_json::Value = serde_json::from_slice(second.next_state().as_bytes()).unwrap();
    assert_eq!(state["history_mode"], "paginated");
}

#[test]
fn result_precedence_upgrades_rank_without_duplicate_finish() {
    let records = decode_lines(
        r#"{"timestamp":"2026-07-17T03:00:00Z","ordinal":0,"type":"session_meta","payload":{"history_mode":"paginated"}}
{"timestamp":"2026-07-17T03:00:01Z","ordinal":1,"type":"response_item","payload":{"type":"function_call","name":"exec_command","arguments":"{}","call_id":"call-1"}}
{"timestamp":"2026-07-17T03:00:02Z","ordinal":2,"type":"event_msg","payload":{"type":"exec_command_end","call_id":"call-1","exit_code":0,"stdout":"fallback"}}
{"timestamp":"2026-07-17T03:00:03Z","ordinal":3,"type":"response_item","payload":{"type":"function_call_output","call_id":"call-1","output":"response"}}
{"timestamp":"2026-07-17T03:00:04Z","ordinal":4,"type":"event_msg","payload":{"type":"item_completed","item":{"type":"command_execution","call_id":"call-1","status":"completed","output":"typed"}}}"#,
    );
    assert_eq!(records.iter().map(finished_count).sum::<usize>(), 1);
    let state: serde_json::Value =
        serde_json::from_slice(records.last().unwrap().next_state().as_bytes()).unwrap();
    assert_eq!(state["completed_semantic_keys"][0]["rank"], 3);
}

#[test]
fn legacy_item_completed_rank_zero_does_not_finish_a_call() {
    let records = decode_lines(
        r#"{"timestamp":"2026-07-17T02:00:00Z","type":"session_meta","payload":{}}
{"timestamp":"2026-07-17T02:00:01Z","type":"response_item","payload":{"type":"function_call","name":"exec_command","arguments":"{}","call_id":"call-1"}}
{"timestamp":"2026-07-17T02:00:02Z","type":"event_msg","payload":{"type":"item_completed","item":{"type":"command_execution","call_id":"call-1","status":"completed"}}}"#,
    );
    assert_eq!(records.iter().map(finished_count).sum::<usize>(), 0);
}

#[test]
fn higher_rank_first_suppresses_later_lower_rank_and_replay() {
    let records = decode_lines(
        r#"{"timestamp":"2026-07-17T03:00:00Z","ordinal":0,"type":"session_meta","payload":{"history_mode":"paginated"}}
{"timestamp":"2026-07-17T03:00:01Z","ordinal":1,"type":"response_item","payload":{"type":"function_call","name":"exec_command","arguments":"{}","call_id":"call-1"}}
{"timestamp":"2026-07-17T03:00:02Z","ordinal":2,"type":"event_msg","payload":{"type":"item_completed","item":{"type":"command_execution","call_id":"call-1","status":"completed"}}}
{"timestamp":"2026-07-17T03:00:03Z","ordinal":3,"type":"response_item","payload":{"type":"function_call_output","call_id":"call-1","output":"late"}}"#,
    );
    assert_eq!(records.iter().map(finished_count).sum::<usize>(), 1);

    let legacy = decode_lines(
        r#"{"timestamp":"2026-07-17T02:00:00Z","type":"session_meta","payload":{}}
{"timestamp":"2026-07-17T02:00:01Z","type":"response_item","payload":{"type":"function_call","name":"exec_command","arguments":"{}","call_id":"call-1"}}
{"timestamp":"2026-07-17T02:00:02Z","type":"response_item","payload":{"type":"function_call_output","call_id":"call-1","output":"one"}}
{"timestamp":"2026-07-17T02:00:03Z","type":"response_item","payload":{"type":"function_call_output","call_id":"call-1","output":"replay"}}"#,
    );
    assert_eq!(legacy.iter().map(finished_count).sum::<usize>(), 1);
}

#[test]
fn unknown_variant_advances_paginated_ordinal_and_valid_record_recovers() {
    let records = decode_lines(
        r#"{"timestamp":"2026-07-17T03:00:00Z","ordinal":0,"type":"session_meta","payload":{"history_mode":"paginated"}}
{"timestamp":"2026-07-17T03:00:01Z","ordinal":1,"type":"response_item","payload":{"type":"future_item","secret":"do-not-parse"}}
{"timestamp":"2026-07-17T03:00:02Z","ordinal":2,"type":"compacted","payload":{"replacement_history":[]}}"#,
    );
    assert!(matches!(
        records[1].disposition(),
        DecodeDisposition::UnknownType { .. }
    ));
    assert!(
        records[2]
            .events()
            .iter()
            .any(|event| matches!(event.payload(), EventPayload::ContextCompacted { .. }))
    );
}

#[test]
fn reasoning_world_state_and_identityless_known_items_emit_no_activity() {
    let records = decode_lines(
        r#"{"timestamp":"2026-07-17T02:00:00Z","type":"session_meta","payload":{}}
{"timestamp":"2026-07-17T02:00:01Z","type":"response_item","payload":{"type":"reasoning","summary":"PRIVATE_REASONING"}}
{"timestamp":"2026-07-17T02:00:02Z","type":"world_state","payload":{"content":"PRIVATE_STATE"}}
{"timestamp":"2026-07-17T02:00:03Z","type":"response_item","payload":{"type":"web_search_call","query":"unstable"}}"#,
    );
    for record in &records[1..] {
        assert!(record.events().is_empty());
        assert!(record.evidence().is_empty());
        let debug = format!("{record:?}");
        assert!(!debug.contains("PRIVATE_REASONING"));
        assert!(!debug.contains("PRIVATE_STATE"));
    }
}

#[test]
fn duplicate_and_invalid_selected_fields_are_isolated_and_rollback_state() {
    let first = decode_one(
        r#"{"timestamp":"2026-07-17T03:00:00Z","ordinal":0,"type":"session_meta","payload":{"history_mode":"paginated"}}"#,
        &DecoderState::default(),
    )
    .unwrap();
    for malformed in [
        r#"{"timestamp":"2026-07-17T03:00:01Z","ordinal":1,"type":"response_item","payload":{"type":"message","type":"reasoning","role":"user","content":"x"}}"#,
        r#"{"timestamp":"2026-07-17T03:00:01Z","ordinal":1,"type":"response_item","payload":{"type":null}}"#,
        r#"{"timestamp":"2026-07-17T03:00:01Z","ordinal":1,"type":"response_item","payload":{"type":7}}"#,
    ] {
        let decoded = decode_one(malformed, first.next_state()).unwrap();
        assert!(matches!(
            decoded.disposition(),
            DecodeDisposition::Malformed { .. }
        ));
        assert_eq!(decoded.next_state(), first.next_state());
        assert!(decoded.events().is_empty());
    }
}

#[test]
fn ordinal_gap_is_malformed_and_rolls_back() {
    let first = decode_one(
        r#"{"timestamp":"2026-07-17T03:00:00Z","ordinal":0,"type":"session_meta","payload":{"history_mode":"paginated"}}"#,
        &DecoderState::default(),
    )
    .unwrap();
    let gap = decode_one(
        r#"{"timestamp":"2026-07-17T03:00:02Z","ordinal":2,"type":"compacted","payload":{}}"#,
        first.next_state(),
    )
    .unwrap();
    assert!(matches!(
        gap.disposition(),
        DecodeDisposition::Malformed { .. }
    ));
    assert_eq!(gap.next_state(), first.next_state());
    assert!(gap.events().is_empty());
}

#[test]
fn privacy_boundary_omits_raw_paths_tokens_base64_reasoning_stdout_and_arguments() {
    let base64 = "QUJD".repeat(80);
    let fixture = format!(
        r#"{{"timestamp":"2026-07-17T02:00:00Z","type":"session_meta","payload":{{}}}}
{{"timestamp":"2026-07-17T02:00:01Z","type":"response_item","payload":{{"type":"function_call","name":"exec_command","arguments":"{{\"cmd\":\"cat /Users/private/secret\",\"token\":\"sk-super-secret\",\"image\":\"{base64}\"}}","call_id":"call-1"}}}}
{{"timestamp":"2026-07-17T02:00:02Z","type":"event_msg","payload":{{"type":"exec_command_end","call_id":"call-1","exit_code":0,"stdout":"sk-output-secret /Users/private/output"}}}}
{{"timestamp":"2026-07-17T02:00:03Z","type":"response_item","payload":{{"type":"reasoning","summary":"PRIVATE_CHAIN_OF_THOUGHT"}}}}"#
    );
    let records = decode_lines(&fixture);
    let event_json = serde_json::to_string(
        &records
            .iter()
            .flat_map(agbox_adapters::DecodedRecord::events)
            .collect::<Vec<_>>(),
    )
    .unwrap();
    let state =
        String::from_utf8(records.last().unwrap().next_state().as_bytes().to_vec()).unwrap();
    let debug = format!("{records:?}");
    for forbidden in [
        "/Users/private",
        "sk-super-secret",
        "sk-output-secret",
        &base64,
        "PRIVATE_CHAIN_OF_THOUGHT",
        "cat /Users",
    ] {
        assert!(!event_json.contains(forbidden));
        assert!(!state.contains(forbidden));
        assert!(!debug.contains(forbidden));
    }
}

#[test]
fn large_message_keeps_exact_hash_and_length_without_evidence() {
    let text = "x".repeat(agbox_adapters::MAX_CAPTURE_BYTES + 4096);
    let json = format!(
        r#"{{"timestamp":"2026-07-17T02:00:01Z","type":"response_item","payload":{{"type":"message","role":"user","content":"{text}"}}}}"#
    );
    let decoded = decode_one(&json, &DecoderState::default()).unwrap();
    let content = decoded
        .events()
        .iter()
        .find_map(|event| match event.payload() {
            EventPayload::MessageCreated { content } => Some(content),
            _ => None,
        })
        .unwrap();
    assert_eq!(content.byte_length(), text.len() as u64);
    assert_eq!(
        content.hash(),
        blake3::hash(text.as_bytes()).to_hex().as_str()
    );
    assert!(content.is_truncated());
    assert!(decoded.evidence().is_empty());
}

#[test]
fn artifact_changes_require_a_trusted_contained_path() {
    let fixture = |path: &str| {
        format!(
            r#"{{"timestamp":"2026-07-17T03:00:00Z","ordinal":0,"type":"session_meta","payload":{{"history_mode":"paginated"}}}}
{{"timestamp":"2026-07-17T03:00:01Z","ordinal":1,"type":"response_item","payload":{{"type":"custom_tool_call","name":"apply_patch","input":"patch","call_id":"call-1"}}}}
{{"timestamp":"2026-07-17T03:00:02Z","ordinal":2,"type":"event_msg","payload":{{"type":"item_completed","item":{{"type":"file_change","call_id":"call-1","status":"completed","changes":[{{"path":"{path}","kind":"update"}}]}}}}}}"#
        )
    };
    assert!(
        decode_lines(&fixture("src/lib.rs"))[2]
            .events()
            .iter()
            .any(|event| matches!(event.payload(), EventPayload::ArtifactChanged { .. }))
    );
    for unsafe_path in ["../secret", "/outside/project/file.rs"] {
        assert!(
            !decode_lines(&fixture(unsafe_path))[2]
                .events()
                .iter()
                .any(|event| matches!(event.payload(), EventPayload::ArtifactChanged { .. }))
        );
    }

    let mut no_root = context();
    no_root.project_root = None;
    let mut state = DecoderState::default();
    let mut records = Vec::new();
    for line in fixture("src/lib.rs").lines() {
        let decoded = CodexAdapter
            .decode(
                &MemoryRecordSource::new(line.as_bytes().to_vec()),
                &no_root,
                &state,
            )
            .unwrap();
        state = decoded.next_state().clone();
        records.push(decoded);
    }
    assert!(
        !records[2]
            .events()
            .iter()
            .any(|event| matches!(event.payload(), EventPayload::ArtifactChanged { .. }))
    );
}

#[test]
fn completed_window_evicts_the_129th_oldest_key() {
    let mut state = decode_one(
        r#"{"timestamp":"2026-07-17T02:00:00Z","type":"session_meta","payload":{}}"#,
        &DecoderState::default(),
    )
    .unwrap()
    .next_state()
    .clone();
    for index in 0..129 {
        let call_id = format!("call-{index}");
        let request = decode_one(
            &format!(
                r#"{{"timestamp":"2026-07-17T02:00:01Z","type":"response_item","payload":{{"type":"function_call","name":"tool","arguments":"{{}}","call_id":"{call_id}"}}}}"#
            ),
            &state,
        )
        .unwrap();
        let result = decode_one(
            &format!(
                r#"{{"timestamp":"2026-07-17T02:00:02Z","type":"response_item","payload":{{"type":"function_call_output","call_id":"{call_id}","output":"ok"}}}}"#
            ),
            request.next_state(),
        )
        .unwrap();
        state = result.next_state().clone();
    }
    let state_json: serde_json::Value = serde_json::from_slice(state.as_bytes()).unwrap();
    assert_eq!(
        state_json["completed_semantic_keys"]
            .as_array()
            .unwrap()
            .len(),
        128
    );

    let request = decode_one(
        r#"{"timestamp":"2026-07-17T02:00:03Z","type":"response_item","payload":{"type":"function_call","name":"tool","arguments":"{}","call_id":"call-0"}}"#,
        &state,
    )
    .unwrap();
    let replay = decode_one(
        r#"{"timestamp":"2026-07-17T02:00:04Z","type":"response_item","payload":{"type":"function_call_output","call_id":"call-0","output":"again"}}"#,
        request.next_state(),
    )
    .unwrap();
    assert_eq!(finished_count(&replay), 1);
}

#[test]
fn historical_pressure_is_evicted_before_128_live_calls() {
    let mut state = decode_one(
        r#"{"timestamp":"2026-07-17T02:00:00Z","type":"session_meta","payload":{}}"#,
        &DecoderState::default(),
    )
    .unwrap()
    .next_state()
    .clone();
    for index in 0..128 {
        let call_id = format!("call-old-{index}");
        let request = decode_one(
            &format!(
                r#"{{"timestamp":"2026-07-17T02:00:01Z","type":"response_item","payload":{{"type":"function_call","name":"tool","arguments":"{{}}","call_id":"{call_id}"}}}}"#
            ),
            &state,
        )
        .unwrap();
        let result = decode_one(
            &format!(
                r#"{{"timestamp":"2026-07-17T02:00:02Z","type":"response_item","payload":{{"type":"function_call_output","call_id":"{call_id}","output":"ok"}}}}"#
            ),
            request.next_state(),
        )
        .unwrap();
        state = result.next_state().clone();
    }
    for index in 0..128 {
        let tool_name = "t".repeat(64);
        let path = format!("src/{}/file.rs", "p".repeat(400));
        let request = decode_one(
            &format!(
                r#"{{"timestamp":"2026-07-17T02:00:03Z","type":"response_item","payload":{{"type":"function_call","name":"{tool_name}","arguments":"{{}}","path":"{path}","call_id":"call-live-{index}"}}}}"#
            ),
            &state,
        )
        .unwrap();
        state = request.next_state().clone();
    }
    let state_json: serde_json::Value = serde_json::from_slice(state.as_bytes()).unwrap();
    assert_eq!(
        state_json["unresolved_calls"].as_array().unwrap().len(),
        128
    );
    assert!(state.as_bytes().len() <= agbox_adapters::MAX_DECODER_STATE_BYTES);

    let result = decode_one(
        r#"{"timestamp":"2026-07-17T02:00:04Z","type":"response_item","payload":{"type":"function_call_output","call_id":"call-live-0","output":"still-correlated"}}"#,
        &state,
    )
    .unwrap();
    assert_eq!(finished_count(&result), 1);
}

#[test]
fn large_result_keeps_exact_hash_and_length_without_plaintext_evidence() {
    let large = "z".repeat(agbox_adapters::MAX_CAPTURE_BYTES + 8192);
    let first = decode_one(
        r#"{"timestamp":"2026-07-17T02:00:00Z","type":"session_meta","payload":{}}"#,
        &DecoderState::default(),
    )
    .unwrap();
    let request = decode_one(
        r#"{"timestamp":"2026-07-17T02:00:01Z","type":"response_item","payload":{"type":"function_call","name":"tool","arguments":"{}","call_id":"call-1"}}"#,
        first.next_state(),
    )
    .unwrap();
    let result = decode_one(
        &format!(
            r#"{{"timestamp":"2026-07-17T02:00:02Z","type":"response_item","payload":{{"type":"function_call_output","call_id":"call-1","output":"{large}"}}}}"#
        ),
        request.next_state(),
    )
    .unwrap();
    let output = result
        .events()
        .iter()
        .find_map(|event| match event.payload() {
            EventPayload::ActionFinished {
                output: Some(output),
                ..
            } => Some(output),
            _ => None,
        })
        .unwrap();
    assert_eq!(output.byte_length(), large.len() as u64);
    assert_eq!(
        output.hash(),
        blake3::hash(large.as_bytes()).to_hex().as_str()
    );
    assert!(output.is_truncated());
    assert!(result.evidence().is_empty());
}

#[test]
fn current_call_variants_require_durable_identity_and_status() {
    let records = decode_lines(
        r#"{"timestamp":"2026-07-17T02:00:00Z","type":"session_meta","payload":{}}
{"timestamp":"2026-07-17T02:00:01Z","type":"response_item","payload":{"type":"local_shell_call","call_id":"call-shell","action":{"command":"pwd"}}}
{"timestamp":"2026-07-17T02:00:02Z","type":"response_item","payload":{"type":"tool_search_call","call_id":"call-search","query":"docs"}}
{"timestamp":"2026-07-17T02:00:03Z","type":"response_item","payload":{"type":"tool_search_output","call_id":"call-search","output":"found"}}
{"timestamp":"2026-07-17T02:00:04Z","type":"response_item","payload":{"type":"web_search_call","id":"call-web","status":"completed","query":"rust"}}
{"timestamp":"2026-07-17T02:00:05Z","type":"response_item","payload":{"type":"image_generation_call","id":"call-image","status":"failed","prompt":"diagram"}}
{"timestamp":"2026-07-17T02:00:06Z","type":"response_item","payload":{"type":"web_search_call","query":"identityless"}}"#,
    );
    assert_eq!(
        records
            .iter()
            .flat_map(agbox_adapters::DecodedRecord::events)
            .filter(|event| matches!(event.payload(), EventPayload::ActionRequested { .. }))
            .count(),
        4
    );
    assert_eq!(records.iter().map(finished_count).sum::<usize>(), 3);
    assert!(records.last().unwrap().events().is_empty());
}

#[cfg(unix)]
#[test]
fn artifact_path_rejects_symlink_escape() {
    use std::{fs, os::unix::fs::symlink};

    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("project");
    let outside = temp.path().join("outside");
    fs::create_dir_all(&root).unwrap();
    fs::create_dir_all(&outside).unwrap();
    symlink(&outside, root.join("linked")).unwrap();
    let mut custom_context = context();
    custom_context.project_root = Some(root);
    let fixture = r#"{"timestamp":"2026-07-17T03:00:00Z","ordinal":0,"type":"session_meta","payload":{"history_mode":"paginated"}}
{"timestamp":"2026-07-17T03:00:01Z","ordinal":1,"type":"response_item","payload":{"type":"custom_tool_call","name":"apply_patch","input":"patch","call_id":"call-1"}}
{"timestamp":"2026-07-17T03:00:02Z","ordinal":2,"type":"event_msg","payload":{"type":"item_completed","item":{"type":"file_change","call_id":"call-1","status":"completed","changes":[{"path":"linked/file.rs","kind":"update"}]}}}"#;
    let mut state = DecoderState::default();
    let records = fixture
        .lines()
        .map(|line| {
            let decoded = CodexAdapter
                .decode(
                    &MemoryRecordSource::new(line.as_bytes().to_vec()),
                    &custom_context,
                    &state,
                )
                .unwrap();
            state = decoded.next_state().clone();
            decoded
        })
        .collect::<Vec<_>>();
    assert!(
        !records[2]
            .events()
            .iter()
            .any(|event| matches!(event.payload(), EventPayload::ArtifactChanged { .. }))
    );
}
