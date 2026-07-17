#![allow(clippy::unwrap_used)]

use std::io::Write;

use agbox_adapters::{
    ClaudeAdapter, DecodeContext, DecodeDisposition, DecodeError, DecoderState, MemoryRecordSource,
    SourceAdapter,
};
use agbox_core::{ActionOutcome, Actor, EventPayload, ProjectId};
use agbox_ingest::{RecordScanner, ScanOutcome};
use proptest::prelude::*;
use time::OffsetDateTime;

fn context() -> DecodeContext {
    DecodeContext {
        project_id: ProjectId::for_test("project_fixture"),
        project_root: Some("/fixture/project".into()),
        source_id: "source_fixture".to_owned(),
        observed_at: OffsetDateTime::UNIX_EPOCH,
        source_generation: 7,
        format: "claude-transcript-2.1".to_owned(),
    }
}

fn decode_lines(fixture: &str) -> Vec<agbox_adapters::DecodedRecord> {
    let adapter = ClaudeAdapter;
    let mut state = DecoderState::default();
    fixture
        .lines()
        .map(|line| {
            let source = MemoryRecordSource::new(line.as_bytes().to_vec());
            let decoded = adapter.decode(&source, &context(), &state).unwrap();
            state = decoded.next_state().clone();
            decoded
        })
        .collect()
}

fn decode_one(
    json: &str,
    state: &DecoderState,
) -> Result<agbox_adapters::DecodedRecord, DecodeError> {
    ClaudeAdapter.decode(
        &MemoryRecordSource::new(json.as_bytes().to_vec()),
        &context(),
        state,
    )
}

fn event_json(record: &agbox_adapters::DecodedRecord) -> String {
    serde_json::to_string(record.events()).unwrap()
}

#[test]
fn claude_basic_fixture_maps_messages_and_tool_pair() {
    let records = decode_lines(include_str!("fixtures/claude/basic.jsonl"));
    let events = records
        .iter()
        .flat_map(agbox_adapters::DecodedRecord::events)
        .collect::<Vec<_>>();

    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event.payload(), EventPayload::MessageCreated { .. }))
            .count(),
        2
    );
    let requested = events
        .iter()
        .find_map(|event| match event.payload() {
            EventPayload::ActionRequested {
                native_action_id,
                tool_name,
                ..
            } => Some((native_action_id, tool_name)),
            _ => None,
        })
        .unwrap();
    assert_eq!(requested.0, "tool-1");
    assert_eq!(requested.1, "Read");
    assert!(events.iter().any(|event| matches!(
        event.payload(),
        EventPayload::ActionFinished { native_action_id, .. } if native_action_id == "tool-1"
    )));
}

#[test]
fn roots_and_matcher_are_claude_jsonl_only() {
    let root = ClaudeAdapter
        .roots(std::path::Path::new("/home/test"))
        .remove(0);
    assert!(root.path.ends_with(".claude/projects"));
    assert!(root.recursive);
    assert!(ClaudeAdapter.matches(&root, std::path::Path::new("project/session.jsonl")));
    assert!(!ClaudeAdapter.matches(&root, std::path::Path::new("../session.jsonl")));
    assert!(!ClaudeAdapter.matches(&root, std::path::Path::new("session.json")));
    assert!(
        ClaudeAdapter
            .trusted_session_time(
                &root,
                std::path::Path::new("session.jsonl"),
                OffsetDateTime::UNIX_EPOCH
            )
            .is_none()
    );
}

