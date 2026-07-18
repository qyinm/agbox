#![allow(clippy::unwrap_used)]

use std::collections::BTreeSet;

use agbox_core::{EventId, ProjectId, SessionId, WorkId, WorkStatus};
use agbox_workgraph::{
    ContractField, ProvisionalContractBuilder, ReducedFact,
    test_support::facts_for_active_parser_work,
};
use time::macros::datetime;

fn event_id(value: &str) -> EventId {
    EventId::parse_wire(value).unwrap()
}

fn project() -> ProjectId {
    ProjectId::for_test("project-a")
}

fn session() -> SessionId {
    SessionId::parse_wire("session-a").unwrap()
}

fn artifact(path: Option<&str>, evidence: &str) -> ReducedFact {
    ReducedFact::Artifact {
        project_id: project(),
        session_id: agbox_core::SessionId::parse_wire("session-a").unwrap(),
        path_hash: format!("hash-{evidence}"),
        project_relative_path: path.map(str::to_owned),
        operation: "update".into(),
        content_hash: None,
        observed_at: datetime!(2026-07-17 12:00 UTC),
        evidence: event_id(evidence),
    }
}

fn verification(succeeded: bool, command: Option<&str>, evidence: &str) -> ReducedFact {
    action_verification(
        "verify",
        succeeded,
        command,
        datetime!(2026-07-17 12:01 UTC),
        evidence,
    )
}

fn action_verification(
    action_id: &str,
    succeeded: bool,
    command: Option<&str>,
    observed_at: time::OffsetDateTime,
    evidence: &str,
) -> ReducedFact {
    ReducedFact::Verification {
        project_id: project(),
        session_id: session(),
        native_action_id: action_id.into(),
        command: command.map(str::to_owned),
        succeeded,
        basis: "structured_tool_result",
        observed_at,
        evidence: event_id(evidence),
    }
}

fn observed_finish(
    action_id: &str,
    succeeded: bool,
    observed_at: time::OffsetDateTime,
    evidence: &str,
) -> ReducedFact {
    ReducedFact::ActionFinishedObserved {
        project_id: project(),
        session_id: session(),
        native_action_id: action_id.into(),
        succeeded,
        observed_at,
        evidence: event_id(evidence),
    }
}

fn action_request(action_id: &str, input: &str, evidence: &str) -> ReducedFact {
    ReducedFact::ActionRequested {
        project_id: project(),
        session_id: session(),
        native_action_id: action_id.into(),
        tool_name: "shell".into(),
        input_hash: format!("hash-{evidence}"),
        redacted_input: Some(input.into()),
        evidence: event_id(evidence),
    }
}

fn agent_run_finished(evidence: &str) -> ReducedFact {
    ReducedFact::AgentRunFinished {
        project_id: project(),
        session_id: session(),
        provider: agbox_core::Provider::Codex,
        native_agent_id: "run-a".into(),
        succeeded: true,
        observed_at: datetime!(2026-07-17 12:03 UTC),
        evidence: event_id(evidence),
    }
}

#[test]
fn provisional_contract_is_useful_without_a_model() {
    let facts = facts_for_active_parser_work();
    let contract = ProvisionalContractBuilder::new("deterministic-v1")
        .build(None, &facts)
        .unwrap();

    assert_eq!(contract.revision, 1);
    assert_eq!(contract.status, WorkStatus::Active);
    assert!(!contract.summary.is_empty());
    assert!(!contract.next_actions.is_empty());
    assert!(!contract.evidence_refs.is_empty());
    assert!(!contract.material_content_hash.is_empty());
}

