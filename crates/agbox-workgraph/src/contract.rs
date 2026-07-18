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
    /// Stable digest of the complete fact slice used for this revision.
    ///
    /// This is intentionally independent of the capped display evidence so a
    /// large aggregate replay remains idempotent.
    #[serde(default)]
    pub fact_set_digest: String,
    pub material_content_hash: String,
    #[serde(default)]
    projection_state: ProjectionState,
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

        let fact_set_digest = fact_set_digest(facts);
        if let Some(previous) = previous
            && previous.fact_set_digest == fact_set_digest
        {
            return Ok(previous.clone());
        }
        let policy = RedactionPolicy::new()?;
        let mut draft = MaterialDraft::from_previous(previous);
        let previous_activity = draft.activity_projection();
        let previous_authoritative = draft.projection_state.authoritative_digest();
        apply_text_facts(&mut draft, facts, &policy)?;
        apply_artifact_facts(&mut draft, facts, &policy)?;
        apply_action_facts(&mut draft, facts, &policy)?;
        draft.normalize();
        let activity_changed = draft.activity_projection() != previous_activity;
        let new_active_observation = draft.projection_state.observe_active_facts(facts);
        let authoritative_changed =
            draft.projection_state.authoritative_digest() != previous_authoritative;
        let new_authoritative_success =
            authoritative_changed && facts.iter().any(ReducedFact::successful_structured_result);
        if previous.is_some_and(|contract| contract.status == WorkStatus::Completed)
            && (activity_changed || new_active_observation)
            && !new_authoritative_success
        {
            draft.projection_state.completion_reopened = true;
        }
        if draft.projection_state.completion_reopened
            && new_authoritative_success
            && !draft.projection_state.has_failed_authority()
        {
            draft.projection_state.completion_reopened = false;
        }
        let (status, status_evidence) = derive_status(
            &draft.projection_state,
            facts,
            previous.map(|value| value.status),
            activity_changed,
        );
        draft.status = status;
        draft
            .field_evidence
            .insert(ContractField::Status, status_evidence);
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
            fact_set_digest,
            material_content_hash,
            projection_state: draft.projection_state,
        })
    }
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
struct ProjectionState {
    actions: BTreeMap<String, ActionProjection>,
    #[serde(default)]
    completion_reopened: bool,
    #[serde(default)]
    active_identity_watermark: BTreeSet<EventId>,
    #[serde(default)]
    active_identity_truncated: bool,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
struct ActionProjection {
    request: Option<ProjectedText>,
    finished: bool,
    authoritative: Option<ProjectedOutcome>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
struct ProjectedText {
    text: String,
    evidence: EventId,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
struct ProjectedOutcome {
    command: Option<String>,
    succeeded: bool,
    observed_at: OffsetDateTime,
    evidence: EventId,
}

impl ProjectionState {
    fn authoritative_digest(&self) -> Vec<(String, Option<ProjectedOutcome>)> {
        self.actions
            .iter()
            .map(|(key, action)| (key.clone(), action.authoritative.clone()))
            .collect()
    }

    fn has_failed_authority(&self) -> bool {
        self.actions
            .values()
            .filter_map(|action| action.authoritative.as_ref())
            .any(|outcome| !outcome.succeeded)
    }

    fn observe_active_facts(&mut self, facts: &[ReducedFact]) -> bool {
        let mut observed_new_identity = false;
        for fact in facts.iter().filter(|fact| fact.active_work()) {
            observed_new_identity |= self
                .active_identity_watermark
                .insert(fact.evidence_id().clone());
        }
        // A reducer page cannot contain more identities than this bound. If a
        // long-lived contract exceeds it across pages, deterministic eviction
        // degrades conservatively: an evicted replay may reopen work, but an
        // unseen active fact is never silently discarded as a lexical loser.
        while self.active_identity_watermark.len() > agbox_core::limits::MAX_BATCH_RECORDS {
            self.active_identity_watermark.pop_last();
            self.active_identity_truncated = true;
        }
        observed_new_identity
    }
}

#[derive(Clone, Eq, PartialEq)]
struct ActivityProjection {
    objective: Option<String>,
    summary: String,
    next_actions: Vec<String>,
    constraints: Vec<String>,
    artifacts: Vec<String>,
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
    projection_state: ProjectionState,
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
                projection_state: ProjectionState::default(),
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
                projection_state: contract.projection_state.clone(),
            },
        )
    }