#[test]
fn multiple_text_blocks_form_one_message_semantic_fact() {
    let decoded = decode_one(
        r#"{"type":"assistant","uuid":"a-multi","sessionId":"s1","timestamp":"2026-07-17T01:00:00Z","message":{"content":[{"type":"text","text":"first"},{"type":"thinking","thinking":"DO_NOT_KEEP"},{"type":"text","text":"second"}]}}"#,
        &DecoderState::default(),
    )
    .unwrap();
    let messages = decoded
        .events()
        .iter()
        .filter(|event| matches!(event.payload(), EventPayload::MessageCreated { .. }))
        .collect::<Vec<_>>();
    assert_eq!(messages.len(), 1);
    let EventPayload::MessageCreated { content } = messages[0].payload() else {
        unreachable!();
    };
    assert_eq!(content.redacted_excerpt(), Some("first\nsecond"));
    assert!(!event_json(&decoded).contains("DO_NOT_KEEP"));
    assert!(!format!("{decoded:?}").contains("DO_NOT_KEEP"));
}

#[test]
fn parallel_tools_correlate_out_of_order_and_ignore_duplicates_and_unknown_ids() {
    let request = decode_one(
        r#"{"type":"assistant","uuid":"a-tools","sessionId":"s1","timestamp":"2026-07-17T01:00:00Z","message":{"content":[{"type":"tool_use","id":"t1","name":"Read","input":{"file_path":"src/a.rs"}},{"type":"tool_use","id":"t2","name":"Read","input":{"file_path":"src/b.rs"}}]}}"#,
        &DecoderState::default(),
    )
    .unwrap();
    let second = decode_one(
        r#"{"type":"user","uuid":"u-t2","sessionId":"s1","timestamp":"2026-07-17T01:00:01Z","message":{"content":[{"type":"tool_result","tool_use_id":"t2","content":"two","is_error":false}]}}"#,
        request.next_state(),
    )
    .unwrap();
    let first = decode_one(
        r#"{"type":"user","uuid":"u-t1","sessionId":"s1","timestamp":"2026-07-17T01:00:02Z","message":{"content":[{"type":"tool_result","tool_use_id":"t1","content":"one","is_error":false}]}}"#,
        second.next_state(),
    )
    .unwrap();
    for (record, expected) in [(&second, "t2"), (&first, "t1")] {
        assert!(record.events().iter().any(|event| matches!(
            event.payload(),
            EventPayload::ActionFinished { native_action_id, .. } if native_action_id == expected
        )));
    }
    for id in ["t1", "unknown"] {
        let duplicate = decode_one(
            &format!(
                r#"{{"type":"user","uuid":"u-{id}","sessionId":"s1","timestamp":"2026-07-17T01:00:03Z","message":{{"content":[{{"type":"tool_result","tool_use_id":"{id}","content":"ignored","is_error":false}}]}}}}"#
            ),
            first.next_state(),
        )
        .unwrap();
        assert!(
            !duplicate
                .events()
                .iter()
                .any(|event| matches!(event.payload(), EventPayload::ActionFinished { .. }))
        );
    }
}

#[test]
fn duplicate_tool_id_replaces_the_older_link_deterministically() {
    let first = decode_one(
        r#"{"type":"assistant","uuid":"a-old","sessionId":"s1","timestamp":"2026-07-17T01:00:00Z","message":{"content":[{"type":"tool_use","id":"same","name":"Read","input":{"path":"old"}}]}}"#,
        &DecoderState::default(),
    )
    .unwrap();
    let second = decode_one(
        r#"{"type":"assistant","uuid":"a-new","sessionId":"s1","timestamp":"2026-07-17T01:00:01Z","message":{"content":[{"type":"tool_use","id":"same","name":"Read","input":{"path":"new"}}]}}"#,
        first.next_state(),
    )
    .unwrap();
    let result = decode_one(
        r#"{"type":"user","uuid":"u-result","sessionId":"s1","timestamp":"2026-07-17T01:00:02Z","message":{"content":[{"type":"tool_result","tool_use_id":"same","content":"done","is_error":false}]}}"#,
        second.next_state(),
    )
    .unwrap();
    let requested = second
        .events()
        .iter()
        .find(|event| matches!(event.payload(), EventPayload::ActionRequested { .. }))
        .unwrap();
    let finished = result
        .events()
        .iter()
        .find(|event| matches!(event.payload(), EventPayload::ActionFinished { .. }))
        .unwrap();
    assert_eq!(finished.causation_id(), Some(requested.event_id().as_str()));
}