#[test]
fn every_non_empty_semantic_field_has_evidence_before_serialization() {
    let contract = ProvisionalContractBuilder::new("deterministic-v1")
        .build(None, &facts_for_active_parser_work())
        .unwrap();

    for field in contract.non_empty_fields() {
        assert!(
            contract
                .field_evidence
                .get(&field)
                .is_some_and(|references| !references.is_empty()),
            "{field:?} must cite evidence"
        );
    }
    assert!(contract.field_evidence.contains_key(&ContractField::Status));
    let encoded = serde_json::to_vec(&contract).unwrap();
    let decoded = serde_json::from_slice(&encoded).unwrap();
    assert_eq!(contract, decoded);
}

#[test]
fn failed_structured_tool_result_blocks_and_success_completes() {
    let blocked = ProvisionalContractBuilder::new("deterministic-v1")
        .build(
            None,
            &[verification(false, Some("cargo test"), "evt-failed")],
        )
        .unwrap();
    assert_eq!(blocked.status, WorkStatus::Blocked);
    assert_eq!(blocked.blockers, ["cargo test"]);
    assert_eq!(blocked.verification, ["cargo test"]);

    let completed = ProvisionalContractBuilder::new("deterministic-v1")
        .build(
            None,
            &[verification(true, Some("cargo test"), "evt-succeeded")],
        )
        .unwrap();
    assert_eq!(completed.status, WorkStatus::Completed);
    assert_eq!(completed.completed_steps, ["cargo test"]);
    assert_eq!(completed.verification, ["cargo test"]);
}

#[test]
fn explicit_human_abandonment_has_status_precedence() {
    let facts = vec![
        verification(true, Some("cargo test"), "evt-before-abandonment"),
        ReducedFact::HumanObjective {
            project_id: project(),
            content_hash: "abandonment-hash".into(),
            redacted_text: Some("Abandon this work".into()),
            observed_at: datetime!(2026-07-17 12:02 UTC),
            evidence: event_id("evt-abandonment"),
        },
    ];
    let contract = ProvisionalContractBuilder::new("deterministic-v1")
        .build(None, &facts)
        .unwrap();

    assert_eq!(contract.status, WorkStatus::Abandoned);
}

#[test]
fn new_observed_activity_reopens_a_completed_contract() {
    let builder =
        ProvisionalContractBuilder::new("deterministic-v1").for_work(WorkId::for_test("work-a"));
    let completed = builder
        .build(
            None,
            &[verification(true, Some("cargo test"), "evt-completed")],
        )
        .unwrap();
    let reopened = builder
        .build(
            Some(&completed),
            &[artifact(Some("src/parser.rs"), "evt-reopen")],
        )
        .unwrap();

    assert_eq!(completed.status, WorkStatus::Completed);
    assert_eq!(reopened.status, WorkStatus::Active);
    assert_eq!(reopened.revision, 2);
    assert_eq!(reopened.artifacts, ["src/parser.rs"]);
}

#[test]
fn reopened_completion_stays_active_until_a_new_authoritative_success() {
    let neutral_facts = vec![
        agent_run_finished("evt-run-finished"),
        ReducedFact::SessionContext {
            project_id: project(),
            session_id: session(),
            provider: agbox_core::Provider::Codex,
            branch_hash: Some("b3:main".into()),
            observed_at: datetime!(2026-07-17 12:03 UTC),
            evidence: event_id("evt-session-context"),
        },
        ReducedFact::AgentStatement {
            project_id: project(),
            content_hash: "b3:waiting".into(),
            redacted_text: Some("Waiting for another verification".into()),
            observed_at: datetime!(2026-07-17 12:03 UTC),
            evidence: event_id("evt-agent-statement"),
        },
        observed_finish(
            "verify",
            false,
            datetime!(2026-07-17 12:03 UTC),
            "evt-mismatched-finish",
        ),
    ];

    for neutral_fact in neutral_facts {
        let builder = ProvisionalContractBuilder::new("deterministic-v1")
            .for_work(WorkId::for_test("work-a"));
        let completed = builder
            .build(
                None,
                &[action_verification(
                    "verify",
                    true,
                    Some("cargo test"),
                    datetime!(2026-07-17 12:01 UTC),
                    "evt-completed",
                )],
            )
            .unwrap();
        let reopened = builder
            .build(
                Some(&completed),
                &[artifact(Some("src/parser.rs"), "evt-reopen")],
            )
            .unwrap();
        let neutral = builder
            .build(Some(&reopened), std::slice::from_ref(&neutral_fact))
            .unwrap();
        let reverified = builder
            .build(
                Some(&neutral),
                &[action_verification(
                    "verify",
                    true,
                    Some("cargo test"),
                    datetime!(2026-07-17 12:04 UTC),
                    "evt-reverified",
                )],
            )
            .unwrap();

        assert_eq!(completed.status, WorkStatus::Completed);
        assert_eq!(reopened.status, WorkStatus::Active);
        assert_eq!(neutral.status, WorkStatus::Active, "{neutral_fact:?}");
        assert_eq!(reverified.status, WorkStatus::Completed);
        assert_eq!(reverified.revision, neutral.revision + 1);
    }
}

