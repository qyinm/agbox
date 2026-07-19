#![allow(clippy::unwrap_used)]

use agbox_core::{Authority, DisclosureClass, EventId, EvidenceId, ProjectId};
use agbox_workgraph::{
    AuthorityEvidence, EndpointPolicy, ProposedAssertion, ProposedAssertions,
    ProvisionalContractBuilder, SemanticError, SemanticPolicy,
    refine_provisional_contract_at_with_policy,
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
    let event_id = EventId::parse_wire("evt-summary").unwrap();
    let evidence_id = EvidenceId::for_test("evidence-summary-distinct");
    let policy = SemanticPolicy::for_project(
        ProjectId::for_test("project-a"),
        [AuthorityEvidence::from_store(
            ProjectId::for_test("project-a"),
            evidence_id.clone(),
            Some(event_id.clone()),
            Authority::ModelInference,
            DisclosureClass::DerivedText,
            "Parser implementation is ready for verification",
        )],
    );
    let proposals = policy
        .validate(ProposedAssertions {
            assertions: vec![ProposedAssertion {
                field: "summary".into(),
                value: "Parser implementation is ready for verification".into(),
                authority: Authority::ModelInference,
                evidence_refs: vec![evidence_id],
                confidence_basis_points: 9_000,
            }],
        })
        .unwrap();
    let refined = refine_provisional_contract_at_with_policy(
        &previous,
        &proposals,
        "semantic-v1",
        previous.created_at,
        &policy,
    )
    .unwrap();
    assert_eq!(
        refined.summary,
        "Parser implementation is ready for verification"
    );
    assert_eq!(
        refined
            .field_evidence
            .get(&agbox_workgraph::ContractField::Summary),
        Some(&vec![event_id])
    );
}

#[cfg(feature = "test-support")]
#[test]
fn summary_refinement_redacts_credentials_before_persistence() {
    let facts = agbox_workgraph::test_support::facts_for_active_parser_work();
    let previous = ProvisionalContractBuilder::new("deterministic-v1")
        .build(None, &facts)
        .unwrap();
    let evidence_id = EvidenceId::for_test("evt-summary");
    let policy = SemanticPolicy::for_project(
        ProjectId::for_test("project-a"),
        [AuthorityEvidence::from_store(
            ProjectId::for_test("project-a"),
            evidence_id.clone(),
            Some(EventId::parse_wire("evt-summary").unwrap()),
            Authority::ModelInference,
            DisclosureClass::DerivedText,
            "safe summary",
        )],
    );
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
    let refined = refine_provisional_contract_at_with_policy(
        &previous,
        &proposals,
        "semantic-v1",
        previous.created_at,
        &policy,
    )
    .unwrap();
    assert_eq!(refined.summary, "Authorization: [REDACTED_SECRET]");
}

#[cfg(feature = "test-support")]
#[test]
fn empty_or_unsupported_proposals_cannot_create_a_refined_revision() {
    let facts = agbox_workgraph::test_support::facts_for_active_parser_work();
    let previous = ProvisionalContractBuilder::new("deterministic-v1")
        .build(None, &facts)
        .unwrap();
    let empty = ProposedAssertions {
        assertions: Vec::new(),
    };
    assert!(matches!(
        refine_provisional_contract_at_with_policy(
            &previous,
            &empty,
            "semantic-v1",
            previous.created_at,
            &SemanticPolicy::default(),
        ),
        Err(SemanticError::NoAssertions)
    ));
}

#[cfg(feature = "test-support")]
#[test]
fn empty_values_are_filtered_and_cannot_create_a_refinement() {
    let facts = agbox_workgraph::test_support::facts_for_active_parser_work();
    let previous = ProvisionalContractBuilder::new("deterministic-v1")
        .build(None, &facts)
        .unwrap();
    let empty_values = ProposedAssertions {
        assertions: ["summary", "blocker", "verification", "objective"]
            .into_iter()
            .map(|field| ProposedAssertion {
                field: field.into(),
                value: " \t ".into(),
                authority: Authority::HumanIntent,
                evidence_refs: Vec::new(),
                confidence_basis_points: 9_000,
            })
            .collect(),
    };
    let policy = SemanticPolicy::default();
    let filtered = policy.validate(empty_values.clone()).unwrap();
    assert!(filtered.assertions.is_empty());
    assert!(matches!(
        refine_provisional_contract_at_with_policy(
            &previous,
            &filtered,
            "semantic-v1",
            previous.created_at,
            &policy,
        ),
        Err(SemanticError::NoAssertions)
    ));
    assert!(matches!(
        refine_provisional_contract_at_with_policy(
            &previous,
            &empty_values,
            "semantic-v1",
            previous.created_at,
            &policy,
        ),
        Err(SemanticError::InvalidProposal)
    ));
}

#[cfg(feature = "test-support")]
#[test]
fn accepted_non_summary_field_is_applied_with_store_provenance() {
    let facts = agbox_workgraph::test_support::facts_for_active_parser_work();
    let previous = ProvisionalContractBuilder::new("deterministic-v1")
        .build(None, &facts)
        .unwrap();
    let evidence_id = EvidenceId::for_test("evidence-objective-distinct");
    let event_id = EventId::parse_wire("evt-objective").unwrap();
    let policy = SemanticPolicy::for_project(
        ProjectId::for_test("project-a"),
        [AuthorityEvidence::from_store(
            ProjectId::for_test("project-a"),
            evidence_id.clone(),
            Some(event_id.clone()),
            Authority::HumanIntent,
            DisclosureClass::HumanIntent,
            "Ship the parser",
        )],
    );
    let proposals = policy
        .validate(ProposedAssertions {
            assertions: vec![ProposedAssertion {
                field: "objective".into(),
                value: "Ship the parser".into(),
                authority: Authority::ModelInference,
                evidence_refs: vec![evidence_id],
                confidence_basis_points: 9_000,
            }],
        })
        .unwrap();
    let refined = refine_provisional_contract_at_with_policy(
        &previous,
        &proposals,
        "semantic-v1",
        previous.created_at,
        &policy,
    )
    .unwrap();
    assert_eq!(refined.objective.as_deref(), Some("Ship the parser"));
    assert_eq!(
        refined
            .field_evidence
            .get(&agbox_workgraph::ContractField::Objective),
        Some(&vec![event_id])
    );
}