#[test]
fn the_129th_tool_evicts_the_front_and_state_stays_bounded() {
    let mut state = DecoderState::default();
    for index in 0..129 {
        let decoded = decode_one(
            &format!(
                r#"{{"type":"assistant","uuid":"a-{index}","sessionId":"s1","timestamp":"2026-07-17T01:00:00Z","message":{{"content":[{{"type":"tool_use","id":"tool-{index}","name":"Read","input":{{"path":"src/{index}.rs"}}}}]}}}}"#
            ),
            &state,
        )
        .unwrap();
        state = decoded.next_state().clone();
        assert!(state.as_bytes().len() <= 32 * 1024);
    }
    let evicted = decode_one(
        r#"{"type":"user","uuid":"u-evicted","sessionId":"s1","timestamp":"2026-07-17T01:00:01Z","message":{"content":[{"type":"tool_result","tool_use_id":"tool-0","content":"old","is_error":false}]}}"#,
        &state,
    )
    .unwrap();
    assert!(
        !evicted
            .events()
            .iter()
            .any(|event| matches!(event.payload(), EventPayload::ActionFinished { .. }))
    );
    let retained = decode_one(
        r#"{"type":"user","uuid":"u-retained","sessionId":"s1","timestamp":"2026-07-17T01:00:02Z","message":{"content":[{"type":"tool_result","tool_use_id":"tool-128","content":"new","is_error":false}]}}"#,
        &state,
    )
    .unwrap();
    assert!(retained.events().iter().any(|event| matches!(
        event.payload(),
        EventPayload::ActionFinished { native_action_id, .. } if native_action_id == "tool-128"
    )));
}

#[test]
fn write_artifact_requires_success_and_a_trusted_project_path() {
    let request = decode_one(
        r#"{"type":"assistant","uuid":"a-write","sessionId":"s1","timestamp":"2026-07-17T01:00:00Z","cwd":"/malicious/root","message":{"content":[{"type":"tool_use","id":"write-1","name":"Write","input":{"file_path":"/fixture/project/src/lib.rs","content":"secret token=abc"}}]}}"#,
        &DecoderState::default(),
    )
    .unwrap();
    let request_json = event_json(&request);
    assert!(request_json.contains("$PROJECT/src/lib.rs"));
    assert!(!request_json.contains("/fixture/project"));
    assert!(!request_json.contains("/malicious/root"));
    assert!(!request_json.contains("token=abc"));

    let success = decode_one(
        r#"{"type":"user","uuid":"u-write","sessionId":"s1","timestamp":"2026-07-17T01:00:01Z","message":{"content":[{"type":"tool_result","tool_use_id":"write-1","content":{"bytes":12},"is_error":false}]}}"#,
        request.next_state(),
    )
    .unwrap();
    assert!(success.events().iter().any(|event| matches!(
        event.payload(),
        EventPayload::ArtifactChanged { path, .. }
            if path.redacted_excerpt() == Some("$PROJECT/src/lib.rs")
    )));

    let failed_request = decode_one(
        r#"{"type":"assistant","uuid":"a-fail","sessionId":"s1","timestamp":"2026-07-17T01:00:02Z","message":{"content":[{"type":"tool_use","id":"write-fail","name":"Edit","input":{"file_path":"src/lib.rs"}}]}}"#,
        success.next_state(),
    )
    .unwrap();
    let failed = decode_one(
        r#"{"type":"user","uuid":"u-fail","sessionId":"s1","timestamp":"2026-07-17T01:00:03Z","message":{"content":[{"type":"tool_result","tool_use_id":"write-fail","content":"no","is_error":true}]}}"#,
        failed_request.next_state(),
    )
    .unwrap();
    assert!(failed.events().iter().any(|event| matches!(
        event.payload(),
        EventPayload::ActionFinished {
            outcome: ActionOutcome::Failed,
            ..
        }
    )));
    assert!(
        !failed
            .events()
            .iter()
            .any(|event| matches!(event.payload(), EventPayload::ArtifactChanged { .. }))
    );
}