#[test]
fn replay_does_not_create_a_duplicate_revision() {
    let facts = facts_for_active_parser_work();
    let builder = ProvisionalContractBuilder::new("deterministic-v1");
    let first = builder.build(None, &facts).unwrap();
    let replay = builder.build(Some(&first), &facts).unwrap();

    assert_eq!(replay.revision, first.revision);
    assert_eq!(replay.material_content_hash, first.material_content_hash);
    assert_eq!(replay, first);
}

#[test]
fn hash_only_facts_support_provenance_without_generating_prose() {
    let facts = vec![
        ReducedFact::HumanObjective {
            project_id: project(),
            content_hash: "private-objective-hash".into(),
            redacted_text: None,
            observed_at: datetime!(2026-07-17 12:00 UTC),
            evidence: event_id("evt-objective-hash"),
        },
        ReducedFact::AgentStatement {
            project_id: project(),
            content_hash: "private-summary-hash".into(),
            redacted_text: None,
            observed_at: datetime!(2026-07-17 12:00 UTC),
            evidence: event_id("evt-summary-hash"),
        },
        artifact(None, "evt-artifact-hash"),
        verification(true, None, "evt-verification-hash"),
    ];
    let contract = ProvisionalContractBuilder::new("deterministic-v1")
        .build(None, &facts)
        .unwrap();

    assert!(contract.objective.is_none());
    assert!(contract.summary.is_empty());
    assert!(contract.artifacts.is_empty());
    assert!(contract.verification.is_empty());
    assert!(contract.completed_steps.is_empty());
    assert_eq!(
        contract
            .evidence_refs
            .iter()
            .cloned()
            .collect::<BTreeSet<_>>(),
        facts
            .iter()
            .map(|fact| fact.evidence_id().clone())
            .collect()
    );
}

#[test]
fn bounded_excerpts_are_redacted_again_at_the_contract_boundary() {
    let facts = vec![ReducedFact::AgentStatement {
        project_id: project(),
        content_hash: "hash".into(),
        redacted_text: Some("token=PRIVATE_VALUE /Users/alice/project".into()),
        observed_at: datetime!(2026-07-17 12:00 UTC),
        evidence: event_id("evt-sensitive"),
    }];
    let contract = ProvisionalContractBuilder::new("deterministic-v1")
        .build(None, &facts)
        .unwrap();

    assert!(!contract.summary.contains("PRIVATE_VALUE"));
    assert!(!contract.summary.contains("/Users/alice"));
}