    fn activity_projection(&self) -> ActivityProjection {
        ActivityProjection {
            objective: self.objective.clone(),
            summary: self.summary.clone(),
            next_actions: self.next_actions.clone(),
            constraints: self.constraints.clone(),
            artifacts: self.artifacts.clone(),
        }
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
                observed_at,
                evidence,
                ..
            } => safe_text(text, DisclosureClass::HumanIntent, policy)
                .transpose()
                .map(|value| {
                    value.map(|value| ((*observed_at, evidence.as_str()), evidence.clone(), value))
                }),
            _ => None,
        })
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .max_by(|left, right| left.0.cmp(&right.0))
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
                observed_at,
                evidence,
                ..
            } => safe_text(text, DisclosureClass::HumanIntent, policy)
                .transpose()
                .map(|value| {
                    value.map(|value| ((*observed_at, evidence.as_str()), evidence.clone(), value))
                }),
            _ => None,
        })
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .max_by(|left, right| left.0.cmp(&right.0))
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
                observed_at,
                evidence,
                ..
            } => safe_text(text, DisclosureClass::AgentStatement, policy)
                .transpose()
                .map(|value| {
                    value.map(|value| ((*observed_at, evidence.as_str()), evidence.clone(), value))
                }),
            _ => None,
        })
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .max_by(|left, right| left.0.cmp(&right.0))
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
    for fact in facts {
        update_action_projection(&mut draft.projection_state, fact, policy)?;
    }
    while draft.projection_state.actions.len() > MAX_CONTRACT_EVIDENCE_REFS {
        draft.projection_state.actions.pop_last();
    }
    project_action_fields(draft);
    Ok(())
}

fn update_action_projection(
    projection: &mut ProjectionState,
    fact: &ReducedFact,
    policy: &RedactionPolicy,
) -> Result<(), ContractBuildError> {
    let Some(key) = fact.action_projection_key() else {
        return Ok(());
    };
    let action = projection.actions.entry(key).or_default();
    match fact {
        ReducedFact::ActionRequested {
            redacted_input: Some(input),
            evidence,
            ..
        } => {
            if let Some(text) = safe_text(input, DisclosureClass::ObservedState, policy)? {
                let candidate = ProjectedText {
                    text,
                    evidence: evidence.clone(),
                };
                if action
                    .request
                    .as_ref()
                    .is_none_or(|existing| candidate.evidence > existing.evidence)
                {
                    action.request = Some(candidate);
                }
            }
        }
        ReducedFact::ActionFinishedObserved { .. } => action.finished = true,
        ReducedFact::EligibleVerificationObserved {
            succeeded,
            observed_at,
            evidence,
            ..
        } => {
            action.finished = true;
            update_authoritative(
                action,
                ProjectedOutcome {
                    command: None,
                    succeeded: *succeeded,
                    observed_at: *observed_at,
                    evidence: evidence.clone(),
                },
            );
        }
        ReducedFact::Verification {
            command,
            succeeded,
            observed_at,
            evidence,
            ..
        } => {
            action.finished = true;
            let command = command
                .as_deref()
                .map(|text| safe_text(text, DisclosureClass::ToolResult, policy))
                .transpose()?
                .flatten();
            update_authoritative(
                action,
                ProjectedOutcome {
                    command,
                    succeeded: *succeeded,
                    observed_at: *observed_at,
                    evidence: evidence.clone(),
                },
            );
        }
        _ => {}
    }
    Ok(())
}

