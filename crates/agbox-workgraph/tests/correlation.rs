#![allow(clippy::unwrap_used)]

use agbox_core::{EventId, ProjectId, Provider, WorkId};
use agbox_workgraph::{
    CorrelationDecision, CorrelationInput, CorrelationSignals, Correlator, MAX_ARTIFACT_HASHES,
    MAX_CANDIDATES, MAX_COMMAND_HASHES, WorkCandidate, score,
};

fn evidence(value: &str) -> EventId {
    EventId::parse_wire(value).unwrap()
}

#[test]
fn same_project_and_overlapping_artifacts_continue_across_agents() {
    let input = CorrelationInput::fixture()
        .provider("codex")
        .project("project-a")
        .branch("main")
        .artifacts(["src/lib.rs", "Cargo.toml"])
        .candidate(
            WorkCandidate::fixture("work-1")
                .provider("claude")
                .project("project-a")
                .branch("main")
                .artifacts(["src/lib.rs", "Cargo.toml"]),
        );

    assert!(matches!(
        Correlator.decide(&input),
        CorrelationDecision::Continue { work_id, .. } if work_id.as_str() == "work-1"
    ));
}

#[test]
fn semantic_similarity_alone_never_merges_work() {
    let input = CorrelationInput::fixture()
        .project("project-a")
        .semantic_similarity_basis_points(9_800)
        .candidate(WorkCandidate::fixture("work-1").project("project-a"));

    assert_eq!(Correlator.decide(&input), CorrelationDecision::Create);
}

#[test]
fn explicit_same_project_work_id_wins_even_when_another_candidate_scores_highly() {
    let input = CorrelationInput::new(
        ProjectId::for_test("project-a"),
        Provider::Codex,
        vec![evidence("evt-explicit")],
    )
    .explicit_work_id(WorkId::for_test("work-explicit"))
    .repository("repo")
    .branch_hash("main")
    .artifact_hashes(["src/lib.rs"])
    .candidate(
        WorkCandidate::new(
            WorkId::for_test("work-overlap"),
            ProjectId::for_test("project-a"),
        )
        .repository("repo")
        .branch_hash("main")
        .artifact_hashes(["src/lib.rs"]),
    )
    .candidate(WorkCandidate::new(
        WorkId::for_test("work-explicit"),
        ProjectId::for_test("project-a"),
    ));

    assert!(matches!(
        Correlator.decide(&input),
        CorrelationDecision::Continue { work_id, association }
            if work_id.as_str() == "work-explicit"
                && association.score.total == 10_000
                && association.evidence_refs == vec![evidence("evt-explicit")]
    ));
}

#[test]
fn explicit_id_cannot_cross_a_project_boundary() {
    let input = CorrelationInput::new(
        ProjectId::for_test("project-a"),
        Provider::Codex,
        vec![evidence("evt-project")],
    )
    .explicit_work_id(WorkId::for_test("work-other"))
    .candidate(WorkCandidate::new(
        WorkId::for_test("work-other"),
        ProjectId::for_test("project-b"),
    ));

    assert_eq!(Correlator.decide(&input), CorrelationDecision::Create);
}

#[test]
fn missing_explicit_id_does_not_fall_back_to_a_different_candidate() {
    let input = CorrelationInput::new(
        ProjectId::for_test("project-a"),
        Provider::Codex,
        vec![evidence("evt-missing-explicit")],
    )
    .explicit_work_id(WorkId::for_test("work-missing"))
    .repository("repo")
    .branch_hash("main")
    .artifact_hashes(["shared"])
    .candidate(
        WorkCandidate::new(
            WorkId::for_test("work-overlap"),
            ProjectId::for_test("project-a"),
        )
        .repository("repo")
        .branch_hash("main")
        .artifact_hashes(["shared"]),
    );

    assert_eq!(Correlator.decide(&input), CorrelationDecision::Create);
}

