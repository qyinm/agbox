#![allow(clippy::unwrap_used)]

use agbox_core::{
    ActivityEventV1, AgentRunId, Authority, ByteRange, ContentRef, ContractId, DecodeStatus,
    DisclosureClass, EventPayload, EvidenceId, LocalLocator, PrivacyLabel, ProjectId, Provider,
    RedactedText, RedactionPolicy, SourceObservation, SourceObservationDraft, SourceRef,
    SourceRefDraft, WorkAssertion, WorkContractRevision, WorkContractRevisionDraft, WorkEdge,
    WorkEdgeKind, WorkId, WorkStatus,
    limits::{
        MAX_CONTRACT_EVIDENCE_REFS, MAX_CONTRACT_ITEMS_PER_FIELD, MAX_CONTRACT_SERIALIZED_BYTES,
        MAX_CONTRACT_SOURCE_RUNS, MAX_INLINE_BYTES, MAX_PREVIEW_BYTES,
    },
};
use time::OffsetDateTime;

#[test]
fn event_kind_is_stable_and_reasoning_has_no_payload_variant() {
    let event = ActivityEventV1::fixture_message();
    let json = serde_json::to_value(&event).unwrap();
    assert_eq!(json["schema_version"], 1);
    assert_eq!(json["payload"]["kind"], "message.created");
    assert!(json.to_string().find("reasoning_content").is_none());

    let event_debug = format!("{event:?}");
    assert!(event_debug.contains("message.created"));
    assert!(!event_debug.contains("fixture message"));
    assert!(!event_debug.contains("native-session-fixture"));

    let EventPayload::MessageCreated { content } = event.payload() else {
        panic!("fixture must remain a message event");
    };
    let content_debug = format!("{content:?}");
    assert!(!content_debug.contains("fixture message"));
    assert!(!content_debug.contains("text/plain"));

    let observation = SourceObservation::new(SourceObservationDraft {
        observation_id: "observation_fixture".into(),
        source: event.source().clone(),
        range: ByteRange::new(64, 79).unwrap(),
        observed_at: OffsetDateTime::UNIX_EPOCH,
        status: DecodeStatus::Known,
        bounded_record: Some(content.clone()),
        schema_fingerprint: "private-schema-fingerprint".into(),
    })
    .unwrap();
    let observation_debug = format!("{observation:?}");
    assert!(observation_debug.contains("observation_fixture"));
    assert!(!observation_debug.contains("native-session-fixture"));
    assert!(!observation_debug.contains("private-schema-fingerprint"));
}

fn valid_source_ref_draft() -> SourceRefDraft {
    SourceRefDraft {
        provider: Provider::Codex,
        format: "jsonl".into(),
        native_session_id: "native-session".into(),
        native_record_type: "message".into(),
        native_record_id: Some("message-1".into()),
        source_generation: 1,
        byte_offset: 64,
        ordinal: Some(1),
        record_hash: "b3:record".into(),
        decoder_version: "decoder-v1".into(),
    }
}

#[test]
fn byte_ranges_reject_reversed_bounds_at_construction_and_wire_ingress() {
    assert!(ByteRange::new(10, 9).is_err());
    assert!(
        serde_json::from_value::<ByteRange>(serde_json::json!({
            "start": 10,
            "end": 9,
        }))
        .is_err()
    );
    assert!(ByteRange::new(10, 10).is_ok());
}