#[test]
fn latest_human_and_agent_text_uses_observation_chronology_not_event_id_order() {
    let facts = vec![
        ReducedFact::HumanObjective {
            project_id: project(),
            content_hash: "old-objective".into(),
            redacted_text: Some("Old objective".into()),
            observed_at: datetime!(2026-07-17 12:00 UTC),
            evidence: event_id("evt-z-old-objective"),
        },
        ReducedFact::HumanObjective {
            project_id: project(),
            content_hash: "new-objective".into(),
            redacted_text: Some("New objective".into()),
            observed_at: datetime!(2026-07-17 12:01 UTC),
            evidence: event_id("evt-a-new-objective"),
        },
        ReducedFact::HumanConstraint {
            project_id: project(),
            content_hash: "old-constraint".into(),
            redacted_text: Some("Never use the old path".into()),
            observed_at: datetime!(2026-07-17 12:00 UTC),
            evidence: event_id("evt-z-old-constraint"),
        },
        ReducedFact::HumanConstraint {
            project_id: project(),
            content_hash: "new-constraint".into(),
            redacted_text: Some("Never expose secrets".into()),
            observed_at: datetime!(2026-07-17 12:01 UTC),
            evidence: event_id("evt-a-new-constraint"),
        },
        ReducedFact::AgentStatement {
            project_id: project(),
            content_hash: "old-summary".into(),
            redacted_text: Some("Old summary".into()),
            observed_at: datetime!(2026-07-17 12:00 UTC),
            evidence: event_id("evt-z-old-summary"),
        },
        ReducedFact::AgentStatement {
            project_id: project(),
            content_hash: "new-summary".into(),
            redacted_text: Some("New summary".into()),
            observed_at: datetime!(2026-07-17 12:01 UTC),
            evidence: event_id("evt-a-new-summary"),
        },
    ];

    let contract = ProvisionalContractBuilder::new("deterministic-v1")
        .build(None, &facts)
        .unwrap();
    assert_eq!(contract.objective.as_deref(), Some("New objective"));
    assert_eq!(contract.constraints, ["Never expose secrets"]);
    assert_eq!(contract.summary, "New summary");
}

#[test]
fn observed_finish_never_supersedes_authoritative_success_or_failure() {
    for succeeded in [false, true] {
        let command = if succeeded {
            "cargo test"
        } else {
            "cargo clippy"
        };
        let authoritative_facts = vec![
            action_request("same-action", command, "evt-request"),
            action_verification(
                "same-action",
                succeeded,
                Some(command),
                datetime!(2026-07-17 12:01 UTC),
                "evt-authoritative",
            ),
        ];
        let mismatched = observed_finish(
            "same-action",
            !succeeded,
            datetime!(2026-07-17 12:02 UTC),
            "evt-mismatched",
        );
        let builder = ProvisionalContractBuilder::new("deterministic-v1");
        let authoritative = builder.build(None, &authoritative_facts).unwrap();
        let later_mismatch = builder
            .build(Some(&authoritative), std::slice::from_ref(&mismatched))
            .unwrap();
        let mut aggregate_facts = authoritative_facts;
        aggregate_facts.push(mismatched);
        let aggregate = builder.build(None, &aggregate_facts).unwrap();

        for contract in [&later_mismatch, &aggregate] {
            assert_eq!(
                contract.status,
                if succeeded {
                    WorkStatus::Completed
                } else {
                    WorkStatus::Blocked
                }
            );
            assert_eq!(contract.verification, [command]);
            assert!(contract.next_actions.is_empty());
            if succeeded {
                assert_eq!(contract.completed_steps, [command]);
            } else {
                assert_eq!(contract.blockers, [command]);
            }
        }
        assert_eq!(later_mismatch.revision, authoritative.revision);
        assert_eq!(
            later_mismatch.material_content_hash,
            authoritative.material_content_hash
        );
    }
}