#[test]
fn outside_traversal_and_long_paths_never_create_artifacts() {
    for (index, path) in [
        "/outside/project/file.rs".to_owned(),
        "../escape.rs".to_owned(),
        format!("/fixture/project/{}", "x".repeat(600)),
    ]
    .into_iter()
    .enumerate()
    {
        let request = decode_one(
            &format!(
                r#"{{"type":"assistant","uuid":"a-path-{index}","sessionId":"s1","timestamp":"2026-07-17T01:00:00Z","message":{{"content":[{{"type":"tool_use","id":"path-{index}","name":"Write","input":{{"file_path":{}}}}}]}}}}"#,
                serde_json::to_string(&path).unwrap()
            ),
            &DecoderState::default(),
        )
        .unwrap();
        let result = decode_one(
            &format!(
                r#"{{"type":"user","uuid":"u-path-{index}","sessionId":"s1","timestamp":"2026-07-17T01:00:01Z","message":{{"content":[{{"type":"tool_result","tool_use_id":"path-{index}","content":"ok","is_error":false}}]}}}}"#
            ),
            request.next_state(),
        )
        .unwrap();
        assert!(
            !result
                .events()
                .iter()
                .any(|event| matches!(event.payload(), EventPayload::ArtifactChanged { .. }))
        );
    }
}

#[test]
fn required_identity_boundaries_and_timestamp_are_enforced() {
    let valid = "s".repeat(128);
    decode_one(
        &format!(
            r#"{{"type":"user","uuid":"u","sessionId":"{valid}","timestamp":"2026-07-17T01:00:00Z","message":{{"content":"ok"}}}}"#
        ),
        &DecoderState::default(),
    )
    .unwrap();
    for json in [
        r#"{"type":"user","uuid":"u","timestamp":"2026-07-17T01:00:00Z","message":{"content":"x"}}"#.to_owned(),
        format!(
            r#"{{"type":"user","uuid":"u","sessionId":"{}","timestamp":"2026-07-17T01:00:00Z","message":{{"content":"x"}}}}"#,
            "s".repeat(129)
        ),
        r#"{"type":"user","uuid":"u","sessionId":"s","timestamp":"yesterday","message":{"content":"x"}}"#.to_owned(),
    ] {
        assert!(decode_one(&json, &DecoderState::default()).is_err());
    }
}

#[test]
fn duplicate_sibling_fields_are_rejected() {
    let error = decode_one(
        r#"{"type":"assistant","uuid":"a","sessionId":"s","timestamp":"2026-07-17T01:00:00Z","message":{"content":[{"type":"tool_use","id":"one","id":"two","name":"Read","input":{}}]}}"#,
        &DecoderState::default(),
    )
    .unwrap_err();
    assert!(matches!(error, DecodeError::Malformed(_)));
}

#[test]
fn metadata_has_observation_without_human_intent_or_activity_facts() {
    let decoded = decode_one(
        r#"{"type":"attachment","message":{"content":"not human intent"}}"#,
        &DecoderState::default(),
    )
    .unwrap();
    assert!(decoded.events().is_empty());
    assert!(decoded.evidence().is_empty());
    assert!(matches!(decoded.disposition(), DecodeDisposition::Known));
}

