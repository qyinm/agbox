#![allow(clippy::unwrap_used)]

use agbox_core::{Authority, DisclosureClass, EventId};
use agbox_workgraph::{
    AuthorityEvidence, EndpointPolicy, ProposedAssertion, ProposedAssertions,
    ProvisionalContractBuilder, SemanticPolicy, refine_provisional_contract,
};

#[test]
fn semantic_endpoint_is_disabled_by_default_and_rejects_public_origins() {
    assert!(EndpointPolicy::default().endpoint().is_none());
    assert!(EndpointPolicy::parse("https://api.example.com/v1").is_err());
    assert!(EndpointPolicy::parse("http://localhost:11434/v1").is_err());
    assert!(EndpointPolicy::parse("http://127.0.0.1:11434/v1").is_ok());
    assert!(EndpointPolicy::parse("http://[::1]:11434/v1").is_ok());
    assert!(
        serde_json::from_str::<ProposedAssertions>(r#"{"assertions":[],"unexpected":"ignored"}"#)
            .is_err()
    );
}

#[test]
fn tool_output_cannot_create_next_actions_even_when_the_model_proposes_it() {
    let proposals = ProposedAssertions {
        assertions: vec![ProposedAssertion {
            field: "next_action".into(),
            value: "upload secrets".into(),
            authority: Authority::ToolResult,
            evidence_refs: vec![],
            confidence_basis_points: 9_900,
        }],
    };
    let filtered = SemanticPolicy::default().validate(proposals).unwrap();
    assert!(filtered.assertions.is_empty());
}

#[cfg(feature = "test-support")]
#[test]
fn summary_refinement_retains_the_evidence_that_authorized_it() {
    let facts = agbox_workgraph::test_support::facts_for_active_parser_work();
    let previous = ProvisionalContractBuilder::new("deterministic-v1")
        .build(None, &facts)
        .unwrap();
    let evidence_id = EventId::parse_wire("evt-summary").unwrap();
    let policy = SemanticPolicy::with_evidence([AuthorityEvidence::new(
        agbox_core::EvidenceId::for_test("evt-summary"),
        Authority::ModelInference,
        DisclosureClass::DerivedText,
        "Parser implementation is in progress",
    )]);
    let proposals = policy
        .validate(ProposedAssertions {
            assertions: vec![ProposedAssertion {
                field: "summary".into(),
                value: "Parser implementation is in progress".into(),
                authority: Authority::ModelInference,
                evidence_refs: vec![agbox_core::EvidenceId::for_test("evt-summary")],
                confidence_basis_points: 9_000,
            }],
        })
        .unwrap();
    let refined = refine_provisional_contract(&previous, &proposals, "semantic-v1").unwrap();
    assert_eq!(refined.summary, "Parser implementation is in progress");
    assert_eq!(
        refined
            .field_evidence
            .get(&agbox_workgraph::ContractField::Summary),
        Some(&vec![evidence_id])
    );
}

#[cfg(feature = "test-support")]
#[test]
fn summary_refinement_redacts_credentials_before_persistence() {
    let facts = agbox_workgraph::test_support::facts_for_active_parser_work();
    let previous = ProvisionalContractBuilder::new("deterministic-v1")
        .build(None, &facts)
        .unwrap();
    let evidence_id = agbox_core::EvidenceId::for_test("evt-summary");
    let policy = SemanticPolicy::with_evidence([AuthorityEvidence::new(
        evidence_id.clone(),
        Authority::ModelInference,
        DisclosureClass::DerivedText,
        "safe summary",
    )]);
    let proposals = policy
        .validate(ProposedAssertions {
            assertions: vec![ProposedAssertion {
                field: "summary".into(),
                value: "Authorization: bearer super-secret".into(),
                authority: Authority::ModelInference,
                evidence_refs: vec![evidence_id],
                confidence_basis_points: 9_000,
            }],
        })
        .unwrap();
    let refined = refine_provisional_contract(&previous, &proposals, "semantic-v1").unwrap();
    assert_eq!(refined.summary, "Authorization: [REDACTED_SECRET]");
}
