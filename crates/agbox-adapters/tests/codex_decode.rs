#![allow(clippy::unwrap_used)]

use std::{fmt::Write as _, fs, path::Path};

use agbox_adapters::{
    CodexAdapter, DecodeContext, DecodeDisposition, DecoderState, MemoryRecordSource, RootClass,
    SourceAdapter, adapters, test_support::decode_fixture_file,
};
use agbox_core::{ActionOutcome, EventPayload, ProjectId, Provider};
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

fn drain_continuations(mut state: DecoderState) -> Vec<agbox_adapters::DecodedRecord> {
    let mut records = Vec::new();
    while let Some(decoded) = CodexAdapter
        .decode_continuation(&context(), &state)
        .unwrap()
    {
        state = decoded.next_state().clone();
        records.push(decoded);
    }
    records
}

#[test]
fn legacy_rollout_uses_response_inputs_and_terminal_event_results() {
    let records = decode_fixture_file("codex", "tests/fixtures/codex/legacy.jsonl").unwrap();
    let events = records
        .iter()
        .flat_map(agbox_adapters::DecodedRecord::events)
        .collect::<Vec<_>>();
    let requested_id = events
        .iter()
        .find_map(|event| match event.payload() {
            EventPayload::ActionRequested {
                native_action_id, ..
            } => Some(native_action_id),
            _ => None,
        })
        .unwrap();
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(
                event.payload(),
                EventPayload::ActionFinished { native_action_id, .. }
                    if native_action_id == requested_id
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
    let temp = tempfile::tempdir().unwrap();
    let roots = CodexAdapter.roots(temp.path());
    assert_eq!(roots.len(), 2);
    assert_eq!(roots[0].class, RootClass::Active);
    assert!(roots[0].path.ends_with(".codex/sessions"));
    assert_eq!(roots[1].class, RootClass::Archive);
    assert!(roots[1].path.ends_with(".codex/archived_sessions"));
    let active_file = Path::new("2026/07/17/rollout.jsonl");
    fs::create_dir_all(
        active_file
            .parent()
            .map(|path| roots[0].path.join(path))
            .unwrap(),
    )
    .unwrap();
    fs::write(roots[0].path.join(active_file), b"{}\n").unwrap();
    fs::create_dir_all(roots[0].path.join("directory.jsonl")).unwrap();
    assert!(CodexAdapter.matches(&roots[0], active_file));
    assert!(!CodexAdapter.matches(&roots[0], Path::new("missing.jsonl")));
    assert!(!CodexAdapter.matches(&roots[0], Path::new("directory.jsonl")));
    assert!(!CodexAdapter.matches(&roots[0], Path::new("../rollout.jsonl")));
    assert!(!CodexAdapter.matches(&roots[0], Path::new("rollout.json")));
    let not_a_root = temp.path().join("not-a-directory");
    fs::write(&not_a_root, b"x").unwrap();
    let metadata_error_root = agbox_adapters::RootSpec {
        path: not_a_root,
        class: RootClass::Active,
        recursive: true,
    };
    assert!(!CodexAdapter.matches(&metadata_error_root, Path::new("child/file.jsonl")));
    #[cfg(unix)]
    {
        use std::os::unix::fs::symlink;

        symlink(
            roots[0].path.join(active_file),
            roots[0].path.join("linked.jsonl"),
        )
        .unwrap();
        assert!(!CodexAdapter.matches(&roots[0], Path::new("linked.jsonl")));
    }
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
fn first_explicit_paginated_session_requires_a_valid_ordinal() {
    for ordinal in [
        "",
        r#""ordinal":null,"#,
        r#""ordinal":"0","#,
        r#""ordinal":-1,"#,
        r#""ordinal":18446744073709551616,"#,
    ] {
        let record = format!(
            r#"{{"timestamp":"2026-07-17T03:00:00Z",{ordinal}"type":"session_meta","payload":{{"history_mode":"paginated"}}}}"#
        );
        let rejected = decode_one(&record, &DecoderState::default()).unwrap();
        assert!(matches!(
            rejected.disposition(),
            DecodeDisposition::Malformed { .. }
        ));
        assert_eq!(rejected.next_state(), &DecoderState::default());
        let recovered = decode_one(
            r#"{"timestamp":"2026-07-17T03:00:01Z","ordinal":0,"type":"session_meta","payload":{"history_mode":"paginated"}}"#,
            rejected.next_state(),
        )
        .unwrap();
        assert!(matches!(recovered.disposition(), DecodeDisposition::Known));
    }
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
    assert_eq!(state["completed_semantic_keys"][0][1], 3);
}

#[test]
fn late_authoritative_result_replaces_staged_failure_with_typed_success() {
    let records = decode_lines(
        r#"{"timestamp":"2026-07-17T03:00:00Z","ordinal":0,"type":"session_meta","payload":{"history_mode":"paginated"}}
{"timestamp":"2026-07-17T03:00:01Z","ordinal":1,"type":"response_item","payload":{"type":"custom_tool_call","name":"apply_patch","input":"patch","call_id":"call-1"}}
{"timestamp":"2026-07-17T03:00:02Z","ordinal":2,"type":"event_msg","payload":{"type":"patch_apply_end","call_id":"call-1","status":"failed","output":"weak failure","path":"src/weak.rs"}}
{"timestamp":"2026-07-17T03:00:03Z","ordinal":3,"type":"event_msg","payload":{"type":"item_completed","item":{"type":"file_change","call_id":"call-1","status":"completed","output":"typed success","changes":[{"path":"src/lib.rs","kind":"update"}]}}}"#,
    );
    assert_eq!(finished_count(&records[2]), 0);
    let request_id = records[1]
        .events()
        .iter()
        .find(|event| matches!(event.payload(), EventPayload::ActionRequested { .. }))
        .unwrap()
        .event_id()
        .as_str();
    let finished = records[3]
        .events()
        .iter()
        .find(|event| matches!(event.payload(), EventPayload::ActionFinished { .. }))
        .unwrap();
    assert_eq!(finished.causation_id(), Some(request_id));
    let EventPayload::ActionFinished {
        outcome, output, ..
    } = finished.payload()
    else {
        unreachable!();
    };
    assert_eq!(*outcome, ActionOutcome::Succeeded);
    let output = output.as_ref().unwrap();
    assert_eq!(output.byte_length(), "typed success".len() as u64);
    assert_eq!(
        output.hash(),
        blake3::hash(b"typed success").to_hex().as_str()
    );
    assert!(
        records[3]
            .events()
            .iter()
            .any(|event| matches!(event.payload(), EventPayload::ArtifactChanged { .. }))
    );
    let state: serde_json::Value =
        serde_json::from_slice(records[3].next_state().as_bytes()).unwrap();
    assert!(state["pending_results"].as_array().unwrap().is_empty());
    assert_eq!(state["completed_semantic_keys"][0][1], 3);
}

#[test]
fn pending_fallback_survives_reload_and_flushes_at_terminal_boundary() {
    let first = decode_one(
        r#"{"timestamp":"2026-07-17T03:00:00Z","ordinal":0,"type":"session_meta","payload":{"history_mode":"paginated"}}"#,
        &DecoderState::default(),
    )
    .unwrap();
    let request = decode_one(
        r#"{"timestamp":"2026-07-17T03:00:01Z","ordinal":1,"type":"response_item","payload":{"type":"function_call","name":"exec_command","arguments":"{}","call_id":"call-1"}}"#,
        first.next_state(),
    )
    .unwrap();
    let request_id = request.events()[0].event_id().as_str().to_owned();
    let weak = decode_one(
        r#"{"timestamp":"2026-07-17T03:00:02Z","ordinal":2,"type":"event_msg","payload":{"type":"exec_command_end","call_id":"call-1","exit_code":17,"stdout":"persisted fallback"}}"#,
        request.next_state(),
    )
    .unwrap();
    assert_eq!(finished_count(&weak), 0);
    let staged: serde_json::Value = serde_json::from_slice(weak.next_state().as_bytes()).unwrap();
    assert_eq!(staged["pending_results"].as_array().unwrap().len(), 1);
    let reloaded = weak.next_state().clone();
    let terminal = decode_one(
        r#"{"timestamp":"2026-07-17T03:00:03Z","ordinal":3,"type":"event_msg","payload":{"type":"task_complete"}}"#,
        &reloaded,
    )
    .unwrap();
    let finished = terminal
        .events()
        .iter()
        .find(|event| matches!(event.payload(), EventPayload::ActionFinished { .. }))
        .unwrap();
    assert_eq!(finished.causation_id(), Some(request_id.as_str()));
    let EventPayload::ActionFinished {
        outcome, output, ..
    } = finished.payload()
    else {
        unreachable!();
    };
    assert_eq!(*outcome, ActionOutcome::Failed);
    let output = output.as_ref().unwrap();
    assert_eq!(output.byte_length(), "persisted fallback".len() as u64);
    assert_eq!(
        output.hash(),
        blake3::hash(b"persisted fallback").to_hex().as_str()
    );
    assert!(terminal.evidence().is_empty());
    let final_state: serde_json::Value =
        serde_json::from_slice(terminal.next_state().as_bytes()).unwrap();
    assert!(
        final_state["pending_results"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert_eq!(final_state["completed_semantic_keys"][0][1], 1);
}

#[test]
fn legacy_response_output_flushes_as_rank_two_fallback() {
    let records = decode_lines(
        r#"{"timestamp":"2026-07-17T02:00:00Z","type":"session_meta","payload":{}}
{"timestamp":"2026-07-17T02:00:01Z","type":"response_item","payload":{"type":"function_call","name":"tool","arguments":"{}","call_id":"call-1"}}
{"timestamp":"2026-07-17T02:00:02Z","type":"response_item","payload":{"type":"function_call_output","call_id":"call-1","output":"response fallback"}}
{"timestamp":"2026-07-17T02:00:03Z","type":"event_msg","payload":{"type":"task_complete"}}"#,
    );
    assert_eq!(finished_count(&records[2]), 0);
    assert_eq!(finished_count(&records[3]), 1);
    let state: serde_json::Value =
        serde_json::from_slice(records[3].next_state().as_bytes()).unwrap();
    assert_eq!(state["completed_semantic_keys"][0][1], 2);
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
{"timestamp":"2026-07-17T02:00:03Z","type":"response_item","payload":{"type":"function_call_output","call_id":"call-1","output":"replay"}}
{"timestamp":"2026-07-17T02:00:04Z","type":"event_msg","payload":{"type":"task_complete"}}"#,
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
fn unknown_variant_ignores_malformed_history_hint_and_recovers_after_reload() {
    let first = decode_one(
        r#"{"timestamp":"2026-07-17T03:00:00Z","ordinal":0,"type":"session_meta","payload":{"history_mode":"paginated"}}"#,
        &DecoderState::default(),
    )
    .unwrap();
    let oversized = "x".repeat(1024);
    let unknown = decode_one(
        &format!(
            r#"{{"timestamp":"2026-07-17T03:00:01Z","ordinal":1,"type":"response_item","payload":{{"type":"future_item","history_mode":{{"secret":"DO_NOT_PARSE"}},"history_mode":"{oversized}"}}}}"#
        ),
        first.next_state(),
    )
    .unwrap();
    assert!(matches!(
        unknown.disposition(),
        DecodeDisposition::UnknownType { .. }
    ));
    assert!(unknown.events().is_empty());
    assert!(!String::from_utf8_lossy(unknown.next_state().as_bytes()).contains("DO_NOT_PARSE"));
    let reloaded = unknown.next_state().clone();
    let recovered = decode_one(
        r#"{"timestamp":"2026-07-17T03:00:02Z","ordinal":2,"type":"compacted","payload":{"replacement_history":[]}}"#,
        &reloaded,
    )
    .unwrap();
    assert!(matches!(recovered.disposition(), DecodeDisposition::Known));
}

#[test]
fn ordinal_exhaustion_rejects_repeated_u64_max_without_progression() {
    let first = decode_one(
        r#"{"timestamp":"2026-07-17T03:00:00Z","ordinal":18446744073709551615,"type":"session_meta","payload":{"history_mode":"paginated"}}"#,
        &DecoderState::default(),
    )
    .unwrap();
    let repeated = decode_one(
        r#"{"timestamp":"2026-07-17T03:00:01Z","ordinal":18446744073709551615,"type":"compacted","payload":{}}"#,
        first.next_state(),
    )
    .unwrap();
    assert!(matches!(
        repeated.disposition(),
        DecodeDisposition::Malformed { .. }
    ));
    assert_eq!(repeated.next_state(), first.next_state());
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
fn malformed_semantics_are_quarantined_while_envelope_progression_recovers() {
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
        assert!(decoded.events().is_empty());
        let progressed: serde_json::Value =
            serde_json::from_slice(decoded.next_state().as_bytes()).unwrap();
        assert_eq!(progressed["last_ordinal"], 1);
        assert_eq!(progressed["unresolved_calls"].as_array().unwrap().len(), 0);
        let reloaded = decoded.next_state().clone();
        let recovered = decode_one(
            r#"{"timestamp":"2026-07-17T03:00:02Z","ordinal":2,"type":"compacted","payload":{"replacement_history":[]}}"#,
            &reloaded,
        )
        .unwrap();
        assert!(matches!(recovered.disposition(), DecodeDisposition::Known));
        assert!(
            recovered
                .events()
                .iter()
                .any(|event| matches!(event.payload(), EventPayload::ContextCompacted { .. }))
        );
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
fn duplicate_change_kind_variants_quarantine_and_following_record_recovers() {
    for duplicate in [
        r#""kind":"update","kind":"delete""#,
        r#""kind":"update","kind":{"future":true}"#,
        r#""kind":"update","kind":7"#,
    ] {
        let first = decode_one(
            r#"{"timestamp":"2026-07-17T03:00:00Z","ordinal":0,"type":"session_meta","payload":{"history_mode":"paginated"}}"#,
            &DecoderState::default(),
        )
        .unwrap();
        let request = decode_one(
            r#"{"timestamp":"2026-07-17T03:00:01Z","ordinal":1,"type":"response_item","payload":{"type":"custom_tool_call","name":"apply_patch","input":"patch","call_id":"call-1"}}"#,
            first.next_state(),
        )
        .unwrap();
        let malformed = decode_one(
            &format!(
                r#"{{"timestamp":"2026-07-17T03:00:02Z","ordinal":2,"type":"event_msg","payload":{{"type":"item_completed","item":{{"type":"file_change","call_id":"call-1","status":"completed","changes":[{{"path":"src/bad.rs",{duplicate}}}]}}}}}}"#
            ),
            request.next_state(),
        )
        .unwrap();
        assert!(matches!(
            malformed.disposition(),
            DecodeDisposition::Malformed { .. }
        ));
        assert!(malformed.events().is_empty());
        let quarantined: serde_json::Value =
            serde_json::from_slice(malformed.next_state().as_bytes()).unwrap();
        assert_eq!(quarantined["last_ordinal"], 2);
        assert_eq!(quarantined["unresolved_calls"].as_array().unwrap().len(), 1);
        let recovered = decode_one(
            r#"{"timestamp":"2026-07-17T03:00:03Z","ordinal":3,"type":"event_msg","payload":{"type":"item_completed","item":{"type":"file_change","call_id":"call-1","status":"completed","changes":[{"path":"src/good.rs","kind":"update"}]}}}"#,
            malformed.next_state(),
        )
        .unwrap();
        assert_eq!(finished_count(&recovered), 1);
        assert!(
            recovered
                .events()
                .iter()
                .any(|event| matches!(event.payload(), EventPayload::ArtifactChanged { .. }))
        );
    }
}

#[test]
fn maximum_file_change_cardinality_shares_one_retained_budget() {
    let first = decode_one(
        r#"{"timestamp":"2026-07-17T03:00:00Z","ordinal":0,"type":"session_meta","payload":{"history_mode":"paginated"}}"#,
        &DecoderState::default(),
    )
    .unwrap();
    let request = decode_one(
        r#"{"timestamp":"2026-07-17T03:00:01Z","ordinal":1,"type":"response_item","payload":{"type":"custom_tool_call","name":"apply_patch","input":"patch","call_id":"call-1"}}"#,
        first.next_state(),
    )
    .unwrap();
    let changes = (0..agbox_adapters::MAX_EVENTS_PER_RECORD)
        .map(|index| {
            format!(
                r#"{{"path":"src/{index}/{}.rs","kind":"update"}}"#,
                "p".repeat(450)
            )
        })
        .collect::<Vec<_>>()
        .join(",");
    let result_text = "q".repeat(50 * 1024);
    let completed = decode_one(
        &format!(
            r#"{{"timestamp":"2026-07-17T03:00:02Z","ordinal":2,"type":"event_msg","payload":{{"type":"item_completed","item":{{"type":"file_change","call_id":"call-1","status":"completed","output":"{result_text}","changes":[{changes}]}}}}}}"#
        ),
        request.next_state(),
    )
    .unwrap();
    assert!(matches!(completed.disposition(), DecodeDisposition::Known));
    assert_eq!(
        completed.events().len(),
        agbox_adapters::MAX_EVENTS_PER_RECORD
    );
    assert_eq!(finished_count(&completed), 1);
    assert_eq!(
        completed
            .events()
            .iter()
            .filter(|event| matches!(event.payload(), EventPayload::ArtifactChanged { .. }))
            .count(),
        agbox_adapters::MAX_EVENTS_PER_RECORD - 1
    );
    let output = completed
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
    assert_eq!(output.byte_length(), result_text.len() as u64);
    assert_eq!(
        output.hash(),
        blake3::hash(result_text.as_bytes()).to_hex().as_str()
    );
    assert!(completed.evidence().is_empty());
    assert!(completed.next_state().as_bytes().len() <= agbox_adapters::MAX_DECODER_STATE_BYTES);
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
                r#"{{"timestamp":"2026-07-17T02:00:02Z","type":"event_msg","payload":{{"type":"exec_command_end","call_id":"{call_id}","exit_code":0,"stdout":"ok"}}}}"#
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
        r#"{"timestamp":"2026-07-17T02:00:04Z","type":"event_msg","payload":{"type":"exec_command_end","call_id":"call-0","exit_code":0,"stdout":"again"}}"#,
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
                r#"{{"timestamp":"2026-07-17T02:00:02Z","type":"event_msg","payload":{{"type":"exec_command_end","call_id":"{call_id}","exit_code":0,"stdout":"ok"}}}}"#
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

    let staged = decode_one(
        r#"{"timestamp":"2026-07-17T02:00:04Z","type":"response_item","payload":{"type":"function_call_output","call_id":"call-live-0","output":"still-correlated"}}"#,
        &state,
    )
    .unwrap();
    assert_eq!(finished_count(&staged), 0);
    let terminal = decode_one(
        r#"{"timestamp":"2026-07-17T02:00:05Z","type":"event_msg","payload":{"type":"task_complete"}}"#,
        staged.next_state(),
    )
    .unwrap();
    assert_eq!(finished_count(&terminal), 1);
}

#[test]
#[allow(clippy::too_many_lines)]
fn maximum_pending_window_fits_and_drains_atomically_across_continuations() {
    let mut state = decode_one(
        r#"{"timestamp":"2026-07-17T03:00:00Z","ordinal":0,"type":"session_meta","payload":{"history_mode":"paginated"}}"#,
        &DecoderState::default(),
    )
    .unwrap()
    .next_state()
    .clone();
    let large_output = "o".repeat(agbox_adapters::MAX_CAPTURE_BYTES + 1);
    let maximum_path = format!("src/{}.rs", "p".repeat(496));
    for index in 0_u64..128 {
        let call_id = format!("call-{index:0>123}");
        let request_ordinal = 1 + index * 2;
        let result_ordinal = request_ordinal + 1;
        let request = decode_one(
            &format!(
                r#"{{"timestamp":"2026-07-17T03:00:01Z","ordinal":{request_ordinal},"type":"response_item","payload":{{"type":"custom_tool_call","name":"apply_patch","input":"patch","call_id":"{call_id}"}}}}"#
            ),
            &state,
        )
        .unwrap();
        let staged = decode_one(
            &format!(
                r#"{{"timestamp":"2026-07-17T03:00:02Z","ordinal":{result_ordinal},"type":"event_msg","payload":{{"type":"patch_apply_end","call_id":"{call_id}","status":"completed","output":"{large_output}","path":"{maximum_path}"}}}}"#
            ),
            request.next_state(),
        )
        .unwrap();
        assert_eq!(finished_count(&staged), 0);
        state = staged.next_state().clone();
    }
    let state_json: serde_json::Value = serde_json::from_slice(state.as_bytes()).unwrap();
    assert_eq!(state_json["pending_results"].as_array().unwrap().len(), 128);
    assert!(
        state_json["unresolved_calls"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert!(state.as_bytes().len() <= agbox_adapters::MAX_DECODER_STATE_BYTES);
    let state_text = String::from_utf8(state.as_bytes().to_vec()).unwrap();
    assert!(!state_text.contains("src/"));
    assert!(!state_text.contains(&large_output[..128]));
    assert!(!state_text.contains("call-000000"));

    let terminal_json = r#"{"timestamp":"2026-07-17T03:00:03Z","ordinal":257,"type":"event_msg","payload":{"type":"task_complete"}}"#;
    let terminal = decode_one(terminal_json, &state).unwrap();
    assert!(CodexAdapter
        .decode(
            &MemoryRecordSource::new(
                br#"{"timestamp":"2026-07-17T03:00:04Z","ordinal":258,"type":"event_msg","payload":{"type":"agent_message","message":"blocked"}}"#.to_vec(),
            ),
            &context(),
            terminal.next_state(),
        )
        .is_err());
    let mut reloaded = DecoderState::default();
    reloaded
        .replace(terminal.next_state().as_bytes().to_vec())
        .unwrap();
    let mut pages = vec![terminal];
    pages.extend(drain_continuations(reloaded));
    let page_summary = pages
        .iter()
        .map(|record| {
            (
                record.events().len(),
                finished_count(record),
                record
                    .events()
                    .iter()
                    .filter(|event| matches!(event.payload(), EventPayload::ArtifactChanged { .. }))
                    .count(),
                format!("{:?}", record.disposition()),
                record.next_state().as_bytes().len(),
            )
        })
        .collect::<Vec<_>>();
    assert!(
        pages.iter().all(|record| {
            let finished = finished_count(record);
            let artifacts = record
                .events()
                .iter()
                .filter(|event| matches!(event.payload(), EventPayload::ArtifactChanged { .. }))
                .count();
            finished == artifacts && record.events().len() <= agbox_adapters::MAX_EVENTS_PER_RECORD
        }),
        "{page_summary:?}"
    );
    assert_eq!(
        pages.iter().map(finished_count).sum::<usize>(),
        128,
        "{:?}",
        pages
            .iter()
            .map(|record| (
                record.events().len(),
                format!("{:?}", record.disposition()),
                record.next_state().as_bytes().len()
            ))
            .collect::<Vec<_>>()
    );
    let event_ids = pages
        .iter()
        .flat_map(agbox_adapters::DecodedRecord::events)
        .map(|event| event.event_id().as_str())
        .collect::<std::collections::HashSet<_>>();
    assert_eq!(
        event_ids.len(),
        pages
            .iter()
            .map(|record| record.events().len())
            .sum::<usize>()
    );
    let observation_ids = pages
        .iter()
        .map(|record| record.observation().observation_id())
        .collect::<std::collections::HashSet<_>>();
    assert_eq!(observation_ids.len(), pages.len());
    let source_hashes = pages
        .iter()
        .map(|record| record.observation().source().record_hash())
        .collect::<std::collections::HashSet<_>>();
    assert_eq!(source_hashes.len(), 1);
    assert!(
        CodexAdapter
            .decode_continuation(&context(), pages.last().unwrap().next_state())
            .unwrap()
            .is_none()
    );
}

#[test]
fn mixed_maximum_live_window_preserves_all_entries_then_drains_every_result() {
    let mut state = decode_one(
        r#"{"timestamp":"2026-07-17T02:00:00Z","type":"session_meta","payload":{}}"#,
        &DecoderState::default(),
    )
    .unwrap()
    .next_state()
    .clone();
    let path = format!("src/{}.rs", "m".repeat(496));
    let output = "v".repeat(agbox_adapters::MAX_CAPTURE_BYTES + 1);
    for index in 0..128 {
        let call_id = format!("call-{index:0>123}");
        let request = decode_one(
            &format!(
                r#"{{"timestamp":"2026-07-17T02:00:01Z","type":"response_item","payload":{{"type":"custom_tool_call","name":"apply_patch","input":"patch","path":"{path}","call_id":"{call_id}"}}}}"#
            ),
            &state,
        )
        .unwrap();
        state = request.next_state().clone();
        if index < 80 {
            let staged = decode_one(
                &format!(
                r#"{{"timestamp":"2026-07-17T02:00:02Z","type":"response_item","payload":{{"type":"function_call_output","call_id":"{call_id}","status":"completed","output":"{output}"}}}}"#
                ),
                &state,
            )
            .unwrap();
            state = staged.next_state().clone();
        }
    }
    let width: serde_json::Value = serde_json::from_slice(state.as_bytes()).unwrap();
    assert_eq!(width["pending_results"].as_array().unwrap().len(), 80);
    assert_eq!(width["unresolved_calls"].as_array().unwrap().len(), 48);
    assert!(state.as_bytes().len() <= agbox_adapters::MAX_DECODER_STATE_BYTES);

    for index in 80..128 {
        let call_id = format!("call-{index:0>123}");
        let staged = decode_one(
            &format!(
                r#"{{"timestamp":"2026-07-17T02:00:03Z","type":"response_item","payload":{{"type":"function_call_output","call_id":"{call_id}","status":"completed","output":"{output}"}}}}"#
            ),
            &state,
        )
        .unwrap();
        state = staged.next_state().clone();
    }
    let staged_width: serde_json::Value = serde_json::from_slice(state.as_bytes()).unwrap();
    assert_eq!(
        staged_width["pending_results"].as_array().unwrap().len(),
        128
    );
    let terminal = decode_one(
        r#"{"timestamp":"2026-07-17T02:00:04Z","type":"event_msg","payload":{"type":"task_complete"}}"#,
        &state,
    )
    .unwrap();
    let continuation_state = terminal.next_state().clone();
    let mut pages = vec![terminal];
    pages.extend(drain_continuations(continuation_state));
    assert_eq!(
        pages.iter().map(finished_count).sum::<usize>(),
        128,
        "{:?}",
        pages
            .iter()
            .map(|record| (
                record.events().len(),
                format!("{:?}", record.disposition()),
                record.next_state().as_bytes().len()
            ))
            .collect::<Vec<_>>()
    );
    assert_eq!(
        pages
            .iter()
            .flat_map(agbox_adapters::DecodedRecord::events)
            .filter(|event| matches!(event.payload(), EventPayload::ArtifactChanged { .. }))
            .count(),
        128
    );
}

#[test]
fn fixture_helper_polls_bounded_continuations_until_none() {
    let mut fixture =
        String::from(r#"{"timestamp":"2026-07-17T02:00:00Z","type":"session_meta","payload":{}}"#);
    for index in 0..33 {
        fixture.push('\n');
        write!(
            fixture,
            r#"{{"timestamp":"2026-07-17T02:00:01Z","type":"response_item","payload":{{"type":"custom_tool_call","name":"apply_patch","input":"patch","path":"src/{index}.rs","call_id":"call-{index}"}}}}"#
        )
        .unwrap();
        fixture.push('\n');
        write!(
            fixture,
            r#"{{"timestamp":"2026-07-17T02:00:02Z","type":"response_item","payload":{{"type":"function_call_output","call_id":"call-{index}","status":"completed","output":"done"}}}}"#
        )
        .unwrap();
    }
    fixture.push_str(
        "\n{\"timestamp\":\"2026-07-17T02:00:03Z\",\"type\":\"event_msg\",\"payload\":{\"type\":\"task_complete\"}}\n",
    );
    let file = tempfile::NamedTempFile::new().unwrap();
    fs::write(file.path(), &fixture).unwrap();
    let records = decode_fixture_file("codex", file.path()).unwrap();
    assert!(records.len() > fixture.lines().count());
    assert_eq!(records.iter().map(finished_count).sum::<usize>(), 33);
    assert!(
        CodexAdapter
            .decode_continuation(&context(), records.last().unwrap().next_state())
            .unwrap()
            .is_none()
    );
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
    assert_eq!(finished_count(&result), 0);
    let terminal = decode_one(
        r#"{"timestamp":"2026-07-17T02:00:03Z","type":"event_msg","payload":{"type":"task_complete"}}"#,
        result.next_state(),
    )
    .unwrap();
    let output = terminal
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
    assert!(terminal.evidence().is_empty());
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
{"timestamp":"2026-07-17T02:00:06Z","type":"response_item","payload":{"type":"web_search_call","query":"identityless"}}
{"timestamp":"2026-07-17T02:00:07Z","type":"event_msg","payload":{"type":"task_complete"}}"#,
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
    assert!(records[6].events().is_empty());
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
