use std::collections::BTreeSet;

use agbox_core::{EventId, ProjectId, Provider, WorkId};
use serde::{Deserialize, Serialize};

pub const CONTINUE_THRESHOLD: u16 = 6_000;
pub const MIN_NON_SEMANTIC_SCORE: u16 = 2_500;
pub const MAX_ARTIFACT_HASHES: usize = 64;
pub const MAX_COMMAND_HASHES: usize = 32;
pub const MAX_CANDIDATES: usize = 64;
const MAX_ASSOCIATION_EVIDENCE: usize = agbox_core::limits::MAX_CONTRACT_EVIDENCE_REFS;

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[allow(clippy::struct_excessive_bools)]
pub struct CorrelationSignals {
    pub explicit_work_id: bool,
    pub explicit_continuation: bool,
    pub same_repository: bool,
    pub same_branch: bool,
    pub artifact_overlap_basis_points: u16,
    pub command_overlap_basis_points: u16,
    pub minutes_since_activity: u32,
    pub semantic_similarity_basis_points: u16,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct CorrelationScore {
    pub total: u16,
    pub non_semantic: u16,
}

#[must_use]
pub fn score(signals: &CorrelationSignals) -> CorrelationScore {
    let explicit: u16 = if signals.explicit_work_id { 10_000 } else { 0 };
    let continuation: u16 = if signals.explicit_continuation {
        9_500
    } else {
        0
    };
    let repository_branch: u16 = if signals.same_repository && signals.same_branch {
        2_500
    } else if signals.same_repository {
        1_500
    } else {
        0
    };
    let artifacts = u16::try_from(
        u32::from(signals.artifact_overlap_basis_points.min(10_000)) * 2_500 / 10_000,
    )
    .unwrap_or(2_500);
    let commands =
        u16::try_from(u32::from(signals.command_overlap_basis_points.min(10_000)) * 1_000 / 10_000)
            .unwrap_or(1_000);
    let temporal: u16 = if signals.minutes_since_activity <= 30 {
        1_000
    } else {
        0
    };
    let semantic = u16::try_from(
        u32::from(signals.semantic_similarity_basis_points.min(10_000)) * 1_000 / 10_000,
    )
    .unwrap_or(1_000);
    let non_semantic = explicit
        .max(continuation)
        .saturating_add(repository_branch)
        .saturating_add(artifacts)
        .saturating_add(commands)
        .saturating_add(temporal);
    CorrelationScore {
        total: non_semantic.saturating_add(semantic).min(10_000),
        non_semantic,
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct WorkCandidate {
    pub work_id: WorkId,
    pub project_id: ProjectId,
    pub provider: Option<Provider>,
    pub repository_hash: Option<String>,
    pub branch_hash: Option<String>,
    pub artifact_hashes: Vec<String>,
    pub command_hashes: Vec<String>,
    pub minutes_since_activity: u32,
    pub semantic_similarity_basis_points: Option<u16>,
    #[serde(default)]
    artifact_hashes_truncated: bool,
    #[serde(default)]
    command_hashes_truncated: bool,
}

impl WorkCandidate {
    #[must_use]
    pub fn new(work_id: WorkId, project_id: ProjectId) -> Self {
        Self {
            work_id,
            project_id,
            provider: None,
            repository_hash: None,
            branch_hash: None,
            artifact_hashes: Vec::new(),
            command_hashes: Vec::new(),
            minutes_since_activity: 0,
            semantic_similarity_basis_points: None,
            artifact_hashes_truncated: false,
            command_hashes_truncated: false,
        }
    }

    #[must_use]
    pub fn repository(mut self, repository_hash: impl Into<String>) -> Self {
        self.repository_hash = Some(repository_hash.into());
        self
    }

    #[must_use]
    pub fn branch_hash(mut self, branch_hash: impl Into<String>) -> Self {
        self.branch_hash = Some(branch_hash.into());
        self
    }

    #[must_use]
    pub fn artifact_hashes(mut self, hashes: impl IntoIterator<Item = impl Into<String>>) -> Self {
        let (values, truncated) = bounded_unique(hashes, MAX_ARTIFACT_HASHES);
        self.artifact_hashes = values;
        self.artifact_hashes_truncated = truncated;
        self
    }

    #[must_use]
    pub fn command_hashes(mut self, hashes: impl IntoIterator<Item = impl Into<String>>) -> Self {
        let (values, truncated) = bounded_unique(hashes, MAX_COMMAND_HASHES);
        self.command_hashes = values;
        self.command_hashes_truncated = truncated;
        self
    }

    #[must_use]
    pub fn recent_minutes(mut self, minutes: u32) -> Self {
        self.minutes_since_activity = minutes;
        self
    }

    #[must_use]
    pub fn semantic_similarity_basis_points(mut self, basis_points: u16) -> Self {
        self.semantic_similarity_basis_points = Some(basis_points.min(10_000));
        self
    }

    #[cfg(feature = "test-support")]
    #[must_use]
    pub fn fixture(work_id: &str) -> Self {
        Self::new(
            WorkId::for_test(work_id),
            ProjectId::for_test("project-fixture"),
        )
    }

    #[cfg(feature = "test-support")]
    #[must_use]
    pub fn provider(mut self, provider: &str) -> Self {
        self.provider = parse_provider(provider);
        self
    }

    #[cfg(feature = "test-support")]
    #[must_use]
    pub fn project(mut self, project_id: &str) -> Self {
        self.project_id = ProjectId::for_test(project_id);
        self
    }

    #[cfg(feature = "test-support")]
    #[must_use]
    pub fn branch(self, branch_hash: &str) -> Self {
        self.branch_hash(branch_hash)
    }

    #[cfg(feature = "test-support")]
    #[must_use]
    pub fn artifacts(self, hashes: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.artifact_hashes(hashes)
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct CorrelationTruncation {
    pub artifact_hashes: bool,
    pub command_hashes: bool,
    pub candidates: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct CorrelationInput {
    project_id: ProjectId,
    provider: Provider,
    evidence_refs: Vec<EventId>,
    explicit_work_id: Option<WorkId>,
    continuation_work_id: Option<WorkId>,
    repository_hash: Option<String>,
    branch_hash: Option<String>,
    artifact_hashes: Vec<String>,
    command_hashes: Vec<String>,
    semantic_similarity_basis_points: u16,
    candidates: Vec<WorkCandidate>,
    truncation: CorrelationTruncation,
}

impl CorrelationInput {
    #[must_use]
    pub fn new(project_id: ProjectId, provider: Provider, evidence_refs: Vec<EventId>) -> Self {
        let mut evidence_refs = evidence_refs;
        evidence_refs.truncate(MAX_ASSOCIATION_EVIDENCE);
        evidence_refs.sort();
        evidence_refs.dedup();
        Self {
            project_id,
            provider,
            evidence_refs,
            explicit_work_id: None,
            continuation_work_id: None,
            repository_hash: None,
            branch_hash: None,
            artifact_hashes: Vec::new(),
            command_hashes: Vec::new(),
            semantic_similarity_basis_points: 0,
            candidates: Vec::new(),
            truncation: CorrelationTruncation::default(),
        }
    }

    #[must_use]
    pub fn explicit_work_id(mut self, work_id: WorkId) -> Self {
        self.explicit_work_id = Some(work_id);
        self
    }

    #[must_use]
    pub fn continuation_work_id(mut self, work_id: WorkId) -> Self {
        self.continuation_work_id = Some(work_id);
        self
    }

    #[must_use]
    pub fn repository(mut self, repository_hash: impl Into<String>) -> Self {
        self.repository_hash = Some(repository_hash.into());
        self
    }

    #[must_use]
    pub fn branch_hash(mut self, branch_hash: impl Into<String>) -> Self {
        self.branch_hash = Some(branch_hash.into());
        self
    }

    #[must_use]
    pub fn artifact_hashes(mut self, hashes: impl IntoIterator<Item = impl Into<String>>) -> Self {
        let (values, truncated) = bounded_unique(hashes, MAX_ARTIFACT_HASHES);
        self.artifact_hashes = values;
        self.truncation.artifact_hashes = truncated;
        self
    }

    #[must_use]
    pub fn command_hashes(mut self, hashes: impl IntoIterator<Item = impl Into<String>>) -> Self {
        let (values, truncated) = bounded_unique(hashes, MAX_COMMAND_HASHES);
        self.command_hashes = values;
        self.truncation.command_hashes = truncated;
        self
    }

    #[must_use]
    pub fn semantic_similarity_basis_points(mut self, basis_points: u16) -> Self {
        self.semantic_similarity_basis_points = basis_points.min(10_000);
        self
    }

    /// Adds a candidate in caller-supplied query priority order.
    ///
    /// Callers should supply exact IDs first, then artifact matches, command
    /// matches, and finally recent active or blocked work. Additional
    /// candidates are discarded with observable truncation.
    #[must_use]
    pub fn candidate(mut self, candidate: WorkCandidate) -> Self {
        if self
            .candidates
            .iter()
            .any(|existing| existing.work_id == candidate.work_id)
        {
            return self;
        }
        if self.candidates.len() == MAX_CANDIDATES {
            self.truncation.candidates = true;
        } else {
            self.candidates.push(candidate);
        }
        self
    }

    #[must_use]
    pub fn project_id(&self) -> &ProjectId {
        &self.project_id
    }

    #[must_use]
    pub fn source_provider(&self) -> Provider {
        self.provider
    }

    #[must_use]
    pub fn evidence_refs(&self) -> &[EventId] {
        &self.evidence_refs
    }

    #[must_use]
    pub fn bounded_artifact_hashes(&self) -> &[String] {
        &self.artifact_hashes
    }

    #[must_use]
    pub fn bounded_command_hashes(&self) -> &[String] {
        &self.command_hashes
    }

    #[must_use]
    pub fn candidates(&self) -> &[WorkCandidate] {
        &self.candidates
    }

    #[must_use]
    pub fn truncation(&self) -> CorrelationTruncation {
        self.truncation
    }

    #[cfg(feature = "test-support")]
    #[must_use]
    pub fn fixture() -> Self {
        Self::new(
            ProjectId::for_test("project-fixture"),
            Provider::Codex,
            vec![
                EventId::parse_wire("evt-correlation-fixture")
                    .unwrap_or_else(|| unreachable!("the static fixture ID is valid")),
            ],
        )
    }

    #[cfg(feature = "test-support")]
    #[must_use]
    pub fn provider(mut self, provider: &str) -> Self {
        self.provider = parse_provider(provider).unwrap_or(self.provider);
        self
    }

    #[cfg(feature = "test-support")]
    #[must_use]
    pub fn project(mut self, project_id: &str) -> Self {
        self.project_id = ProjectId::for_test(project_id);
        self
    }

    #[cfg(feature = "test-support")]
    #[must_use]
    pub fn branch(self, branch_hash: &str) -> Self {
        self.branch_hash(branch_hash)
    }

    #[cfg(feature = "test-support")]
    #[must_use]
    pub fn artifacts(self, hashes: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.artifact_hashes(hashes)
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct WorkAssociation {
    pub work_id: WorkId,
    pub score: CorrelationScore,
    pub evidence_refs: Vec<EventId>,
    /// True for a low-confidence graph proposal. It is never an assignment,
    /// scheduling decision, acceptance, or instruction to execute.
    pub proposal_only: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "decision", rename_all = "snake_case")]
pub enum CorrelationDecision {
    Create,
    Continue {
        work_id: WorkId,
        association: WorkAssociation,
    },
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct CorrelationOutcome {
    pub decision: CorrelationDecision,
    pub proposals: Vec<WorkAssociation>,
    pub candidates_considered: usize,
    pub truncation: CorrelationTruncation,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct Correlator;

impl Correlator {
    #[must_use]
    pub fn decide(&self, input: &CorrelationInput) -> CorrelationDecision {
        self.correlate(input).decision
    }

    #[must_use]
    #[allow(clippy::too_many_lines)]
    pub fn correlate(&self, input: &CorrelationInput) -> CorrelationOutcome {
        let mut truncation = input.truncation;
        let mut seen = BTreeSet::new();
        let mut selected = Vec::new();
        for candidate in &input.candidates {
            if !seen.insert(&candidate.work_id) {
                continue;
            }
            if selected.len() == MAX_CANDIDATES {
                truncation.candidates = true;
                break;
            }
            selected.push(candidate);
        }
        let candidates_considered = selected.len();
        if input.evidence_refs.is_empty() {
            return CorrelationOutcome {
                decision: CorrelationDecision::Create,
                proposals: Vec::new(),
                candidates_considered,
                truncation,
            };
        }
        let candidates = selected
            .into_iter()
            .filter(|candidate| candidate.project_id == input.project_id);
        let input_artifacts = bounded_set(&input.artifact_hashes, MAX_ARTIFACT_HASHES);
        let input_commands = bounded_set(&input.command_hashes, MAX_COMMAND_HASHES);

        let mut scored = candidates
            .map(|candidate| {
                truncation.artifact_hashes |= candidate.artifact_hashes_truncated
                    || candidate.artifact_hashes.len() > MAX_ARTIFACT_HASHES;
                truncation.command_hashes |= candidate.command_hashes_truncated
                    || candidate.command_hashes.len() > MAX_COMMAND_HASHES;
                let signals = signals(input, candidate, &input_artifacts, &input_commands);
                (candidate, score(&signals), signals.explicit_work_id)
            })
            .collect::<Vec<_>>();

        scored.sort_by(|left, right| {
            right
                .1
                .total
                .cmp(&left.1.total)
                .then_with(|| right.1.non_semantic.cmp(&left.1.non_semantic))
                .then_with(|| left.0.work_id.cmp(&right.0.work_id))
        });

        if let Some((candidate, candidate_score, _)) =
            scored.iter().find(|(_, _, explicit)| *explicit)
        {
            let association = association(input, candidate, *candidate_score, false);
            return CorrelationOutcome {
                decision: CorrelationDecision::Continue {
                    work_id: candidate.work_id.clone(),
                    association,
                },
                proposals: Vec::new(),
                candidates_considered,
                truncation,
            };
        }
        if input.explicit_work_id.is_some() {
            return CorrelationOutcome {
                decision: CorrelationDecision::Create,
                proposals: Vec::new(),
                candidates_considered,
                truncation,
            };
        }
        if let Some(continuation_work_id) = &input.continuation_work_id {
            let Some((candidate, candidate_score, _)) = scored
                .iter()
                .find(|(candidate, _, _)| candidate.work_id == *continuation_work_id)
            else {
                return CorrelationOutcome {
                    decision: CorrelationDecision::Create,
                    proposals: Vec::new(),
                    candidates_considered,
                    truncation,
                };
            };
            let association = association(input, candidate, *candidate_score, false);
            return CorrelationOutcome {
                decision: CorrelationDecision::Continue {
                    work_id: candidate.work_id.clone(),
                    association,
                },
                proposals: Vec::new(),
                candidates_considered,
                truncation,
            };
        }

        let qualifying = scored
            .iter()
            .filter(|(_, candidate_score, _)| {
                candidate_score.total >= CONTINUE_THRESHOLD
                    && candidate_score.non_semantic >= MIN_NON_SEMANTIC_SCORE
            })
            .collect::<Vec<_>>();
        let Some((winner, winner_score, _)) = qualifying.first().copied() else {
            return CorrelationOutcome {
                decision: CorrelationDecision::Create,
                proposals: Vec::new(),
                candidates_considered,
                truncation,
            };
        };
        let tied = qualifying
            .iter()
            .take_while(|(_, candidate_score, _)| *candidate_score == *winner_score)
            .copied()
            .collect::<Vec<_>>();
        if tied.len() > 1 {
            return CorrelationOutcome {
                decision: CorrelationDecision::Create,
                proposals: tied
                    .into_iter()
                    .map(|(candidate, candidate_score, _)| {
                        association(input, candidate, *candidate_score, true)
                    })
                    .collect(),
                candidates_considered,
                truncation,
            };
        }

        let association = association(input, winner, *winner_score, false);
        CorrelationOutcome {
            decision: CorrelationDecision::Continue {
                work_id: winner.work_id.clone(),
                association,
            },
            proposals: Vec::new(),
            candidates_considered,
            truncation,
        }
    }
}

fn signals(
    input: &CorrelationInput,
    candidate: &WorkCandidate,
    input_artifacts: &BTreeSet<&String>,
    input_commands: &BTreeSet<&String>,
) -> CorrelationSignals {
    let explicit_work_id = input
        .explicit_work_id
        .as_ref()
        .is_some_and(|work_id| work_id == &candidate.work_id);
    let explicit_continuation = input
        .continuation_work_id
        .as_ref()
        .is_some_and(|work_id| work_id == &candidate.work_id);
    let same_repository = match (&input.repository_hash, &candidate.repository_hash) {
        (Some(left), Some(right)) => left == right,
        (None, None) => true,
        _ => false,
    };
    CorrelationSignals {
        explicit_work_id,
        explicit_continuation,
        same_repository,
        same_branch: same_repository
            && input
                .branch_hash
                .as_ref()
                .zip(candidate.branch_hash.as_ref())
                .is_some_and(|(left, right)| left == right),
        artifact_overlap_basis_points: overlap_basis_points(
            input_artifacts,
            &candidate.artifact_hashes,
            MAX_ARTIFACT_HASHES,
        ),
        command_overlap_basis_points: overlap_basis_points(
            input_commands,
            &candidate.command_hashes,
            MAX_COMMAND_HASHES,
        ),
        minutes_since_activity: candidate.minutes_since_activity,
        semantic_similarity_basis_points: candidate
            .semantic_similarity_basis_points
            .unwrap_or(input.semantic_similarity_basis_points),
    }
}

fn association(
    input: &CorrelationInput,
    candidate: &WorkCandidate,
    candidate_score: CorrelationScore,
    proposal_only: bool,
) -> WorkAssociation {
    WorkAssociation {
        work_id: candidate.work_id.clone(),
        score: candidate_score,
        evidence_refs: input
            .evidence_refs
            .iter()
            .take(MAX_ASSOCIATION_EVIDENCE)
            .cloned()
            .collect(),
        proposal_only,
    }
}

fn overlap_basis_points(left: &BTreeSet<&String>, right: &[String], bound: usize) -> u16 {
    let right = right.iter().take(bound).collect::<BTreeSet<_>>();
    let denominator = left.len().max(right.len());
    if denominator == 0 {
        return 0;
    }
    let overlap = left.intersection(&right).count();
    u16::try_from(overlap.saturating_mul(10_000) / denominator).unwrap_or(10_000)
}

fn bounded_set(values: &[String], bound: usize) -> BTreeSet<&String> {
    values.iter().take(bound).collect()
}

fn bounded_unique(
    values: impl IntoIterator<Item = impl Into<String>>,
    bound: usize,
) -> (Vec<String>, bool) {
    let mut unique = BTreeSet::new();
    let mut truncated = false;
    for value in values {
        unique.insert(value.into());
        if unique.len() > bound {
            truncated = true;
            unique.pop_last();
        }
    }
    (unique.into_iter().collect(), truncated)
}

#[cfg(feature = "test-support")]
fn parse_provider(value: &str) -> Option<Provider> {
    match value {
        "claude" => Some(Provider::Claude),
        "codex" => Some(Provider::Codex),
        _ => None,
    }
}
