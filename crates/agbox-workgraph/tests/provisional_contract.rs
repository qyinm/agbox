#![allow(clippy::unwrap_used)]

use std::collections::BTreeSet;

use agbox_core::{EventId, ProjectId, WorkId, WorkStatus};
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
    ReducedFact::Verification {
        project_id: project(),
        session_id: agbox_core::SessionId::parse_wire("session-a").unwrap(),
        native_action_id: "verify".into(),
        command: command.map(str::to_owned),
        succeeded,
        basis: "structured_tool_result",
        observed_at: datetime!(2026-07-17 12:01 UTC),
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
            evidence: event_id("evt-objective-hash"),
        },
        ReducedFact::AgentStatement {
            project_id: project(),
            content_hash: "private-summary-hash".into(),
            redacted_text: None,
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
        evidence: event_id("evt-sensitive"),
    }];
    let contract = ProvisionalContractBuilder::new("deterministic-v1")
        .build(None, &facts)
        .unwrap();

    assert!(!contract.summary.contains("PRIVATE_VALUE"));
    assert!(!contract.summary.contains("/Users/alice"));
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
