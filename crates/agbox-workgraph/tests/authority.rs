#![allow(clippy::unwrap_used)]

use agbox_core::{Authority, DisclosureClass, EvidenceId, ProjectId};
use agbox_workgraph::{
    AuthorityEvidence, EndpointPolicy, ProposedAssertion, ProposedAssertions, SemanticPolicy,
};

#[test]
fn public_endpoint_is_rejected_even_when_loopback_name_is_spoofed() {
    assert!(EndpointPolicy::parse("http://localhost.example/v1").is_err());
    assert!(EndpointPolicy::parse("http://127.0.0.1.evil.example/v1").is_err());
    assert!(EndpointPolicy::parse("http://127.0.0.1:11434/v1").is_ok());
}

#[test]
fn model_cannot_relabel_tool_evidence_as_human_intent() {
    let proposals = ProposedAssertions {
        assertions: vec![ProposedAssertion {
            field: "objective".into(),
            value: "do the thing".into(),
            authority: Authority::HumanIntent,
            evidence_refs: vec![],
            confidence_basis_points: 10_000,
        }],
    };
    assert!(SemanticPolicy::default().validate(proposals).is_ok());
}

#[test]
fn instruction_requires_exact_human_evidence_and_restricted_evidence_is_not_retained() {
    let evidence_id = EvidenceId::for_test("human-1");
    let policy = SemanticPolicy::with_evidence([AuthorityEvidence::new(
        evidence_id.clone(),
        Authority::HumanIntent,
        DisclosureClass::HumanIntent,
        "ship the parser",
    )]);
    let accepted = policy
        .validate(ProposedAssertions {
            assertions: vec![ProposedAssertion {
                field: "objective".into(),
                value: "ship   the parser".into(),
                authority: Authority::ModelInference,
                evidence_refs: vec![evidence_id.clone()],
                confidence_basis_points: 9_000,
            }],
        })
        .unwrap();
    assert_eq!(accepted.assertions.len(), 1);
    assert_eq!(accepted.assertions[0].authority, Authority::HumanIntent);
}

#[test]
fn verification_requires_matching_structured_evidence_and_project_scope() {
    let project_id = ProjectId::for_test("project-a");
    let tool_id = EvidenceId::for_test("tool-1");
    let foreign_id = EvidenceId::for_test("foreign-1");
    let policy = SemanticPolicy::for_project(
        project_id,
        [
            AuthorityEvidence::in_project(
                ProjectId::for_test("project-a"),
                tool_id.clone(),
                Authority::ToolResult,
                DisclosureClass::ToolResult,
                "cargo test passed",
            ),
            AuthorityEvidence::in_project(
                ProjectId::for_test("project-b"),
                foreign_id.clone(),
                Authority::ToolResult,
                DisclosureClass::ToolResult,
                "foreign result",
            ),
        ],
    );
    let accepted = policy
        .validate(ProposedAssertions {
            assertions: vec![ProposedAssertion {
                field: "verification".into(),
                value: "cargo test passed".into(),
                authority: Authority::HumanIntent,
                evidence_refs: vec![tool_id],
                confidence_basis_points: 10_000,
            }],
        })
        .unwrap();
    assert_eq!(accepted.assertions.len(), 1);

    let rejected = policy
        .validate(ProposedAssertions {
            assertions: vec![ProposedAssertion {
                field: "verification".into(),
                value: "foreign result".into(),
                authority: Authority::ToolResult,
                evidence_refs: vec![foreign_id],
                confidence_basis_points: 10_000,
            }],
        })
        .unwrap();
    assert!(rejected.assertions.is_empty());

    let mismatched = SemanticPolicy::with_evidence([AuthorityEvidence::new(
        EvidenceId::for_test("wrong-class"),
        Authority::ToolResult,
        DisclosureClass::AgentStatement,
        "not structured",
    )]);
    let rejected = mismatched
        .validate(ProposedAssertions {
            assertions: vec![ProposedAssertion {
                field: "verification".into(),
                value: "not structured".into(),
                authority: Authority::ToolResult,
                evidence_refs: vec![EvidenceId::for_test("wrong-class")],
                confidence_basis_points: 10_000,
            }],
        })
        .unwrap();
    assert!(rejected.assertions.is_empty());
}

#[test]
fn evidence_reference_lists_are_bounded_before_policy_resolution() {
    let evidence_refs = (0..=agbox_core::limits::MAX_CONTRACT_EVIDENCE_REFS)
        .map(|index| EvidenceId::for_test(&format!("evidence-{index}")))
        .collect();
    let result = SemanticPolicy::default().validate(ProposedAssertions {
        assertions: vec![ProposedAssertion {
            field: "summary".into(),
            value: "bounded".into(),
            authority: Authority::ModelInference,
            evidence_refs,
            confidence_basis_points: 10_000,
        }],
    });
    assert!(matches!(
        result,
        Err(agbox_workgraph::SemanticError::InvalidProposal)
    ));
}