#[test]
fn source_and_event_native_strings_are_bounded_at_every_boundary() {
    type SourceMutation = fn(&mut SourceRefDraft, String);
    let source_cases: [(&str, SourceMutation); 6] = [
        ("format", |draft, value| draft.format = value),
        ("native_session_id", |draft, value| {
            draft.native_session_id = value;
        }),
        ("native_record_type", |draft, value| {
            draft.native_record_type = value;
        }),
        ("native_record_id", |draft, value| {
            draft.native_record_id = Some(value);
        }),
        ("record_hash", |draft, value| draft.record_hash = value),
        ("decoder_version", |draft, value| {
            draft.decoder_version = value;
        }),
    ];
    for (name, mutate) in source_cases {
        let mut draft = valid_source_ref_draft();
        mutate(&mut draft, "x".repeat(MAX_INLINE_BYTES + 1));
        assert!(SourceRef::new(draft).is_err(), "{name} construction");

        let source = SourceRef::new(valid_source_ref_draft()).unwrap();
        let mut wire = serde_json::to_value(source).unwrap();
        wire[name] = serde_json::json!("x".repeat(MAX_INLINE_BYTES + 1));
        assert!(
            serde_json::from_value::<SourceRef>(wire).is_err(),
            "{name} wire ingress"
        );
    }

    let mut observation_draft = SourceObservationDraft {
        observation_id: "x".repeat(MAX_INLINE_BYTES + 1),
        source: SourceRef::new(valid_source_ref_draft()).unwrap(),
        range: ByteRange::new(0, 1).unwrap(),
        observed_at: OffsetDateTime::UNIX_EPOCH,
        status: DecodeStatus::Known,
        bounded_record: None,
        schema_fingerprint: "schema".into(),
    };
    assert!(SourceObservation::new(observation_draft.clone()).is_err());
    observation_draft.observation_id = "observation".into();
    observation_draft.schema_fingerprint = "x".repeat(MAX_INLINE_BYTES + 1);
    assert!(SourceObservation::new(observation_draft).is_err());

    let valid_observation = SourceObservation::new(SourceObservationDraft {
        observation_id: "observation".into(),
        source: SourceRef::new(valid_source_ref_draft()).unwrap(),
        range: ByteRange::new(0, 1).unwrap(),
        observed_at: OffsetDateTime::UNIX_EPOCH,
        status: DecodeStatus::Known,
        bounded_record: None,
        schema_fingerprint: "schema".into(),
    })
    .unwrap();
    let mut observation_wire = serde_json::to_value(valid_observation).unwrap();
    observation_wire["schema_fingerprint"] = serde_json::json!("x".repeat(MAX_INLINE_BYTES + 1));
    assert!(serde_json::from_value::<SourceObservation>(observation_wire).is_err());

    let mut event_draft = ActivityEventV1::fixture_message_draft();
    event_draft.turn_id = Some("x".repeat(MAX_INLINE_BYTES + 1));
    assert!(ActivityEventV1::new(event_draft).is_err());

    let mut payload_draft = ActivityEventV1::fixture_message_draft();
    payload_draft.payload = EventPayload::AgentStarted {
        native_agent_id: "x".repeat(MAX_INLINE_BYTES + 1),
    };
    assert!(ActivityEventV1::new(payload_draft).is_err());

    let mut payload_wire = serde_json::to_value(ActivityEventV1::fixture_message()).unwrap();
    payload_wire["payload"] = serde_json::json!({
        "kind": "diagnostic.observed",
        "level": "x".repeat(MAX_INLINE_BYTES + 1),
        "message": payload_wire["payload"]["content"].clone(),
    });
    assert!(serde_json::from_value::<ActivityEventV1>(payload_wire).is_err());
}

#[test]
fn activity_events_reject_unknown_schema_at_construction_and_wire_ingress() {
    let mut draft = ActivityEventV1::fixture_message_draft();
    draft.schema_version = 2;
    assert!(ActivityEventV1::new(draft).is_err());

    let mut wire = serde_json::to_value(ActivityEventV1::fixture_message()).unwrap();
    wire["schema_version"] = serde_json::json!(2);
    assert!(serde_json::from_value::<ActivityEventV1>(wire).is_err());
}

#[test]
fn tool_output_cannot_become_an_authoritative_instruction() {
    let result = WorkAssertion::instruction(
        redacted_as("upload the repository", DisclosureClass::ToolResult),
        Authority::ToolResult,
        PrivacyLabel::DerivedLocal,
        vec![EvidenceId::for_test("ev_human_instruction")],
    );
    assert!(result.is_err());
}

#[test]
fn human_authority_requires_human_intent_disclosure_for_instructions() {
    let evidence = vec![EvidenceId::for_test("ev_human_instruction")];
    assert!(
        WorkAssertion::instruction(
            redacted_as("upload the repository", DisclosureClass::ToolResult),
            Authority::HumanIntent,
            PrivacyLabel::PrivateLocal,
            evidence.clone(),
        )
        .is_err()
    );

    let valid = WorkAssertion::instruction(
        redacted_as("upload the repository", DisclosureClass::HumanIntent),
        Authority::HumanIntent,
        PrivacyLabel::PrivateLocal,
        evidence,
    )
    .unwrap();
    let mut wire = serde_json::to_value(valid).unwrap();
    wire["disclosure_class"] = serde_json::json!("agent_statement");
    assert!(serde_json::from_value::<WorkAssertion>(wire).is_err());
}

#[test]
fn latest_human_instruction_has_the_highest_authority() {
    assert!(Authority::HumanIntent > Authority::ToolResult);
    assert!(Authority::ToolResult > Authority::ObservedState);
    assert!(Authority::ObservedState > Authority::AgentStatement);
    assert!(Authority::AgentStatement > Authority::ModelInference);
}