fn project_action_fields(draft: &mut MaterialDraft) {
    for field in [
        ContractField::CompletedSteps,
        ContractField::NextActions,
        ContractField::Blockers,
        ContractField::Verification,
    ] {
        draft.field_evidence.remove(&field);
    }
    draft.completed_steps.clear();
    draft.next_actions.clear();
    draft.blockers.clear();
    draft.verification.clear();

    for action in draft.projection_state.actions.values() {
        if !action.finished
            && let Some(request) = &action.request
        {
            draft.next_actions.push(request.text.clone());
            draft
                .field_evidence
                .entry(ContractField::NextActions)
                .or_default()
                .push(request.evidence.clone());
        }
        let Some(outcome) = &action.authoritative else {
            continue;
        };
        let Some(command) = &outcome.command else {
            continue;
        };
        draft.verification.push(command.clone());
        draft
            .field_evidence
            .entry(ContractField::Verification)
            .or_default()
            .push(outcome.evidence.clone());
        let (values, field) = if outcome.succeeded {
            (&mut draft.completed_steps, ContractField::CompletedSteps)
        } else {
            (&mut draft.blockers, ContractField::Blockers)
        };
        values.push(command.clone());
        draft
            .field_evidence
            .entry(field)
            .or_default()
            .push(outcome.evidence.clone());
    }
}

fn update_authoritative(action: &mut ActionProjection, candidate: ProjectedOutcome) {
    let candidate_key = (candidate.observed_at, candidate.evidence.as_str());
    if action
        .authoritative
        .as_ref()
        .is_none_or(|existing| candidate_key > (existing.observed_at, existing.evidence.as_str()))
    {
        action.authoritative = Some(candidate);
    }
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
    projection: &ProjectionState,
    facts: &[ReducedFact],
    previous: Option<WorkStatus>,
    activity_changed: bool,
) -> (WorkStatus, Vec<EventId>) {
    let abandonment = facts
        .iter()
        .filter(|fact| fact.explicit_abandonment())
        .map(|fact| fact.evidence_id().clone())
        .collect::<Vec<_>>();
    if !abandonment.is_empty() {
        return (WorkStatus::Abandoned, abandonment);
    }

    let failed = projection
        .actions
        .values()
        .filter_map(|action| action.authoritative.as_ref())
        .filter(|outcome| !outcome.succeeded)
        .map(|outcome| outcome.evidence.clone())
        .collect::<Vec<_>>();
    if !failed.is_empty() {
        return (WorkStatus::Blocked, failed);
    }
    if projection.completion_reopened {
        return (
            WorkStatus::Active,
            facts
                .iter()
                .map(|fact| fact.evidence_id().clone())
                .collect(),
        );
    }
    let completed = projection
        .actions
        .values()
        .filter_map(|action| action.authoritative.as_ref())
        .filter(|outcome| outcome.succeeded)
        .map(|outcome| outcome.evidence.clone())
        .collect::<Vec<_>>();
    if !completed.is_empty() {
        return (WorkStatus::Completed, completed);
    }
    let active = facts
        .iter()
        .filter(|fact| fact.active_work())
        .map(|fact| fact.evidence_id().clone())
        .collect::<Vec<_>>();
    if activity_changed && !active.is_empty() {
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

fn fact_set_digest(facts: &[ReducedFact]) -> String {
    let mut evidence = facts
        .iter()
        .map(ReducedFact::evidence_id)
        .collect::<Vec<_>>();
    evidence.sort();
    evidence.dedup();
    let mut hasher = blake3::Hasher::new();
    for reference in evidence {
        hasher.update(&(reference.as_str().len() as u64).to_le_bytes());
        hasher.update(reference.as_str().as_bytes());
    }
    format!("b3:{}", hasher.finalize().to_hex())
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

    fn action_projection_key(&self) -> Option<String> {
        match self {
            Self::ActionRequested {
                session_id,
                native_action_id,
                ..
            }
            | Self::ActionFinishedObserved {
                session_id,
                native_action_id,
                ..
            }
            | Self::EligibleVerificationObserved {
                session_id,
                native_action_id,
                ..
            }
            | Self::Verification {
                session_id,
                native_action_id,
                ..
            } => {
                let mut hasher = blake3::Hasher::new();
                for value in [session_id.as_str(), native_action_id.as_str()] {
                    hasher.update(&(value.len() as u64).to_le_bytes());
                    hasher.update(value.as_bytes());
                }
                Some(format!("b3:{}", hasher.finalize().to_hex()))
            }
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
            | Self::Verification { observed_at, .. }
            | Self::HumanObjective { observed_at, .. }
            | Self::HumanConstraint { observed_at, .. }
            | Self::AgentStatement { observed_at, .. } => Some(*observed_at),
            Self::ActionRequested { .. } => None,
        }
    }
}