#[test]
fn explicit_continuation_selects_the_named_same_project_work() {
    let input = CorrelationInput::new(
        ProjectId::for_test("project-a"),
        Provider::Codex,
        vec![evidence("evt-continuation")],
    )
    .continuation_work_id(WorkId::for_test("work-continued"))
    .candidate(WorkCandidate::new(
        WorkId::for_test("work-continued"),
        ProjectId::for_test("project-a"),
    ));

    assert!(matches!(
        Correlator.decide(&input),
        CorrelationDecision::Continue { work_id, association }
            if work_id.as_str() == "work-continued"
                && association.score.total == 10_000
    ));
}

#[test]
fn association_requires_correlation_evidence() {
    let input = CorrelationInput::new(
        ProjectId::for_test("project-a"),
        Provider::Codex,
        Vec::new(),
    )
    .explicit_work_id(WorkId::for_test("work-explicit"))
    .candidate(WorkCandidate::new(
        WorkId::for_test("work-explicit"),
        ProjectId::for_test("project-a"),
    ));

    assert_eq!(Correlator.decide(&input), CorrelationDecision::Create);
}

#[test]
fn tied_non_explicit_candidates_split_deterministically_into_proposals() {
    let input = CorrelationInput::new(
        ProjectId::for_test("project-a"),
        Provider::Codex,
        vec![evidence("evt-tie")],
    )
    .repository("repo")
    .branch_hash("main")
    .artifact_hashes(["shared"])
    .candidate(
        WorkCandidate::new(WorkId::for_test("work-b"), ProjectId::for_test("project-a"))
            .repository("repo")
            .branch_hash("main")
            .artifact_hashes(["shared"]),
    )
    .candidate(
        WorkCandidate::new(WorkId::for_test("work-a"), ProjectId::for_test("project-a"))
            .repository("repo")
            .branch_hash("main")
            .artifact_hashes(["shared"]),
    );

    let outcome = Correlator.correlate(&input);
    assert_eq!(outcome.decision, CorrelationDecision::Create);
    assert_eq!(
        outcome
            .proposals
            .iter()
            .map(|proposal| proposal.work_id.as_str())
            .collect::<Vec<_>>(),
        ["work-a", "work-b"]
    );
    assert!(
        outcome
            .proposals
            .iter()
            .all(|proposal| proposal.proposal_only)
    );
}

#[test]
fn bounded_inputs_make_every_truncation_observable() {
    let mut input = CorrelationInput::new(
        ProjectId::for_test("project-a"),
        Provider::Codex,
        vec![evidence("evt-bounds")],
    )
    .artifact_hashes((0..=MAX_ARTIFACT_HASHES).map(|index| format!("artifact-{index}")))
    .command_hashes((0..=MAX_COMMAND_HASHES).map(|index| format!("command-{index}")));
    for index in 0..=MAX_CANDIDATES {
        input = input.candidate(WorkCandidate::new(
            WorkId::for_test(&format!("work-{index:03}")),
            ProjectId::for_test("project-a"),
        ));
    }

    let outcome = Correlator.correlate(&input);
    assert_eq!(input.bounded_artifact_hashes().len(), MAX_ARTIFACT_HASHES);
    assert_eq!(input.bounded_command_hashes().len(), MAX_COMMAND_HASHES);
    assert_eq!(input.candidates().len(), MAX_CANDIDATES);
    assert!(outcome.truncation.artifact_hashes);
    assert!(outcome.truncation.command_hashes);
    assert!(outcome.truncation.candidates);
    assert_eq!(outcome.candidates_considered, MAX_CANDIDATES);
    assert_eq!(outcome.decision, CorrelationDecision::Create);
}

#[test]
fn fixed_scoring_matches_the_contract() {
    let scored = score(&CorrelationSignals {
        explicit_work_id: false,
        explicit_continuation: false,
        same_repository: true,
        same_branch: true,
        artifact_overlap_basis_points: 10_000,
        command_overlap_basis_points: 5_000,
        minutes_since_activity: 10,
        semantic_similarity_basis_points: 8_000,
    });
    assert_eq!(scored.non_semantic, 6_500);
    assert_eq!(scored.total, 7_300);
}