#[test]
fn context_change_uses_only_trusted_project_labels_and_branch_hashes() {
    let first = decode_one(
        r#"{"type":"system","uuid":"sys1","sessionId":"s1","timestamp":"2026-07-17T01:00:00Z","cwd":"/fixture/project/sub","gitBranch":"secret/customer-branch","mode":"plan","permissionMode":"safe"}"#,
        &DecoderState::default(),
    )
    .unwrap();
    let serialized = event_json(&first);
    assert!(serialized.contains("$PROJECT/sub"));
    assert!(!serialized.contains("/fixture/project"));
    assert!(!serialized.contains("secret/customer-branch"));
    assert!(serialized.contains("branch_hash"));

    let repeated = decode_one(
        r#"{"type":"system","uuid":"sys2","sessionId":"s1","timestamp":"2026-07-17T01:00:01Z","cwd":"/fixture/project/sub","gitBranch":"secret/customer-branch","mode":"plan","permissionMode":"safe"}"#,
        first.next_state(),
    )
    .unwrap();
    assert!(
        !repeated
            .events()
            .iter()
            .any(|event| matches!(event.payload(), EventPayload::SessionContextChanged { .. }))
    );
}

#[test]
fn unknown_fields_are_bounded_and_only_shape_changes_schema_fingerprint() {
    let field = "f".repeat(4096);
    let one = decode_one(
        &format!(r#"{{"type":"future","{field}":"private-one"}}"#),
        &DecoderState::default(),
    )
    .unwrap();
    let two = decode_one(
        &format!(r#"{{"type":"future","{field}":"private-two"}}"#),
        &DecoderState::default(),
    )
    .unwrap();
    assert_eq!(
        one.observation().schema_fingerprint(),
        two.observation().schema_fingerprint()
    );
    assert!(!format!("{one:?}").contains("private-one"));
    assert!(matches!(
        one.disposition(),
        DecodeDisposition::UnknownType { .. }
    ));
}

#[test]
fn cardinality_limit_returns_a_bounded_oversized_diagnostic() {
    let blocks = (0..65)
        .map(|index| format!(r#"{{"type":"text","text":"{index}"}}"#))
        .collect::<Vec<_>>()
        .join(",");
    let decoded = decode_one(
        &format!(
            r#"{{"type":"assistant","uuid":"many","sessionId":"s1","timestamp":"2026-07-17T01:00:00Z","message":{{"content":[{blocks}]}}}}"#
        ),
        &DecoderState::default(),
    )
    .unwrap();
    assert!(decoded.events().is_empty());
    assert!(decoded.evidence().is_empty());
    assert!(matches!(
        decoded.disposition(),
        DecodeDisposition::Oversized { .. }
    ));
}

#[test]
fn content_media_types_distinguish_text_structured_json_and_paths() {
    let message = decode_one(
        r#"{"type":"user","uuid":"u-media","sessionId":"s1","timestamp":"2026-07-17T01:00:00Z","message":{"content":"hello"}}"#,
        &DecoderState::default(),
    )
    .unwrap();
    let message_content = message
        .events()
        .iter()
        .find_map(|event| match event.payload() {
            EventPayload::MessageCreated { content } => Some(content),
            _ => None,
        })
        .unwrap();
    assert_eq!(message_content.media_type(), "text/plain");

    let request = decode_one(
        r#"{"type":"assistant","uuid":"a-media","sessionId":"s1","timestamp":"2026-07-17T01:00:01Z","message":{"content":[{"type":"tool_use","id":"media-tool","name":"Write","input":{"file_path":"src/a.rs"}}]}}"#,
        message.next_state(),
    )
    .unwrap();
    let request_content = request
        .events()
        .iter()
        .find_map(|event| match event.payload() {
            EventPayload::ActionRequested { input, .. } => Some(input),
            _ => None,
        })
        .unwrap();
    assert_eq!(request_content.media_type(), "application/json");

    let result = decode_one(
        r#"{"type":"user","uuid":"u-media-result","sessionId":"s1","timestamp":"2026-07-17T01:00:02Z","message":{"content":[{"type":"tool_result","tool_use_id":"media-tool","content":{"ok":true},"is_error":false}]}}"#,
        request.next_state(),
    )
    .unwrap();
    let path = result
        .events()
        .iter()
        .find_map(|event| match event.payload() {
            EventPayload::ArtifactChanged { path, .. } => Some(path),
            _ => None,
        })
        .unwrap();
    assert_eq!(path.media_type(), "text/uri-list");
}

#[test]
fn structured_array_and_top_level_tool_results_are_streamed_as_whole_values() {
    let request = decode_one(
        r#"{"type":"assistant","uuid":"a-raw","sessionId":"s1","timestamp":"2026-07-17T01:00:00Z","message":{"content":[{"type":"tool_use","id":"raw-array","name":"Read","input":["src/a.rs",{"line":1}]}]}}"#,
        &DecoderState::default(),
    )
    .unwrap();
    let EventPayload::ActionRequested { input, .. } = request.events()[0].payload() else {
        panic!("expected request");
    };
    let raw_input = br#"["src/a.rs",{"line":1}]"#;
    assert_eq!(input.hash(), blake3::hash(raw_input).to_hex().as_str());
    assert_eq!(input.byte_length(), u64::try_from(raw_input.len()).unwrap());

    let result = decode_one(
        r#"{"type":"user","uuid":"u-raw","sessionId":"s1","timestamp":"2026-07-17T01:00:01Z","message":{"content":[{"type":"tool_result","tool_use_id":"raw-array","is_error":false}]},"toolUseResult":[{"type":"text","text":"ok"}]}"#,
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
    let raw_output = br#"[{"type":"text","text":"ok"}]"#;
    assert_eq!(output.hash(), blake3::hash(raw_output).to_hex().as_str());
    assert_eq!(
        result
            .evidence()
            .iter()
            .find(|evidence| &evidence.content == output)
            .unwrap()
            .plaintext
            .as_slice(),
        raw_output
    );
}

#[test]
fn source_identity_and_observation_are_distinct_across_files() {
    let json = r#"{"type":"user","uuid":"same","sessionId":"s1","timestamp":"2026-07-17T01:00:00Z","message":{"content":"same"}}"#;
    let source = MemoryRecordSource::new(json.as_bytes().to_vec());
    let mut second_context = context();
    second_context.source_id = "source_other".to_owned();
    let first = ClaudeAdapter
        .decode(&source, &context(), &DecoderState::default())
        .unwrap();
    let second = ClaudeAdapter
        .decode(&source, &second_context, &DecoderState::default())
        .unwrap();
    assert_ne!(first.events()[0].event_id(), second.events()[0].event_id());
    assert_ne!(
        first.evidence()[0].evidence_id,
        second.evidence()[0].evidence_id
    );
    assert_ne!(
        first.observation().observation_id(),
        second.observation().observation_id()
    );
}

#[test]
fn invalid_trusted_project_root_fails_closed() {
    let mut invalid = context();
    invalid.project_root = Some("../fixture/project".into());
    let source = MemoryRecordSource::new(
        br#"{"type":"user","uuid":"u","sessionId":"s","timestamp":"2026-07-17T01:00:00Z","message":{"content":"x"}}"#.to_vec(),
    );
    assert!(matches!(
        ClaudeAdapter.decode(&source, &invalid, &DecoderState::default()),
        Err(DecodeError::Malformed(_))
    ));
}

#[test]
fn multi_block_utf8_prefix_never_splits_a_scalar() {
    let first = "a".repeat(65_534);
    let json = format!(
        r#"{{"type":"assistant","uuid":"utf8","sessionId":"s1","timestamp":"2026-07-17T01:00:00Z","message":{{"content":[{{"type":"text","text":"{first}"}},{{"type":"text","text":"🦀"}}]}}}}"#
    );
    let decoded = decode_one(&json, &DecoderState::default()).unwrap();
    let EventPayload::MessageCreated { content } = decoded.events()[0].payload() else {
        panic!("expected message");
    };
    assert!(content.is_truncated());
    assert!(content.hash().starts_with("seq:b3:"));
}

#[test]
fn fixture_file_helper_preserves_correlation_state() {
    let path =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/claude/basic.jsonl");
    let records = agbox_adapters::test_support::decode_fixture_file("claude", path).unwrap();
    assert_eq!(records.len(), 3);
    assert!(records[2].events().iter().any(|event| matches!(
        event.payload(),
        EventPayload::ActionFinished { native_action_id, .. } if native_action_id == "tool-1"
    )));
}

#[test]
fn terminal_window_integrity_error_overrides_an_early_json_diagnostic() {
    let mut bytes = br#"{"type":!"#.to_vec();
    bytes.extend(std::iter::repeat_n(b' ', 9 * 1024));
    bytes.push(b'\n');
    let mut file = tempfile::tempfile().unwrap();
    file.write_all(&bytes).unwrap();
    let mutator = file.try_clone().unwrap();
    let mut scanner = RecordScanner::new(file, 0, u64::try_from(bytes.len()).unwrap()).unwrap();
    let ScanOutcome::Complete(window) = scanner.next().unwrap() else {
        panic!("expected complete record");
    };
    mutator.set_len(32).unwrap();

    let error = ClaudeAdapter
        .decode(&window, &context(), &DecoderState::default())
        .unwrap_err();
    assert!(
        matches!(error, DecodeError::Io(ref io) if io.kind() == std::io::ErrorKind::UnexpectedEof)
    );
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(16))]

    #[test]
    fn tool_result_order_preserves_exactly_once_correlation(
        priorities in prop::collection::vec(any::<u16>(), 1..8)
    ) {
        let expected = priorities.len();
        let request_blocks = priorities
            .iter()
            .enumerate()
            .map(|(index, _)| format!(
                r#"{{"type":"tool_use","id":"p-{index}","name":"Read","input":{{"path":"src/{index}.rs"}}}}"#
            ))
            .collect::<Vec<_>>()
            .join(",");
        let request = decode_one(
            &format!(
                r#"{{"type":"assistant","uuid":"prop-request","sessionId":"s1","timestamp":"2026-07-17T01:00:00Z","message":{{"content":[{request_blocks}]}}}}"#
            ),
            &DecoderState::default(),
        ).unwrap();
        let mut order = priorities.into_iter().enumerate().collect::<Vec<_>>();
        order.sort_by_key(|(index, priority)| (*priority, *index));
        let mut state = request.next_state().clone();
        let mut finished = Vec::new();
        for (sequence, (index, _)) in order.into_iter().enumerate() {
            let result = decode_one(
                &format!(
                    r#"{{"type":"user","uuid":"prop-{sequence}","sessionId":"s1","timestamp":"2026-07-17T01:00:01Z","message":{{"content":[{{"type":"tool_result","tool_use_id":"p-{index}","content":"ok","is_error":false}}]}}}}"#
                ),
                &state,
            ).unwrap();
            state = result.next_state().clone();
            finished.extend(result.events().iter().filter_map(|event| match event.payload() {
                EventPayload::ActionFinished { native_action_id, .. } => Some(native_action_id.clone()),
                _ => None,
            }));
        }
        finished.sort();
        finished.dedup();
        prop_assert_eq!(finished.len(), expected);
    }
}

#[test]
fn claude_array_user_content_keeps_text_and_excludes_private_blocks() {
    let records = decode_lines(include_str!("fixtures/claude/array-content.jsonl"));
    let serialized = serde_json::to_string(
        &records
            .iter()
            .flat_map(agbox_adapters::DecodedRecord::events)
            .collect::<Vec<_>>(),
    )
    .unwrap();
    let debug = format!("{records:?}");

    assert!(records[0].events().iter().any(|event| {
        event.actor() == Actor::Human
            && matches!(event.payload(), EventPayload::MessageCreated { .. })
    }));
    for forbidden in ["REDACTED_FIXTURE", "PRIVATE_REASONING_FIXTURE"] {
        assert!(!serialized.contains(forbidden));
        assert!(!debug.contains(forbidden));
        assert!(
            !records[0]
                .next_state()
                .as_bytes()
                .windows(forbidden.len())
                .any(|window| window == forbidden.as_bytes())
        );
    }
}
