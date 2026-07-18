use std::collections::{BTreeMap, BTreeSet};

use agbox_core::{
    ContractId, DisclosureClass, EventId, ProjectId, RedactionError, RedactionPolicy, WorkId,
    WorkStatus, limits::MAX_CONTRACT_EVIDENCE_REFS,
};
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use crate::ReducedFact;

const STRUCTURED_TOOL_RESULT: &str = "structured_tool_result";

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ContractField {
    Objective,
    Status,
    Summary,
    CompletedSteps,
    NextActions,
    Blockers,
    Constraints,
    CompletionCriteria,
    Artifacts,
    Verification,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ProvisionalContract {
    pub contract_id: ContractId,
    pub work_id: WorkId,
    pub revision: u64,
    pub project_id: ProjectId,
    pub objective: Option<String>,
    pub status: WorkStatus,
    pub summary: String,
    pub completed_steps: Vec<String>,
    pub next_actions: Vec<String>,
    pub blockers: Vec<String>,
    pub constraints: Vec<String>,
    pub completion_criteria: Vec<String>,
    pub artifacts: Vec<String>,
    pub verification: Vec<String>,
    pub evidence_refs: Vec<EventId>,
    pub field_evidence: BTreeMap<ContractField, Vec<EventId>>,
    pub evidence_truncated: bool,
    pub confidence_basis_points: u16,
    pub created_at: OffsetDateTime,
    pub extractor_version: String,
    pub material_content_hash: String,
}

impl ProvisionalContract {
    #[must_use]
    pub fn non_empty_fields(&self) -> Vec<ContractField> {
        let mut fields = vec![ContractField::Status];
        if self
            .objective
            .as_ref()
            .is_some_and(|value| !value.is_empty())
        {
            fields.push(ContractField::Objective);
        }
        if !self.summary.is_empty() {
            fields.push(ContractField::Summary);
        }
        for (field, non_empty) in [
            (
                ContractField::CompletedSteps,
                !self.completed_steps.is_empty(),
            ),
            (ContractField::NextActions, !self.next_actions.is_empty()),
            (ContractField::Blockers, !self.blockers.is_empty()),
            (ContractField::Constraints, !self.constraints.is_empty()),
            (
                ContractField::CompletionCriteria,
                !self.completion_criteria.is_empty(),
            ),
            (ContractField::Artifacts, !self.artifacts.is_empty()),
            (ContractField::Verification, !self.verification.is_empty()),
        ] {
            if non_empty {
                fields.push(field);
            }
        }
        fields
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ContractBuildError {
    #[error("provisional contracts require at least one observed fact")]
    MissingFacts,
    #[error("all facts in a provisional contract must belong to one project")]
    MixedProjects,
    #[error("extractor version exceeds the inline-content bound")]
    ExtractorVersionTooLarge,
    #[error("the contract revision counter overflowed")]
    RevisionOverflow,
    #[error("contract text could not be redacted")]
    Redaction(#[from] RedactionError),
    #[error("material contract serialization failed")]
    Serialization(#[from] serde_json::Error),
    #[error("every non-empty semantic field must cite retained evidence")]
    MissingFieldEvidence,
}

#[derive(Clone, Debug)]
pub struct ProvisionalContractBuilder {
    extractor_version: String,
    work_id: Option<WorkId>,
}

impl ProvisionalContractBuilder {
    #[must_use]
    pub fn new(extractor_version: impl Into<String>) -> Self {
        Self {
            extractor_version: extractor_version.into(),
            work_id: None,
        }
    }

    #[must_use]
    pub fn for_work(mut self, work_id: WorkId) -> Self {
        self.work_id = Some(work_id);
        self
    }

    /// Builds one deterministic provisional revision.
    ///
    /// Replaying facts that do not materially change the previous contract
    /// returns the previous revision unchanged, allowing the persistence layer
    /// to avoid inserting a duplicate immutable revision.
    ///
    /// # Errors
    ///
    /// Returns an error for empty or cross-project fact sets, an oversized
    /// extractor version, redaction failures, serialization failures, or
    /// revision overflow.
    #[allow(clippy::too_many_lines)]
    pub fn build(
        &self,
        previous: Option<&ProvisionalContract>,
        facts: &[ReducedFact],
    ) -> Result<ProvisionalContract, ContractBuildError> {
        if facts.is_empty() {
            return previous.cloned().ok_or(ContractBuildError::MissingFacts);
        }
        if self.extractor_version.len() > agbox_core::limits::MAX_INLINE_BYTES {
            return Err(ContractBuildError::ExtractorVersionTooLarge);
        }
        let project_id = facts
            .first()
            .map(ReducedFact::project_id)
            .ok_or(ContractBuildError::MissingFacts)?
            .clone();
        if facts.iter().any(|fact| fact.project_id() != &project_id)
            || previous.is_some_and(|contract| contract.project_id != project_id)
        {
            return Err(ContractBuildError::MixedProjects);
        }

        let previous_evidence: BTreeSet<EventId> = previous
            .map(|contract| {
                contract
                    .evidence_refs
                    .iter()
                    .take(MAX_CONTRACT_EVIDENCE_REFS)
                    .cloned()
                    .collect()
            })
            .unwrap_or_default();
        let new_facts = facts
            .iter()
            .filter(|fact| !previous_evidence.contains(fact.evidence_id()))
            .collect::<Vec<_>>();
        let policy = RedactionPolicy::new()?;
        let mut draft = MaterialDraft::from_previous(previous);
        apply_text_facts(&mut draft, facts, &policy)?;
        apply_artifact_facts(&mut draft, facts, &policy)?;
        apply_action_facts(&mut draft, facts, &policy)?;

        let all_fact_refs;
        let status_facts = if previous.is_some() {
            new_facts.as_slice()
        } else {
            all_fact_refs = facts.iter().collect::<Vec<_>>();
            all_fact_refs.as_slice()
        };
        if !status_facts.is_empty() {
            let (status, evidence) =
                derive_status(status_facts, previous.map(|value| value.status));
            draft.status = status;
            draft.field_evidence.insert(ContractField::Status, evidence);
        }
        draft
            .field_evidence
            .entry(ContractField::Status)
            .or_insert_with(|| {
                facts
                    .iter()
                    .map(|fact| fact.evidence_id().clone())
                    .take(MAX_CONTRACT_EVIDENCE_REFS)
                    .collect()
            });
        draft.normalize();

        let (provenance_evidence, provenance_truncated) = bounded_provenance(
            previous
                .into_iter()
                .flat_map(|contract| contract.evidence_refs.iter())
                .chain(facts.iter().map(ReducedFact::evidence_id)),
        );
        let evidence_truncated =
            provenance_truncated || previous.is_some_and(|contract| contract.evidence_truncated);
        draft.bound_field_evidence();
        let all_evidence = retain_field_evidence(&mut draft, &provenance_evidence);
        validate_field_evidence(&draft, &all_evidence)?;

        let material_content_hash = material_hash(&draft)?;
        if let Some(previous) = previous
            && previous.material_content_hash == material_content_hash
        {
            return Ok(previous.clone());
        }

        let work_id = previous
            .map(|contract| contract.work_id.clone())
            .or_else(|| self.work_id.clone())
            .unwrap_or_else(|| stable_work_id(&project_id, &all_evidence));
        let contract_id = previous.map_or_else(
            || stable_contract_id(&work_id),
            |contract| contract.contract_id.clone(),
        );
        let revision = previous.map_or(Ok(1), |contract| {
            contract
                .revision
                .checked_add(1)
                .ok_or(ContractBuildError::RevisionOverflow)
        })?;
        let created_at = facts
            .iter()
            .filter_map(ReducedFact::observed_at)
            .max()
            .or_else(|| previous.map(|contract| contract.created_at))
            .unwrap_or(OffsetDateTime::UNIX_EPOCH);

        Ok(ProvisionalContract {
            contract_id,
            work_id,
            revision,
            project_id,
            objective: draft.objective,
            status: draft.status,
            summary: draft.summary,
            completed_steps: draft.completed_steps,
            next_actions: draft.next_actions,
            blockers: draft.blockers,
            constraints: draft.constraints,
            completion_criteria: draft.completion_criteria,
            artifacts: draft.artifacts,
            verification: draft.verification,
            evidence_refs: all_evidence,
            field_evidence: draft.field_evidence,
            evidence_truncated,
            confidence_basis_points: 10_000,
            created_at,
            extractor_version: self.extractor_version.clone(),
            material_content_hash,
        })
    }
}

#[derive(Serialize)]
struct MaterialDraft {
    objective: Option<String>,
    status: WorkStatus,
    summary: String,
    completed_steps: Vec<String>,
    next_actions: Vec<String>,
    blockers: Vec<String>,
    constraints: Vec<String>,
    completion_criteria: Vec<String>,
    artifacts: Vec<String>,
    verification: Vec<String>,
    field_evidence: BTreeMap<ContractField, Vec<EventId>>,
}

impl MaterialDraft {
    fn from_previous(previous: Option<&ProvisionalContract>) -> Self {
        previous.map_or_else(
            || Self {
                objective: None,
                status: WorkStatus::Observed,
                summary: String::new(),
                completed_steps: Vec::new(),
                next_actions: Vec::new(),
                blockers: Vec::new(),
                constraints: Vec::new(),
                completion_criteria: Vec::new(),
                artifacts: Vec::new(),
                verification: Vec::new(),
                field_evidence: BTreeMap::new(),
            },
            |contract| Self {
                objective: contract.objective.clone(),
                status: contract.status,
                summary: contract.summary.clone(),
                completed_steps: contract.completed_steps.clone(),
                next_actions: contract.next_actions.clone(),
                blockers: contract.blockers.clone(),
                constraints: contract.constraints.clone(),
                completion_criteria: contract.completion_criteria.clone(),
                artifacts: contract.artifacts.clone(),
                verification: contract.verification.clone(),
                field_evidence: contract.field_evidence.clone(),
            },
        )
    }

    fn normalize(&mut self) {
        for values in [
            &mut self.completed_steps,
            &mut self.next_actions,
            &mut self.blockers,
            &mut self.constraints,
            &mut self.completion_criteria,
            &mut self.artifacts,
            &mut self.verification,
        ] {
            values.sort();
            values.dedup();
            values.truncate(agbox_core::limits::MAX_CONTRACT_ITEMS_PER_FIELD);
        }
        self.field_evidence.retain(|_, references| {
            references.sort();
            references.dedup();
            !references.is_empty()
        });
    }

    fn bound_field_evidence(&mut self) {
        for references in self.field_evidence.values_mut() {
            references.sort();
            references.dedup();
            references.truncate(MAX_CONTRACT_EVIDENCE_REFS);
        }
    }
}

fn apply_text_facts(
    draft: &mut MaterialDraft,
    facts: &[ReducedFact],
    policy: &RedactionPolicy,
) -> Result<(), ContractBuildError> {
    if let Some((_, evidence, text)) = facts
        .iter()
        .filter_map(|fact| match fact {
            ReducedFact::HumanObjective {
                redacted_text: Some(text),
                evidence,
                ..
            } => safe_text(text, DisclosureClass::HumanIntent, policy)
                .transpose()
                .map(|value| value.map(|value| (evidence.as_str(), evidence.clone(), value))),
            _ => None,
        })
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .max_by(|left, right| left.0.cmp(right.0))
    {
        draft.objective = Some(text);
        draft
            .field_evidence
            .insert(ContractField::Objective, vec![evidence]);
    }
    if let Some((_, evidence, text)) = facts
        .iter()
        .filter_map(|fact| match fact {
            ReducedFact::HumanConstraint {
                redacted_text: Some(text),
                evidence,
                ..
            } => safe_text(text, DisclosureClass::HumanIntent, policy)
                .transpose()
                .map(|value| value.map(|value| (evidence.as_str(), evidence.clone(), value))),
            _ => None,
        })
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .max_by(|left, right| left.0.cmp(right.0))
    {
        draft.constraints = vec![text];
        draft
            .field_evidence
            .insert(ContractField::Constraints, vec![evidence]);
    }
    if let Some((_, evidence, text)) = facts
        .iter()
        .filter_map(|fact| match fact {
            ReducedFact::AgentStatement {
                redacted_text: Some(text),
                evidence,
                ..
            } => safe_text(text, DisclosureClass::AgentStatement, policy)
                .transpose()
                .map(|value| value.map(|value| (evidence.as_str(), evidence.clone(), value))),
            _ => None,
        })
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .max_by(|left, right| left.0.cmp(right.0))
    {
        draft.summary = text;
        draft
            .field_evidence
            .insert(ContractField::Summary, vec![evidence]);
    }
    Ok(())
}

fn apply_artifact_facts(
    draft: &mut MaterialDraft,
    facts: &[ReducedFact],
    policy: &RedactionPolicy,
) -> Result<(), ContractBuildError> {
    for (text, evidence) in facts.iter().filter_map(|fact| match fact {
        ReducedFact::Artifact {
            project_relative_path: Some(path),
            evidence,
            ..
        } => Some((path, evidence)),
        _ => None,
    }) {
        if let Some(text) = safe_text(text, DisclosureClass::ObservedState, policy)? {
            draft.artifacts.push(text);
            draft
                .field_evidence
                .entry(ContractField::Artifacts)
                .or_default()
                .push(evidence.clone());
        }
    }
    Ok(())
}

fn apply_action_facts(
    draft: &mut MaterialDraft,
    facts: &[ReducedFact],
    policy: &RedactionPolicy,
) -> Result<(), ContractBuildError> {
    let outcomes = latest_finished_outcomes(facts.iter());

    for (_action_id, (fact, _)) in outcomes {
        let ReducedFact::Verification {
            command: Some(command),
            succeeded,
            evidence,
            ..
        } = fact
        else {
            continue;
        };
        let Some(text) = safe_text(command, DisclosureClass::ToolResult, policy)? else {
            continue;
        };
        draft.verification.push(text.clone());
        draft
            .field_evidence
            .entry(ContractField::Verification)
            .or_default()
            .push(evidence.clone());
        if *succeeded {
            draft.completed_steps.push(text);
            draft
                .field_evidence
                .entry(ContractField::CompletedSteps)
                .or_default()
                .push(evidence.clone());
            draft.blockers.clear();
            draft.field_evidence.remove(&ContractField::Blockers);
        } else {
            draft.blockers.push(text);
            draft
                .field_evidence
                .entry(ContractField::Blockers)
                .or_default()
                .push(evidence.clone());
        }
    }

    let finished = facts
        .iter()
        .filter_map(ReducedFact::finished_action_id)
        .collect::<BTreeSet<_>>();
    for fact in facts {
        let ReducedFact::ActionRequested {
            native_action_id,
            redacted_input: Some(input),
            evidence,
            ..
        } = fact
        else {
            continue;
        };
        if finished.contains(native_action_id.as_str()) {
            continue;
        }
        if let Some(text) = safe_text(input, DisclosureClass::ObservedState, policy)? {
            draft.next_actions.push(text);
            draft
                .field_evidence
                .entry(ContractField::NextActions)
                .or_default()
                .push(evidence.clone());
        }
    }
    Ok(())
}

fn safe_text(
    text: &str,
    disclosure_class: DisclosureClass,
    policy: &RedactionPolicy,
) -> Result<Option<String>, RedactionError> {
    if text.is_empty() || text.len() > agbox_core::limits::MAX_PREVIEW_BYTES {
        return Ok(None);
    }
    let redacted = policy.redact(text, None, disclosure_class)?;
    Ok((!redacted.value().is_empty()).then(|| redacted.value().to_owned()))
}

fn derive_status(
    facts: &[&ReducedFact],
    previous: Option<WorkStatus>,
) -> (WorkStatus, Vec<EventId>) {
    let abandonment = facts
        .iter()
        .filter(|fact| fact.explicit_abandonment())
        .map(|fact| fact.evidence_id().clone())
        .collect::<Vec<_>>();
    if !abandonment.is_empty() {
        return (WorkStatus::Abandoned, abandonment);
    }

    let outcomes = latest_finished_outcomes(facts.iter().copied());
    let failed = outcomes
        .values()
        .filter(|(fact, _)| fact.failed_structured_result())
        .map(|(fact, _)| fact.evidence_id().clone())
        .collect::<Vec<_>>();
    if !failed.is_empty() {
        return (WorkStatus::Blocked, failed);
    }
    let completed = outcomes
        .values()
        .filter(|(fact, _)| fact.successful_structured_result())
        .map(|(fact, _)| fact.evidence_id().clone())
        .collect::<Vec<_>>();
    if !completed.is_empty() {
        return (WorkStatus::Completed, completed);
    }
    let active = facts
        .iter()
        .filter(|fact| fact.active_work())
        .map(|fact| fact.evidence_id().clone())
        .collect::<Vec<_>>();
    if !active.is_empty() {
        return (WorkStatus::Active, active);
    }
    (
        previous.unwrap_or(WorkStatus::Observed),
        facts
            .iter()
            .map(|fact| fact.evidence_id().clone())
            .collect(),
    )
}

#[derive(Clone, Eq, Ord, PartialEq, PartialOrd)]
struct StatusKey(OffsetDateTime, String);

impl StatusKey {
    fn for_fact(fact: &ReducedFact) -> Self {
        Self(
            fact.observed_at().unwrap_or(OffsetDateTime::UNIX_EPOCH),
            fact.evidence_id().as_str().to_owned(),
        )
    }
}

fn latest_finished_outcomes<'a>(
    facts: impl IntoIterator<Item = &'a ReducedFact>,
) -> BTreeMap<&'a str, (&'a ReducedFact, StatusKey)> {
    let mut outcomes = BTreeMap::new();
    for fact in facts {
        if let Some(action_id) = fact.finished_action_id() {
            let key = StatusKey::for_fact(fact);
            if outcomes
                .get(action_id)
                .is_none_or(|(_, existing)| key > *existing)
            {
                outcomes.insert(action_id, (fact, key));
            }
        }
    }
    outcomes
}

fn retain_field_evidence(
    draft: &mut MaterialDraft,
    provenance_evidence: &[EventId],
) -> Vec<EventId> {
    let mut retained = draft
        .field_evidence
        .values()
        .filter_map(|references| references.first().cloned())
        .collect::<Vec<_>>();
    retained.sort();
    retained.dedup();
    let mut remaining = draft
        .field_evidence
        .values()
        .flatten()
        .chain(provenance_evidence.iter())
        .cloned()
        .collect::<Vec<_>>();
    remaining.sort();
    remaining.dedup();
    for evidence in remaining {
        if retained.len() == MAX_CONTRACT_EVIDENCE_REFS {
            break;
        }
        if !retained.contains(&evidence) {
            retained.push(evidence);
        }
    }
    retained.sort();
    for references in draft.field_evidence.values_mut() {
        references.retain(|reference| retained.contains(reference));
    }
    retained
}

fn bounded_provenance<'a>(evidence: impl IntoIterator<Item = &'a EventId>) -> (Vec<EventId>, bool) {
    let mut retained = BTreeSet::new();
    let mut truncated = false;
    for reference in evidence {
        retained.insert(reference.clone());
        if retained.len() > MAX_CONTRACT_EVIDENCE_REFS {
            retained.pop_last();
            truncated = true;
        }
    }
    (retained.into_iter().collect(), truncated)
}

fn validate_field_evidence(
    draft: &MaterialDraft,
    all_evidence: &[EventId],
) -> Result<(), ContractBuildError> {
    let required = [
        (ContractField::Objective, draft.objective.is_some()),
        (ContractField::Status, true),
        (ContractField::Summary, !draft.summary.is_empty()),
        (
            ContractField::CompletedSteps,
            !draft.completed_steps.is_empty(),
        ),
        (ContractField::NextActions, !draft.next_actions.is_empty()),
        (ContractField::Blockers, !draft.blockers.is_empty()),
        (ContractField::Constraints, !draft.constraints.is_empty()),
        (
            ContractField::CompletionCriteria,
            !draft.completion_criteria.is_empty(),
        ),
        (ContractField::Artifacts, !draft.artifacts.is_empty()),
        (ContractField::Verification, !draft.verification.is_empty()),
    ];
    let valid = required.into_iter().all(|(field, non_empty)| {
        !non_empty
            || draft.field_evidence.get(&field).is_some_and(|references| {
                !references.is_empty()
                    && references
                        .iter()
                        .all(|reference| all_evidence.contains(reference))
            })
    });
    if valid {
        Ok(())
    } else {
        Err(ContractBuildError::MissingFieldEvidence)
    }
}

fn material_hash(draft: &MaterialDraft) -> Result<String, serde_json::Error> {
    let encoded = serde_json::to_vec(draft)?;
    Ok(format!("b3:{}", blake3::hash(&encoded).to_hex()))
}

fn stable_work_id(project_id: &ProjectId, evidence_refs: &[EventId]) -> WorkId {
    let mut hasher = blake3::Hasher::new();
    hasher.update(project_id.as_str().as_bytes());
    for evidence in evidence_refs {
        hasher.update(&(evidence.as_str().len() as u64).to_le_bytes());
        hasher.update(evidence.as_str().as_bytes());
    }
    let value = format!("work_{}", &hasher.finalize().to_hex()[..24]);
    WorkId::parse_wire(&value).unwrap_or_default()
}

fn stable_contract_id(work_id: &WorkId) -> ContractId {
    let value = format!(
        "contract_{}",
        &blake3::hash(work_id.as_str().as_bytes()).to_hex()[..24]
    );
    ContractId::parse_wire(&value).unwrap_or_else(|| {
        ContractId::parse_wire("contract_invalid")
            .unwrap_or_else(|| unreachable!("the static fallback contract ID is valid"))
    })
}

impl ReducedFact {
    #[must_use]
    pub fn project_id(&self) -> &ProjectId {
        match self {
            Self::AgentRunStarted { project_id, .. }
            | Self::AgentRunFinished { project_id, .. }
            | Self::SessionContext { project_id, .. }
            | Self::Artifact { project_id, .. }
            | Self::ActionRequested { project_id, .. }
            | Self::ActionFinishedObserved { project_id, .. }
            | Self::EligibleVerificationObserved { project_id, .. }
            | Self::Verification { project_id, .. }
            | Self::HumanObjective { project_id, .. }
            | Self::HumanConstraint { project_id, .. }
            | Self::AgentStatement { project_id, .. } => project_id,
        }
    }

    #[must_use]
    pub fn explicit_abandonment(&self) -> bool {
        match self {
            Self::HumanObjective {
                redacted_text: Some(text),
                ..
            }
            | Self::HumanConstraint {
                redacted_text: Some(text),
                ..
            } => {
                let normalized = text.trim().to_ascii_lowercase();
                [
                    "abandon",
                    "cancel this work",
                    "stop this work",
                    "do not continue",
                ]
                .iter()
                .any(|marker| normalized.starts_with(marker))
            }
            _ => false,
        }
    }

    #[must_use]
    pub fn current_blocker(&self) -> bool {
        self.failed_structured_result()
    }

    #[must_use]
    pub fn completion_verified(&self) -> bool {
        self.successful_structured_result()
    }

    #[must_use]
    pub fn active_work(&self) -> bool {
        matches!(
            self,
            Self::AgentRunStarted { .. }
                | Self::Artifact { .. }
                | Self::ActionRequested { .. }
                | Self::HumanObjective { .. }
                | Self::HumanConstraint { .. }
        )
    }

    fn successful_structured_result(&self) -> bool {
        matches!(
            self,
            Self::Verification {
                succeeded: true,
                basis: STRUCTURED_TOOL_RESULT,
                ..
            } | Self::EligibleVerificationObserved {
                succeeded: true,
                basis: STRUCTURED_TOOL_RESULT,
                ..
            }
        )
    }

    fn failed_structured_result(&self) -> bool {
        matches!(
            self,
            Self::Verification {
                succeeded: false,
                basis: STRUCTURED_TOOL_RESULT,
                ..
            } | Self::EligibleVerificationObserved {
                succeeded: false,
                basis: STRUCTURED_TOOL_RESULT,
                ..
            }
        )
    }

    fn finished_action_id(&self) -> Option<&str> {
        match self {
            Self::ActionFinishedObserved {
                native_action_id, ..
            }
            | Self::EligibleVerificationObserved {
                native_action_id, ..
            }
            | Self::Verification {
                native_action_id, ..
            } => Some(native_action_id),
            _ => None,
        }
    }

    fn observed_at(&self) -> Option<OffsetDateTime> {
        match self {
            Self::AgentRunStarted { observed_at, .. }
            | Self::AgentRunFinished { observed_at, .. }
            | Self::SessionContext { observed_at, .. }
            | Self::Artifact { observed_at, .. }
            | Self::ActionFinishedObserved { observed_at, .. }
            | Self::EligibleVerificationObserved { observed_at, .. }
            | Self::Verification { observed_at, .. } => Some(*observed_at),
            Self::ActionRequested { .. }
            | Self::HumanObjective { .. }
            | Self::HumanConstraint { .. }
            | Self::AgentStatement { .. } => None,
        }
    }
}