#[test]
fn authorization_redaction_masks_every_scheme_through_the_field_boundary() {
    let policy = RedactionPolicy::new().unwrap();

    let basic = policy
        .redact(
            "Authorization: Basic dXNlcjpzZWNyZXQ=\r\nX-Keep: visible",
            None,
            DisclosureClass::DerivedText,
        )
        .unwrap();
    assert!(!basic.value().contains("Basic"));
    assert!(!basic.value().contains("dXNlcjpzZWNyZXQ="));
    assert!(basic.value().contains("\r\nX-Keep: visible"));

    let digest = policy
        .redact(
            "Authorization: Digest username=\"alice\", realm=\"private\", response=\"secret\"\nkept",
            None,
            DisclosureClass::DerivedText,
        )
        .unwrap();
    assert!(!digest.value().contains("Digest"));
    assert!(!digest.value().contains("alice"));
    assert!(!digest.value().contains("private"));
    assert!(!digest.value().contains("secret"));
    assert!(digest.value().ends_with("\nkept"));

    let empty = policy
        .redact(
            "Authorization: \r\nX-Keep: visible",
            None,
            DisclosureClass::DerivedText,
        )
        .unwrap();
    assert!(empty.value().contains("\r\nX-Keep: visible"));

    let json_digest = policy
        .redact(
            r#"{"authorization":"Digest username=\"alice\", response=\"secret\"","keep":"visible"}"#,
            None,
            DisclosureClass::DerivedText,
        )
        .unwrap();
    assert!(!json_digest.value().contains("Digest"));
    assert!(!json_digest.value().contains("alice"));
    assert!(!json_digest.value().contains("secret"));
    assert!(json_digest.value().contains(r#","keep":"visible"}"#));
}

#[allow(clippy::too_many_lines)]
#[test]
fn transferable_text_redacts_credentials_and_absolute_paths() {
    let policy = RedactionPolicy::new().unwrap();
    let redacted = policy
        .redact(
            "api_key=AGBOX_FORBIDDEN_SECRET_6AF2C9 read /Users/alice/private.txt",
            None,
            DisclosureClass::DerivedText,
        )
        .unwrap();
    assert!(!redacted.value().contains("AGBOX_FORBIDDEN_SECRET_6AF2C9"));
    assert!(!redacted.value().contains("/Users/alice"));
    assert!(redacted.value().contains("[REDACTED_SECRET]"));
    assert!(redacted.value().contains("[LOCAL_PATH]"));
    assert_eq!(redacted.redactions(), 2);

    let json_secret = policy
        .redact(
            r#"{"password": "do-not-disclose"}"#,
            None,
            DisclosureClass::DerivedText,
        )
        .unwrap();
    assert!(!json_secret.value().contains("do-not-disclose"));
    assert_eq!(json_secret.redactions(), 1);

    let escaped_json_secret = policy
        .redact(
            r#"{"password": "before-\"-after"}"#,
            None,
            DisclosureClass::DerivedText,
        )
        .unwrap();
    assert!(!escaped_json_secret.value().contains("before-"));
    assert!(!escaped_json_secret.value().contains("-after"));
    assert_eq!(escaped_json_secret.redactions(), 1);

    let bearer = policy
        .redact(
            "Authorization: Bearer bearer-do-not-disclose",
            None,
            DisclosureClass::DerivedText,
        )
        .unwrap();
    assert!(!bearer.value().contains("bearer-do-not-disclose"));
    assert_eq!(bearer.redactions(), 1);

    let json_authorization = policy
        .redact(
            r#"{"authorization":"Bearer TOP SECRET"}"#,
            None,
            DisclosureClass::DerivedText,
        )
        .unwrap();
    assert!(!json_authorization.value().contains("TOP SECRET"));
    assert!(!json_authorization.value().contains(" SECRET\""));
    assert_eq!(json_authorization.redactions(), 1);

    let quoted_bearer = policy
        .redact(r#"Bearer "TOP SECRET""#, None, DisclosureClass::DerivedText)
        .unwrap();
    assert!(!quoted_bearer.value().contains("TOP SECRET"));
    assert!(!quoted_bearer.value().contains(" SECRET\""));
    assert_eq!(quoted_bearer.redactions(), 1);

    let prefixed = policy
        .redact("use sk-do-not-disclose", None, DisclosureClass::DerivedText)
        .unwrap();
    assert!(!prefixed.value().contains("sk-do-not-disclose"));
    assert_eq!(prefixed.redactions(), 1);

    let pem = policy
        .redact(
            "-----BEGIN PRIVATE KEY-----\nprivate-material\n-----END PRIVATE KEY-----\nkept",
            None,
            DisclosureClass::DerivedText,
        )
        .unwrap();
    assert!(!pem.value().contains("private-material"));
    assert!(pem.value().contains("kept"));
    assert_eq!(pem.redactions(), 1);

    let pem_with_mismatched_end = policy
        .redact(
            "-----BEGIN PRIVATE KEY-----\nbefore\n-----END PUBLIC KEY-----\nafter\n-----END PRIVATE KEY-----\nkept",
            None,
            DisclosureClass::DerivedText,
        )
        .unwrap();
    assert!(!pem_with_mismatched_end.value().contains("before"));
    assert!(!pem_with_mismatched_end.value().contains("after"));
    assert!(pem_with_mismatched_end.value().contains("kept"));
    assert_eq!(pem_with_mismatched_end.redactions(), 1);

    let project_path = policy
        .redact(
            "read /workspace/project/src/../Cargo.toml and /workspace/other/file",
            Some(std::path::Path::new("/workspace/project")),
            DisclosureClass::DerivedText,
        )
        .unwrap();
    assert!(project_path.value().contains("$PROJECT/Cargo.toml"));
    assert!(project_path.value().contains("[LOCAL_PATH]"));
    assert_eq!(project_path.redactions(), 2);

    let invalid_absolute = policy
        .redact("read /../escape", None, DisclosureClass::DerivedText)
        .unwrap();
    assert!(!invalid_absolute.value().contains("/../escape"));
    assert_eq!(invalid_absolute.redactions(), 1);

    assert!(
        policy
            .redact(
                &"x".repeat(MAX_INLINE_BYTES + 1),
                None,
                DisclosureClass::DerivedText,
            )
            .is_err()
    );
    assert!(
        policy
            .redact(
                &"x".repeat(MAX_INLINE_BYTES),
                None,
                DisclosureClass::DerivedText,
            )
            .is_ok()
    );

    let utf8_preview = policy
        .redact(
            &format!("x{}", "é".repeat(MAX_PREVIEW_BYTES)),
            None,
            DisclosureClass::DerivedText,
        )
        .unwrap();
    assert!(utf8_preview.value().len() <= MAX_PREVIEW_BYTES);
    assert!(
        utf8_preview
            .value()
            .is_char_boundary(utf8_preview.value().len())
    );

    let debug = format!("{redacted:?}");
    assert!(!debug.contains(redacted.value()));
    assert!(!debug.contains("AGBOX_FORBIDDEN_SECRET_6AF2C9"));
}

fn redacted(value: &str) -> RedactedText {
    redacted_as(value, DisclosureClass::DerivedText)
}

fn redacted_as(value: &str, disclosure_class: DisclosureClass) -> RedactedText {
    RedactionPolicy::new()
        .unwrap()
        .redact(value, None, disclosure_class)
        .unwrap()
}

fn valid_contract_draft() -> WorkContractRevisionDraft {
    contract_draft_as(DisclosureClass::DerivedText)
}

fn contract_draft_as(disclosure_class: DisclosureClass) -> WorkContractRevisionDraft {
    WorkContractRevisionDraft {
        contract_id: ContractId::parse_wire("contract_fixture").unwrap(),
        work_id: WorkId::for_test("work_fixture"),
        revision: 1,
        project_id: ProjectId::for_test("project_fixture"),
        objective: Some(redacted_as(
            "ship the bounded domain model",
            disclosure_class,
        )),
        status: WorkStatus::Active,
        summary: redacted_as("domain model in progress", disclosure_class),
        completed_steps: vec![redacted_as("identity kernel complete", disclosure_class)],
        next_actions: vec![redacted_as("validate all boundaries", disclosure_class)],
        blockers: Vec::new(),
        constraints: vec![redacted_as("retain evidence", disclosure_class)],
        completion_criteria: vec![redacted_as("all contract tests pass", disclosure_class)],
        artifacts: vec![redacted_as("crates/agbox-core", disclosure_class)],
        verification: vec![redacted_as("cargo test", disclosure_class)],
        source_runs: vec![AgentRunId::parse_wire("run_fixture").unwrap()],
        evidence_refs: vec![EvidenceId::for_test("ev_fixture")],
        confidence_basis_points: 9_000,
        created_at: OffsetDateTime::UNIX_EPOCH,
        extractor_version: "extractor-v1".into(),
        disclosure_class,
    }
}

#[allow(clippy::too_many_lines)]
fn assert_assertion_and_edge_boundaries(too_long: &str, evidence: &[EvidenceId]) {
    let invalid_assertions = [
        WorkAssertion::new(
            too_long.into(),
            redacted_as("value", DisclosureClass::ObservedState),
            Authority::ObservedState,
            PrivacyLabel::DerivedLocal,
            evidence.to_vec(),
            9_000,
        ),
        WorkAssertion::new(
            "summary".into(),
            redacted_as("value", DisclosureClass::ObservedState),
            Authority::ObservedState,
            PrivacyLabel::DerivedLocal,
            Vec::new(),
            9_000,
        ),
        WorkAssertion::new(
            "summary".into(),
            redacted_as("value", DisclosureClass::ObservedState),
            Authority::ObservedState,
            PrivacyLabel::DerivedLocal,
            vec![EvidenceId::for_test("ev"); MAX_CONTRACT_EVIDENCE_REFS + 1],
            9_000,
        ),
        WorkAssertion::new(
            "summary".into(),
            redacted_as("value", DisclosureClass::ObservedState),
            Authority::ObservedState,
            PrivacyLabel::DerivedLocal,
            evidence.to_vec(),
            10_001,
        ),
        WorkAssertion::instruction(
            redacted_as("continue", DisclosureClass::HumanIntent),
            Authority::HumanIntent,
            PrivacyLabel::PrivateLocal,
            Vec::new(),
        ),
    ];
    assert!(invalid_assertions.iter().all(Result::is_err));

    let valid_assertion = WorkAssertion::new(
        "next_action".into(),
        redacted_as("continue", DisclosureClass::HumanIntent),
        Authority::HumanIntent,
        PrivacyLabel::PrivateLocal,
        evidence.to_vec(),
        10_000,
    )
    .unwrap();
    let assertion_debug = format!("{valid_assertion:?}");
    assert!(!assertion_debug.contains("next_action"));
    assert!(!assertion_debug.contains("continue"));
    let assertion_wire = serde_json::to_value(valid_assertion).unwrap();
    let mut unsafe_instruction = assertion_wire.clone();
    unsafe_instruction["authority"] = serde_json::json!("tool_result");
    assert!(serde_json::from_value::<WorkAssertion>(unsafe_instruction).is_err());

    let mut raw_secret_assertion = assertion_wire.clone();
    raw_secret_assertion["value"] =
        serde_json::json!("api_key=wire-secret read /Users/alice/private");
    let rescanned_assertion =
        serde_json::from_value::<WorkAssertion>(raw_secret_assertion).unwrap();
    assert!(!rescanned_assertion.value().contains("wire-secret"));
    assert!(!rescanned_assertion.value().contains("/Users/alice"));

    let mut oversized_assertion = assertion_wire.clone();
    oversized_assertion["value"] = serde_json::json!("x".repeat(MAX_INLINE_BYTES + 1));
    assert!(serde_json::from_value::<WorkAssertion>(oversized_assertion).is_err());

    for forbidden_class in [
        DisclosureClass::Reasoning,
        DisclosureClass::SystemInstruction,
        DisclosureClass::DeveloperInstruction,
    ] {
        assert!(
            WorkAssertion::new(
                "summary".into(),
                redacted_as("must not transfer", forbidden_class),
                Authority::AgentStatement,
                PrivacyLabel::RestrictedLocal,
                evidence.to_vec(),
                9_000,
            )
            .is_err()
        );
        let mut forbidden_wire = assertion_wire.clone();
        forbidden_wire["disclosure_class"] = serde_json::to_value(forbidden_class).unwrap();
        assert!(serde_json::from_value::<WorkAssertion>(forbidden_wire).is_err());
    }

    let invalid_edges = [
        WorkEdge::new(
            WorkId::for_test("work_a"),
            WorkId::for_test("work_b"),
            WorkEdgeKind::DependsOn,
            Vec::new(),
        ),
        WorkEdge::new(
            WorkId::for_test("work_a"),
            WorkId::for_test("work_b"),
            WorkEdgeKind::DependsOn,
            vec![EvidenceId::for_test("ev"); MAX_CONTRACT_EVIDENCE_REFS + 1],
        ),
    ];
    assert!(invalid_edges.iter().all(Result::is_err));
}

#[allow(clippy::too_many_lines)]
fn assert_content_boundaries(too_long: &str) {
    assert!(
        ContentRef::bounded(
            too_long.into(),
            1,
            "text/plain",
            None,
            DisclosureClass::ObservedState,
            None,
        )
        .is_err()
    );
    assert!(
        ContentRef::bounded(
            "b3:ok".into(),
            1,
            too_long,
            None,
            DisclosureClass::ObservedState,
            None,
        )
        .is_err()
    );
    assert!(
        ContentRef::bounded(
            "b3:ok".into(),
            1,
            "text/plain",
            Some(LocalLocator::SourceRange {
                source_id: "source".into(),
                generation: 1,
                byte_start: 2,
                byte_end: 1,
            }),
            DisclosureClass::ObservedState,
            None,
        )
        .is_err()
    );
    let invalid_content = serde_json::json!({
        "hash": too_long,
        "byte_length": 1,
        "media_type": "text/plain",
        "local_locator": null,
        "redacted_excerpt": null,
        "truncated": false,
        "disclosure_class": "observed_state",
    });
    assert!(serde_json::from_value::<ContentRef>(invalid_content).is_err());

    let scanned_excerpt = RedactionPolicy::new()
        .unwrap()
        .redact(
            "api_key=constructor-secret",
            None,
            DisclosureClass::ToolResult,
        )
        .unwrap();
    let constructed_content = ContentRef::bounded(
        "b3:scanned".into(),
        24,
        "text/plain",
        None,
        DisclosureClass::ToolResult,
        Some(scanned_excerpt),
    )
    .unwrap();
    assert!(
        !constructed_content
            .redacted_excerpt()
            .unwrap()
            .contains("constructor-secret")
    );

    let raw_wire_excerpt = serde_json::json!({
        "hash": "b3:wire",
        "byte_length": 24,
        "media_type": "text/plain",
        "local_locator": null,
        "redacted_excerpt": "api_key=wire-secret",
        "truncated": false,
        "disclosure_class": "tool_result",
    });
    let rescanned_content = serde_json::from_value::<ContentRef>(raw_wire_excerpt).unwrap();
    assert!(
        !rescanned_content
            .redacted_excerpt()
            .unwrap()
            .contains("wire-secret")
    );
    assert!(
        rescanned_content
            .redacted_excerpt()
            .unwrap()
            .contains("[REDACTED_SECRET]")
    );

    for forbidden_class in [
        DisclosureClass::Reasoning,
        DisclosureClass::SystemInstruction,
        DisclosureClass::DeveloperInstruction,
    ] {
        assert!(
            ContentRef::bounded(
                "b3:forbidden-without-excerpt".into(),
                0,
                "application/octet-stream",
                None,
                forbidden_class,
                None,
            )
            .is_err()
        );
        let forbidden_excerpt = redacted_as("must not transfer", forbidden_class);
        assert!(
            ContentRef::bounded(
                "b3:forbidden".into(),
                17,
                "text/plain",
                None,
                forbidden_class,
                Some(forbidden_excerpt),
            )
            .is_err()
        );
        let forbidden_wire = serde_json::json!({
            "hash": "b3:forbidden-wire",
            "byte_length": 17,
            "media_type": "text/plain",
            "local_locator": null,
            "redacted_excerpt": "must not transfer",
            "truncated": false,
            "disclosure_class": forbidden_class,
        });
        assert!(serde_json::from_value::<ContentRef>(forbidden_wire).is_err());

        let forbidden_wire_without_excerpt = serde_json::json!({
            "hash": "b3:forbidden-wire-without-excerpt",
            "byte_length": 0,
            "media_type": "application/octet-stream",
            "local_locator": null,
            "redacted_excerpt": null,
            "truncated": false,
            "disclosure_class": forbidden_class,
        });
        assert!(serde_json::from_value::<ContentRef>(forbidden_wire_without_excerpt).is_err());
    }

    assert!(
        ContentRef::bounded(
            "b3:mismatch".into(),
            17,
            "text/plain",
            None,
            DisclosureClass::HumanIntent,
            Some(redacted_as("agent text", DisclosureClass::AgentStatement)),
        )
        .is_err()
    );
}

type WireTextMutation = fn(&mut serde_json::Value, String);
type ListMutation = fn(&mut WorkContractRevisionDraft, Vec<RedactedText>);

fn assert_contract_field_boundaries() {
    let text_cases: [(&str, WireTextMutation); 10] = [
        ("objective", |wire, value| {
            wire["objective"] = serde_json::json!(value);
        }),
        ("summary", |wire, value| {
            wire["summary"] = serde_json::json!(value);
        }),
        ("completed_steps", |wire, value| {
            wire["completed_steps"] = serde_json::json!([value]);
        }),
        ("next_actions", |wire, value| {
            wire["next_actions"] = serde_json::json!([value]);
        }),
        ("blockers", |wire, value| {
            wire["blockers"] = serde_json::json!([value]);
        }),
        ("constraints", |wire, value| {
            wire["constraints"] = serde_json::json!([value]);
        }),
        ("completion_criteria", |wire, value| {
            wire["completion_criteria"] = serde_json::json!([value]);
        }),
        ("artifacts", |wire, value| {
            wire["artifacts"] = serde_json::json!([value]);
        }),
        ("verification", |wire, value| {
            wire["verification"] = serde_json::json!([value]);
        }),
        ("extractor_version", |wire, value| {
            wire["extractor_version"] = serde_json::json!(value);
        }),
    ];
    for (name, mutate) in text_cases {
        let revision = WorkContractRevision::new(valid_contract_draft()).unwrap();
        let mut wire = serde_json::to_value(revision).unwrap();
        mutate(&mut wire, "x".repeat(MAX_INLINE_BYTES + 1));
        assert!(
            serde_json::from_value::<WorkContractRevision>(wire).is_err(),
            "{name} accepted an oversized wire string"
        );
    }

    let mut oversized_metadata = valid_contract_draft();
    oversized_metadata.extractor_version = "x".repeat(MAX_INLINE_BYTES + 1);
    assert!(WorkContractRevision::new(oversized_metadata).is_err());

    let list_cases: [(&str, ListMutation); 7] = [
        ("completed_steps", |draft, value| {
            draft.completed_steps = value;
        }),
        ("next_actions", |draft, value| draft.next_actions = value),
        ("blockers", |draft, value| draft.blockers = value),
        ("constraints", |draft, value| draft.constraints = value),
        ("completion_criteria", |draft, value| {
            draft.completion_criteria = value;
        }),
        ("artifacts", |draft, value| draft.artifacts = value),
        ("verification", |draft, value| draft.verification = value),
    ];
    for (name, mutate) in list_cases {
        let mut draft = valid_contract_draft();
        mutate(
            &mut draft,
            vec![redacted(""); MAX_CONTRACT_ITEMS_PER_FIELD + 1],
        );
        assert!(
            WorkContractRevision::new(draft).is_err(),
            "{name} accepted too many items"
        );
    }
}

fn assert_contract_global_boundaries() {
    let mut too_many_source_runs = valid_contract_draft();
    too_many_source_runs.source_runs =
        vec![AgentRunId::parse_wire("run_fixture").unwrap(); MAX_CONTRACT_SOURCE_RUNS + 1];
    assert!(WorkContractRevision::new(too_many_source_runs).is_err());

    let mut missing_evidence = valid_contract_draft();
    missing_evidence.evidence_refs.clear();
    assert!(WorkContractRevision::new(missing_evidence).is_err());

    let mut too_many_evidence = valid_contract_draft();
    too_many_evidence.evidence_refs =
        vec![EvidenceId::for_test("ev_fixture"); MAX_CONTRACT_EVIDENCE_REFS + 1];
    assert!(WorkContractRevision::new(too_many_evidence).is_err());

    let mut invalid_confidence = valid_contract_draft();
    invalid_confidence.confidence_basis_points = 10_001;
    assert!(WorkContractRevision::new(invalid_confidence).is_err());

    let mut oversized_serialization = valid_contract_draft();
    let full_preview = redacted(&"a".repeat(MAX_PREVIEW_BYTES));
    oversized_serialization.completed_steps =
        vec![full_preview.clone(); MAX_CONTRACT_ITEMS_PER_FIELD];
    oversized_serialization.next_actions = vec![full_preview.clone(); MAX_CONTRACT_ITEMS_PER_FIELD];
    oversized_serialization.blockers = vec![full_preview.clone(); MAX_CONTRACT_ITEMS_PER_FIELD];
    oversized_serialization.constraints = vec![full_preview.clone(); MAX_CONTRACT_ITEMS_PER_FIELD];
    oversized_serialization.completion_criteria = vec![full_preview; MAX_CONTRACT_ITEMS_PER_FIELD];
    assert!(WorkContractRevision::new(oversized_serialization).is_err());

    let mut scanned_draft = valid_contract_draft();
    scanned_draft.summary = redacted("api_key=constructor-secret read /Users/alice/private");
    let scanned_revision = WorkContractRevision::new(scanned_draft).unwrap();
    assert!(!scanned_revision.summary().contains("constructor-secret"));
    assert!(!scanned_revision.summary().contains("/Users/alice"));

    let valid_revision = WorkContractRevision::new(valid_contract_draft()).unwrap();
    valid_revision.validate().unwrap();
    let revision_debug = format!("{valid_revision:?}");
    assert!(!revision_debug.contains("ship the bounded domain model"));
    assert!(!revision_debug.contains("domain model in progress"));
    assert!(!revision_debug.contains("extractor-v1"));
    let valid_wire = serde_json::to_value(valid_revision).unwrap();
    for forbidden_class in [
        DisclosureClass::Reasoning,
        DisclosureClass::SystemInstruction,
        DisclosureClass::DeveloperInstruction,
    ] {
        assert!(WorkContractRevision::new(contract_draft_as(forbidden_class)).is_err());
        let mut forbidden_wire = valid_wire.clone();
        forbidden_wire["disclosure_class"] = serde_json::to_value(forbidden_class).unwrap();
        assert!(serde_json::from_value::<WorkContractRevision>(forbidden_wire).is_err());
    }
    let mut mismatched_draft = valid_contract_draft();
    mismatched_draft.disclosure_class = DisclosureClass::HumanIntent;
    assert!(WorkContractRevision::new(mismatched_draft).is_err());

    let mut raw_secret_wire = valid_wire.clone();
    raw_secret_wire["summary"] = serde_json::json!("api_key=wire-secret read /Users/alice/private");
    raw_secret_wire["next_actions"] = serde_json::json!(["password=wire-list-secret"]);
    let rescanned_revision =
        serde_json::from_value::<WorkContractRevision>(raw_secret_wire).unwrap();
    assert!(!rescanned_revision.summary().contains("wire-secret"));
    assert!(!rescanned_revision.summary().contains("/Users/alice"));
    assert!(!rescanned_revision.next_actions()[0].contains("wire-list-secret"));

    let mut oversized_raw_wire = valid_wire.clone();
    let large_raw_value = "x".repeat(60 * 1024);
    oversized_raw_wire["objective"] = serde_json::json!(large_raw_value);
    oversized_raw_wire["summary"] = serde_json::json!(large_raw_value);
    for field in [
        "completed_steps",
        "next_actions",
        "blockers",
        "constraints",
        "completion_criteria",
        "artifacts",
        "verification",
    ] {
        oversized_raw_wire[field] = serde_json::json!([large_raw_value]);
    }
    assert!(serde_json::to_vec(&oversized_raw_wire).unwrap().len() > MAX_CONTRACT_SERIALIZED_BYTES);
    assert!(serde_json::from_value::<WorkContractRevision>(oversized_raw_wire).is_err());

    let mut invalid_wire = valid_wire;
    invalid_wire["confidence_basis_points"] = serde_json::json!(10_001);
    assert!(serde_json::from_value::<WorkContractRevision>(invalid_wire).is_err());
}

fn assert_exact_limits_are_accepted() {
    let evidence_at_limit = vec![EvidenceId::for_test("ev_limit"); MAX_CONTRACT_EVIDENCE_REFS];
    assert!(
        WorkAssertion::new(
            "f".repeat(MAX_INLINE_BYTES),
            redacted_as("value", DisclosureClass::ObservedState),
            Authority::ObservedState,
            PrivacyLabel::DerivedLocal,
            evidence_at_limit.clone(),
            10_000,
        )
        .is_ok()
    );
    assert!(
        WorkEdge::new(
            WorkId::for_test("work_a"),
            WorkId::for_test("work_b"),
            WorkEdgeKind::DependsOn,
            evidence_at_limit.clone(),
        )
        .is_ok()
    );

    let mut contract = valid_contract_draft();
    contract.completed_steps = vec![redacted(""); MAX_CONTRACT_ITEMS_PER_FIELD];
    contract.source_runs =
        vec![AgentRunId::parse_wire("run_limit").unwrap(); MAX_CONTRACT_SOURCE_RUNS];
    contract.evidence_refs = evidence_at_limit;
    contract.confidence_basis_points = 10_000;
    contract.extractor_version = "v".repeat(MAX_INLINE_BYTES);
    assert!(WorkContractRevision::new(contract).is_ok());

    let excerpt = redacted(&"e".repeat(MAX_PREVIEW_BYTES));
    assert!(
        ContentRef::bounded(
            "h".repeat(128),
            MAX_INLINE_BYTES as u64,
            "m".repeat(128),
            Some(LocalLocator::SourceRange {
                source_id: "s".repeat(128),
                generation: 1,
                byte_start: 0,
                byte_end: MAX_INLINE_BYTES as u64,
            }),
            DisclosureClass::DerivedText,
            Some(excerpt),
        )
        .is_ok()
    );

    let mut source = valid_source_ref_draft();
    source.native_session_id = "s".repeat(MAX_INLINE_BYTES);
    assert!(SourceRef::new(source).is_ok());

    let mut event = ActivityEventV1::fixture_message_draft();
    event.turn_id = Some("t".repeat(MAX_INLINE_BYTES));
    event.payload = EventPayload::AgentStarted {
        native_agent_id: "a".repeat(MAX_INLINE_BYTES),
    };
    assert!(ActivityEventV1::new(event).is_ok());
}

#[test]
fn bounded_contracts_reject_every_invalid_limit_and_unsafe_wire_shape() {
    let too_long = "x".repeat(MAX_INLINE_BYTES + 1);
    let evidence = vec![EvidenceId::for_test("ev_fixture")];

    assert_assertion_and_edge_boundaries(&too_long, &evidence);
    assert_content_boundaries(&too_long);
    assert_contract_field_boundaries();
    assert_contract_global_boundaries();
    assert_exact_limits_are_accepted();
}

#[test]
fn standalone_event_payload_wire_ingress_rejects_oversized_text() {
    let wire = serde_json::json!({
        "kind": "agent.started",
        "native_agent_id": "x".repeat(MAX_INLINE_BYTES + 1),
    });
    assert!(serde_json::from_value::<EventPayload>(wire).is_err());
}