#[test]
fn mixed_action_outcomes_keep_failed_blockers_in_both_action_id_orders() {
    for (failed_id, successful_id) in [("a", "z"), ("z", "a")] {
        let facts = vec![
            action_verification(
                failed_id,
                false,
                Some("cargo test failing-suite"),
                datetime!(2026-07-17 12:01 UTC),
                &format!("evt-failed-{failed_id}"),
            ),
            action_verification(
                successful_id,
                true,
                Some("cargo test passing-suite"),
                datetime!(2026-07-17 12:01 UTC),
                &format!("evt-success-{successful_id}"),
            ),
        ];
        let contract = ProvisionalContractBuilder::new("deterministic-v1")
            .build(None, &facts)
            .unwrap();
        assert_eq!(contract.status, WorkStatus::Blocked);
        assert_eq!(contract.blockers, ["cargo test failing-suite"]);
        assert_eq!(contract.completed_steps, ["cargo test passing-suite"]);
    }
}

#[test]
fn finishing_one_action_removes_only_its_next_action_across_revisions() {
    let builder = ProvisionalContractBuilder::new("deterministic-v1");
    let requested = builder
        .build(
            None,
            &[
                action_request("finished", "cargo test finished", "evt-request-finished"),
                action_request("pending", "cargo test pending", "evt-request-pending"),
            ],
        )
        .unwrap();
    let finished = builder
        .build(
            Some(&requested),
            &[action_verification(
                "finished",
                true,
                Some("cargo test finished"),
                datetime!(2026-07-17 12:01 UTC),
                "evt-finish",
            )],
        )
        .unwrap();

    assert_eq!(requested.next_actions.len(), 2);
    assert_eq!(finished.next_actions, ["cargo test pending"]);
    assert_eq!(finished.completed_steps, ["cargo test finished"]);
}

#[test]
fn identical_large_aggregate_replay_is_deduplicated_beyond_evidence_cap() {
    let mut facts = (0..=agbox_core::limits::MAX_CONTRACT_EVIDENCE_REFS)
        .map(|index| {
            artifact(
                Some(&format!("src/generated-{index}.rs")),
                &format!("evt-{index:03}"),
            )
        })
        .collect::<Vec<_>>();
    facts.push(action_verification(
        "verify-large",
        true,
        Some("cargo test"),
        datetime!(2026-07-17 12:02 UTC),
        "evt-verification-large",
    ));
    let builder = ProvisionalContractBuilder::new("deterministic-v1");
    let first = builder.build(None, &facts).unwrap();
    let replay = builder.build(Some(&first), &facts).unwrap();

    assert!(first.evidence_truncated);
    assert_eq!(first.status, WorkStatus::Completed);
    assert_eq!(replay.revision, first.revision);
    assert_eq!(replay.status, first.status);
    assert_eq!(replay.material_content_hash, first.material_content_hash);
}

#[test]
fn contract_content_converges_across_one_hundred_deterministic_permutations() {
    let facts = facts_for_active_parser_work();
    let expected = ProvisionalContractBuilder::new("deterministic-v1")
        .for_work(WorkId::for_test("work-convergent"))
        .build(None, &facts)
        .unwrap();
    let mut order = (0..facts.len()).collect::<Vec<_>>();

    for _ in 0..100 {
        let reordered = order
            .iter()
            .map(|index| facts[*index].clone())
            .collect::<Vec<_>>();
        let actual = ProvisionalContractBuilder::new("deterministic-v1")
            .for_work(WorkId::for_test("work-convergent"))
            .build(None, &reordered)
            .unwrap();
        assert_eq!(actual.status, expected.status);
        assert_eq!(actual.material_content_hash, expected.material_content_hash);
        assert_eq!(actual.objective, expected.objective);
        assert_eq!(actual.summary, expected.summary);
        assert_eq!(actual.next_actions, expected.next_actions);
        assert!(next_permutation(&mut order));
    }
}

fn next_permutation(values: &mut [usize]) -> bool {
    let Some(pivot) = (0..values.len().saturating_sub(1))
        .rev()
        .find(|index| values[*index] < values[*index + 1])
    else {
        return false;
    };
    let successor = (pivot + 1..values.len())
        .rev()
        .find(|index| values[*index] > values[pivot])
        .unwrap();
    values.swap(pivot, successor);
    values[pivot + 1..].reverse();
    true
}
