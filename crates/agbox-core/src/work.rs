use std::fmt;

use serde::{Deserialize, Deserializer, Serialize, de};
use time::OffsetDateTime;

use crate::{
    AgentRunId, Authority, ContractId, DisclosureClass, EvidenceId, PrivacyLabel, ProjectId,
    RedactedText, RedactionError, RedactionPolicy, WorkId,
    limits::{
        MAX_CONTRACT_EVIDENCE_REFS, MAX_CONTRACT_ITEMS_PER_FIELD, MAX_CONTRACT_SERIALIZED_BYTES,
        MAX_CONTRACT_SOURCE_RUNS, MAX_INLINE_BYTES,
    },
};

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkStatus {
    Observed,
    Active,
    Blocked,
    Completed,
    Abandoned,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkEdgeKind {
    Continues,
    DependsOn,
    BlockedBy,
    Produces,
    ValidatedBy,
    Supersedes,
}

#[derive(Clone, Eq, PartialEq, Serialize)]
pub struct WorkAssertion {
    field: String,
    value: String,
    authority: Authority,
    privacy: PrivacyLabel,
    evidence_refs: Vec<EvidenceId>,
    confidence_basis_points: u16,
    disclosure_class: DisclosureClass,
}

impl fmt::Debug for WorkAssertion {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("WorkAssertion")
            .field("authority", &self.authority)
            .field("privacy", &self.privacy)
            .field("disclosure_class", &self.disclosure_class)
            .field("evidence_count", &self.evidence_refs.len())
            .field("value_bytes", &self.value.len())
            .finish_non_exhaustive()
    }
}

#[derive(Debug, thiserror::Error)]
pub enum AssertionError {
    #[error("only explicit human-intent authority and disclosure may define an instruction")]
    InstructionAuthority,
    #[error("an assertion must cite evidence")]
    MissingEvidence,
    #[error("assertion text exceeds the inline-content bound")]
    TextTooLarge,
    #[error("assertion evidence exceeds its bound")]
    TooManyEvidenceRefs,
    #[error("assertion confidence exceeds 10,000 basis points")]
    InvalidConfidence,
    #[error("assertion text could not be redacted")]
    Redaction(#[from] RedactionError),
    #[error("assertion disclosure class is forbidden")]
    ForbiddenDisclosure,
}

impl WorkAssertion {
    /// Constructs a validated evidence-backed assertion.
    ///
    /// # Errors
    ///
    /// Returns [`AssertionError`] when text, evidence, confidence, or
    /// instruction authority violates the assertion contract.
    pub fn new(
        field: String,
        value: RedactedText,
        authority: Authority,
        privacy: PrivacyLabel,
        evidence_refs: Vec<EvidenceId>,
        confidence_basis_points: u16,
    ) -> Result<Self, AssertionError> {
        let disclosure_class = value.disclosure_class();
        let assertion = Self {
            field,
            value: value.into_value(),
            authority,
            privacy,
            evidence_refs,
            confidence_basis_points,
            disclosure_class,
        };
        assertion.validate()?;
        Ok(assertion)
    }

    /// Constructs an instruction backed by explicit human intent and evidence.
    ///
    /// # Errors
    ///
    /// Returns [`AssertionError`] for non-human authority, missing or excessive
    /// evidence, or oversized text.
    pub fn instruction(
        value: RedactedText,
        authority: Authority,
        privacy: PrivacyLabel,
        evidence_refs: Vec<EvidenceId>,
    ) -> Result<Self, AssertionError> {
        Self::new(
            "next_action".into(),
            value,
            authority,
            privacy,
            evidence_refs,
            10_000,
        )
    }

    /// Revalidates the assertion before a store write.
    ///
    /// # Errors
    ///
    /// Returns [`AssertionError`] when an assertion invariant is violated.
    pub fn validate(&self) -> Result<(), AssertionError> {
        if self.field == "next_action"
            && (!self.authority.may_define_instruction()
                || self.disclosure_class != DisclosureClass::HumanIntent)
        {
            return Err(AssertionError::InstructionAuthority);
        }
        if self.evidence_refs.is_empty() {
            return Err(AssertionError::MissingEvidence);
        }
        if self.field.len() > MAX_INLINE_BYTES || self.value.len() > MAX_INLINE_BYTES {
            return Err(AssertionError::TextTooLarge);
        }
        if self.evidence_refs.len() > MAX_CONTRACT_EVIDENCE_REFS {
            return Err(AssertionError::TooManyEvidenceRefs);
        }
        if self.confidence_basis_points > 10_000 {
            return Err(AssertionError::InvalidConfidence);
        }
        if !self.disclosure_class.is_transferable() {
            return Err(AssertionError::ForbiddenDisclosure);
        }
        Ok(())
    }

    #[must_use]
    pub fn field(&self) -> &str {
        &self.field
    }

    #[must_use]
    pub fn value(&self) -> &str {
        &self.value
    }

    #[must_use]
    pub fn authority(&self) -> Authority {
        self.authority
    }

    #[must_use]
    pub fn privacy(&self) -> PrivacyLabel {
        self.privacy
    }

    #[must_use]
    pub fn evidence_refs(&self) -> &[EvidenceId] {
        &self.evidence_refs
    }

    #[must_use]
    pub fn confidence_basis_points(&self) -> u16 {
        self.confidence_basis_points
    }

    #[must_use]
    pub fn disclosure_class(&self) -> DisclosureClass {
        self.disclosure_class
    }
}

#[derive(Deserialize)]
struct WorkAssertionWire {
    field: String,
    value: String,
    authority: Authority,
    privacy: PrivacyLabel,
    evidence_refs: Vec<EvidenceId>,
    confidence_basis_points: u16,
    disclosure_class: DisclosureClass,
}

impl<'de> Deserialize<'de> for WorkAssertion {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = WorkAssertionWire::deserialize(deserializer)?;
        let redacted_value = RedactionPolicy::new()
            .and_then(|policy| policy.redact(&wire.value, None, wire.disclosure_class))
            .map_err(de::Error::custom)?;
        Self::new(
            wire.field,
            redacted_value,
            wire.authority,
            wire.privacy,
            wire.evidence_refs,
            wire.confidence_basis_points,
        )
        .map_err(de::Error::custom)
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct WorkEdge {
    from: WorkId,
    to: WorkId,
    kind: WorkEdgeKind,
    evidence_refs: Vec<EvidenceId>,
}

#[derive(Debug, thiserror::Error)]
pub enum WorkEdgeError {
    #[error("a work edge must cite evidence")]
    MissingEvidence,
    #[error("work-edge evidence exceeds its bound")]
    TooManyEvidenceRefs,
}

impl WorkEdge {
    /// Constructs a validated evidence-backed relationship between work items.
    ///
    /// # Errors
    ///
    /// Returns [`WorkEdgeError`] when evidence is missing or exceeds its bound.
    pub fn new(
        from: WorkId,
        to: WorkId,
        kind: WorkEdgeKind,
        evidence_refs: Vec<EvidenceId>,
    ) -> Result<Self, WorkEdgeError> {
        let edge = Self {
            from,
            to,
            kind,
            evidence_refs,
        };
        edge.validate()?;
        Ok(edge)
    }

    /// Revalidates the edge before a store write.
    ///
    /// # Errors
    ///
    /// Returns [`WorkEdgeError`] when an edge invariant is violated.
    pub fn validate(&self) -> Result<(), WorkEdgeError> {
        if self.evidence_refs.is_empty() {
            return Err(WorkEdgeError::MissingEvidence);
        }
        if self.evidence_refs.len() > MAX_CONTRACT_EVIDENCE_REFS {
            return Err(WorkEdgeError::TooManyEvidenceRefs);
        }
        Ok(())
    }

    #[must_use]
    pub fn from(&self) -> &WorkId {
        &self.from
    }

    #[must_use]
    pub fn to(&self) -> &WorkId {
        &self.to
    }

    #[must_use]
    pub fn kind(&self) -> WorkEdgeKind {
        self.kind
    }

    #[must_use]
    pub fn evidence_refs(&self) -> &[EvidenceId] {
        &self.evidence_refs
    }
}

#[derive(Deserialize)]
struct WorkEdgeWire {
    from: WorkId,
    to: WorkId,
    kind: WorkEdgeKind,
    evidence_refs: Vec<EvidenceId>,
}

impl<'de> Deserialize<'de> for WorkEdge {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = WorkEdgeWire::deserialize(deserializer)?;
        Self::new(wire.from, wire.to, wire.kind, wire.evidence_refs).map_err(de::Error::custom)
    }
}

#[derive(Clone)]
pub struct WorkContractRevisionDraft {
    pub contract_id: ContractId,
    pub work_id: WorkId,
    pub revision: u64,
    pub project_id: ProjectId,
    pub objective: Option<RedactedText>,
    pub status: WorkStatus,
    pub summary: RedactedText,
    pub completed_steps: Vec<RedactedText>,
    pub next_actions: Vec<RedactedText>,
    pub blockers: Vec<RedactedText>,
    pub constraints: Vec<RedactedText>,
    pub completion_criteria: Vec<RedactedText>,
    pub artifacts: Vec<RedactedText>,
    pub verification: Vec<RedactedText>,
    pub source_runs: Vec<AgentRunId>,
    pub evidence_refs: Vec<EvidenceId>,
    pub confidence_basis_points: u16,
    pub created_at: OffsetDateTime,
    pub extractor_version: String,
    pub disclosure_class: DisclosureClass,
}

impl fmt::Debug for WorkContractRevisionDraft {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        SanitizedContractDebug::from_draft(self).fmt(formatter)
    }
}

#[derive(Clone, Eq, PartialEq, Serialize)]
pub struct WorkContractRevision {
    contract_id: ContractId,
    work_id: WorkId,
    revision: u64,
    project_id: ProjectId,
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
    source_runs: Vec<AgentRunId>,
    evidence_refs: Vec<EvidenceId>,
    confidence_basis_points: u16,
    created_at: OffsetDateTime,
    extractor_version: String,
    disclosure_class: DisclosureClass,
}

impl fmt::Debug for WorkContractRevision {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        SanitizedContractDebug::from_revision(self).fmt(formatter)
    }
}

struct SanitizedContractDebug<'a> {
    name: &'static str,
    contract_id: &'a ContractId,
    work_id: &'a WorkId,
    objective_bytes: usize,
    summary_bytes: usize,
    completed_steps_count: usize,
    next_actions_count: usize,
    blockers_count: usize,
    constraints_count: usize,
    completion_criteria_count: usize,
    artifacts_count: usize,
    verification_count: usize,
    source_run_count: usize,
    evidence_count: usize,
    extractor_version_bytes: usize,
    disclosure_class: DisclosureClass,
}

impl<'a> SanitizedContractDebug<'a> {
    fn from_draft(draft: &'a WorkContractRevisionDraft) -> Self {
        Self {
            name: "WorkContractRevisionDraft",
            contract_id: &draft.contract_id,
            work_id: &draft.work_id,
            objective_bytes: draft
                .objective
                .as_ref()
                .map_or(0, |value| value.value().len()),
            summary_bytes: draft.summary.value().len(),
            completed_steps_count: draft.completed_steps.len(),
            next_actions_count: draft.next_actions.len(),
            blockers_count: draft.blockers.len(),
            constraints_count: draft.constraints.len(),
            completion_criteria_count: draft.completion_criteria.len(),
            artifacts_count: draft.artifacts.len(),
            verification_count: draft.verification.len(),
            source_run_count: draft.source_runs.len(),
            evidence_count: draft.evidence_refs.len(),
            extractor_version_bytes: draft.extractor_version.len(),
            disclosure_class: draft.disclosure_class,
        }
    }

    fn from_revision(revision: &'a WorkContractRevision) -> Self {
        Self {
            name: "WorkContractRevision",
            contract_id: &revision.contract_id,
            work_id: &revision.work_id,
            objective_bytes: revision.objective.as_ref().map_or(0, String::len),
            summary_bytes: revision.summary.len(),
            completed_steps_count: revision.completed_steps.len(),
            next_actions_count: revision.next_actions.len(),
            blockers_count: revision.blockers.len(),
            constraints_count: revision.constraints.len(),
            completion_criteria_count: revision.completion_criteria.len(),
            artifacts_count: revision.artifacts.len(),
            verification_count: revision.verification.len(),
            source_run_count: revision.source_runs.len(),
            evidence_count: revision.evidence_refs.len(),
            extractor_version_bytes: revision.extractor_version.len(),
            disclosure_class: revision.disclosure_class,
        }
    }
}

impl fmt::Debug for SanitizedContractDebug<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct(self.name)
            .field("contract_id", self.contract_id)
            .field("work_id", self.work_id)
            .field("objective_bytes", &self.objective_bytes)
            .field("summary_bytes", &self.summary_bytes)
            .field("completed_steps_count", &self.completed_steps_count)
            .field("next_actions_count", &self.next_actions_count)
            .field("blockers_count", &self.blockers_count)
            .field("constraints_count", &self.constraints_count)
            .field("completion_criteria_count", &self.completion_criteria_count)
            .field("artifacts_count", &self.artifacts_count)
            .field("verification_count", &self.verification_count)
            .field("source_run_count", &self.source_run_count)
            .field("evidence_count", &self.evidence_count)
            .field("disclosure_class", &self.disclosure_class)
            .field("extractor_version_bytes", &self.extractor_version_bytes)
            .finish_non_exhaustive()
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ContractError {
    #[error("{0} exceeds the inline-content bound")]
    TextTooLarge(&'static str),
    #[error("{0} exceeds its item-count bound")]
    TooManyItems(&'static str),
    #[error("source runs exceed their bound")]
    TooManySourceRuns,
    #[error("a work contract must cite evidence")]
    MissingEvidence,
    #[error("contract evidence exceeds its bound")]
    TooManyEvidenceRefs,
    #[error("contract confidence exceeds 10,000 basis points")]
    InvalidConfidence,
    #[error("serialized contract exceeds its bound")]
    SerializedTooLarge,
    #[error("contract serialization failed")]
    Serialization(#[from] serde_json::Error),
    #[error("contract text could not be redacted")]
    Redaction(#[from] RedactionError),
    #[error("contract disclosure class is forbidden")]
    ForbiddenDisclosure,
    #[error("contract semantic text has mismatched disclosure provenance")]
    DisclosureMismatch,
}

impl WorkContractRevision {
    /// Constructs an immutable, validated contract revision from a draft.
    ///
    /// # Errors
    ///
    /// Returns [`ContractError`] when any text, list, evidence, confidence, or
    /// final serialized-size invariant is violated.
    pub fn new(draft: WorkContractRevisionDraft) -> Result<Self, ContractError> {
        validate_draft_disclosure(&draft)?;
        let revision = Self {
            contract_id: draft.contract_id,
            work_id: draft.work_id,
            revision: draft.revision,
            project_id: draft.project_id,
            objective: draft.objective.map(RedactedText::into_value),
            status: draft.status,
            summary: draft.summary.into_value(),
            completed_steps: into_strings(draft.completed_steps),
            next_actions: into_strings(draft.next_actions),
            blockers: into_strings(draft.blockers),
            constraints: into_strings(draft.constraints),
            completion_criteria: into_strings(draft.completion_criteria),
            artifacts: into_strings(draft.artifacts),
            verification: into_strings(draft.verification),
            source_runs: draft.source_runs,
            evidence_refs: draft.evidence_refs,
            confidence_basis_points: draft.confidence_basis_points,
            created_at: draft.created_at,
            extractor_version: draft.extractor_version,
            disclosure_class: draft.disclosure_class,
        };
        revision.validate()?;
        Ok(revision)
    }

    /// Revalidates the complete revision before a store write.
    ///
    /// # Errors
    ///
    /// Returns [`ContractError`] when any revision invariant is violated.
    pub fn validate(&self) -> Result<(), ContractError> {
        validate_optional_text("objective", self.objective.as_ref())?;
        validate_text("summary", &self.summary)?;
        validate_text_list("completed_steps", &self.completed_steps)?;
        validate_text_list("next_actions", &self.next_actions)?;
        validate_text_list("blockers", &self.blockers)?;
        validate_text_list("constraints", &self.constraints)?;
        validate_text_list("completion_criteria", &self.completion_criteria)?;
        validate_text_list("artifacts", &self.artifacts)?;
        validate_text_list("verification", &self.verification)?;
        validate_text("extractor_version", &self.extractor_version)?;
        if self.source_runs.len() > MAX_CONTRACT_SOURCE_RUNS {
            return Err(ContractError::TooManySourceRuns);
        }
        if self.evidence_refs.is_empty() {
            return Err(ContractError::MissingEvidence);
        }
        if self.evidence_refs.len() > MAX_CONTRACT_EVIDENCE_REFS {
            return Err(ContractError::TooManyEvidenceRefs);
        }
        if self.confidence_basis_points > 10_000 {
            return Err(ContractError::InvalidConfidence);
        }
        if !self.disclosure_class.is_transferable() {
            return Err(ContractError::ForbiddenDisclosure);
        }
        if serde_json::to_vec(self)?.len() > MAX_CONTRACT_SERIALIZED_BYTES {
            return Err(ContractError::SerializedTooLarge);
        }
        Ok(())
    }

    #[must_use]
    pub fn contract_id(&self) -> &ContractId {
        &self.contract_id
    }

    #[must_use]
    pub fn work_id(&self) -> &WorkId {
        &self.work_id
    }

    #[must_use]
    pub fn revision(&self) -> u64 {
        self.revision
    }

    #[must_use]
    pub fn project_id(&self) -> &ProjectId {
        &self.project_id
    }

    #[must_use]
    pub fn objective(&self) -> Option<&str> {
        self.objective.as_deref()
    }

    #[must_use]
    pub fn status(&self) -> WorkStatus {
        self.status
    }

    #[must_use]
    pub fn summary(&self) -> &str {
        &self.summary
    }

    #[must_use]
    pub fn completed_steps(&self) -> &[String] {
        &self.completed_steps
    }

    #[must_use]
    pub fn next_actions(&self) -> &[String] {
        &self.next_actions
    }

    #[must_use]
    pub fn blockers(&self) -> &[String] {
        &self.blockers
    }

    #[must_use]
    pub fn constraints(&self) -> &[String] {
        &self.constraints
    }

    #[must_use]
    pub fn completion_criteria(&self) -> &[String] {
        &self.completion_criteria
    }

    #[must_use]
    pub fn artifacts(&self) -> &[String] {
        &self.artifacts
    }

    #[must_use]
    pub fn verification(&self) -> &[String] {
        &self.verification
    }

    #[must_use]
    pub fn source_runs(&self) -> &[AgentRunId] {
        &self.source_runs
    }

    #[must_use]
    pub fn evidence_refs(&self) -> &[EvidenceId] {
        &self.evidence_refs
    }

    #[must_use]
    pub fn confidence_basis_points(&self) -> u16 {
        self.confidence_basis_points
    }

    #[must_use]
    pub fn created_at(&self) -> OffsetDateTime {
        self.created_at
    }

    #[must_use]
    pub fn extractor_version(&self) -> &str {
        &self.extractor_version
    }

    #[must_use]
    pub fn disclosure_class(&self) -> DisclosureClass {
        self.disclosure_class
    }
}

fn validate_draft_disclosure(draft: &WorkContractRevisionDraft) -> Result<(), ContractError> {
    let class = draft.disclosure_class;
    if !class.is_transferable() {
        return Err(ContractError::ForbiddenDisclosure);
    }
    let objective_matches = draft
        .objective
        .as_ref()
        .is_none_or(|value| value.disclosure_class() == class);
    let all_lists_match = [
        draft.completed_steps.as_slice(),
        draft.next_actions.as_slice(),
        draft.blockers.as_slice(),
        draft.constraints.as_slice(),
        draft.completion_criteria.as_slice(),
        draft.artifacts.as_slice(),
        draft.verification.as_slice(),
    ]
    .into_iter()
    .flatten()
    .all(|value| value.disclosure_class() == class);
    if !objective_matches || draft.summary.disclosure_class() != class || !all_lists_match {
        return Err(ContractError::DisclosureMismatch);
    }
    Ok(())
}

fn into_strings(values: Vec<RedactedText>) -> Vec<String> {
    values.into_iter().map(RedactedText::into_value).collect()
}

fn validate_optional_text(
    field: &'static str,
    value: Option<&String>,
) -> Result<(), ContractError> {
    if let Some(value) = value {
        validate_text(field, value)?;
    }
    Ok(())
}

fn validate_text(field: &'static str, value: &str) -> Result<(), ContractError> {
    if value.len() > MAX_INLINE_BYTES {
        return Err(ContractError::TextTooLarge(field));
    }
    Ok(())
}

fn validate_text_list(field: &'static str, values: &[String]) -> Result<(), ContractError> {
    if values.len() > MAX_CONTRACT_ITEMS_PER_FIELD {
        return Err(ContractError::TooManyItems(field));
    }
    for value in values {
        validate_text(field, value)?;
    }
    Ok(())
}

#[derive(Deserialize, Serialize)]
struct WorkContractRevisionWire {
    contract_id: ContractId,
    work_id: WorkId,
    revision: u64,
    project_id: ProjectId,
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
    source_runs: Vec<AgentRunId>,
    evidence_refs: Vec<EvidenceId>,
    confidence_basis_points: u16,
    created_at: OffsetDateTime,
    extractor_version: String,
    disclosure_class: DisclosureClass,
}

impl WorkContractRevisionWire {
    fn validate_serialized_size(&self) -> Result<(), ContractError> {
        if serde_json::to_vec(self)?.len() > MAX_CONTRACT_SERIALIZED_BYTES {
            return Err(ContractError::SerializedTooLarge);
        }
        Ok(())
    }

    fn into_draft(self) -> Result<WorkContractRevisionDraft, ContractError> {
        let policy = RedactionPolicy::new()?;
        Ok(WorkContractRevisionDraft {
            contract_id: self.contract_id,
            work_id: self.work_id,
            revision: self.revision,
            project_id: self.project_id,
            objective: self
                .objective
                .map(|value| policy.redact(&value, None, self.disclosure_class))
                .transpose()?,
            status: self.status,
            summary: policy.redact(&self.summary, None, self.disclosure_class)?,
            completed_steps: redact_wire_list(
                "completed_steps",
                self.completed_steps,
                self.disclosure_class,
                &policy,
            )?,
            next_actions: redact_wire_list(
                "next_actions",
                self.next_actions,
                self.disclosure_class,
                &policy,
            )?,
            blockers: redact_wire_list("blockers", self.blockers, self.disclosure_class, &policy)?,
            constraints: redact_wire_list(
                "constraints",
                self.constraints,
                self.disclosure_class,
                &policy,
            )?,
            completion_criteria: redact_wire_list(
                "completion_criteria",
                self.completion_criteria,
                self.disclosure_class,
                &policy,
            )?,
            artifacts: redact_wire_list(
                "artifacts",
                self.artifacts,
                self.disclosure_class,
                &policy,
            )?,
            verification: redact_wire_list(
                "verification",
                self.verification,
                self.disclosure_class,
                &policy,
            )?,
            source_runs: self.source_runs,
            evidence_refs: self.evidence_refs,
            confidence_basis_points: self.confidence_basis_points,
            created_at: self.created_at,
            extractor_version: self.extractor_version,
            disclosure_class: self.disclosure_class,
        })
    }
}

fn redact_wire_list(
    field: &'static str,
    values: Vec<String>,
    disclosure_class: DisclosureClass,
    policy: &RedactionPolicy,
) -> Result<Vec<RedactedText>, ContractError> {
    if values.len() > MAX_CONTRACT_ITEMS_PER_FIELD {
        return Err(ContractError::TooManyItems(field));
    }
    values
        .into_iter()
        .map(|value| {
            policy
                .redact(&value, None, disclosure_class)
                .map_err(ContractError::from)
        })
        .collect()
}

impl<'de> Deserialize<'de> for WorkContractRevision {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = WorkContractRevisionWire::deserialize(deserializer)?;
        wire.validate_serialized_size().map_err(de::Error::custom)?;
        let draft = wire.into_draft().map_err(de::Error::custom)?;
        Self::new(draft).map_err(de::Error::custom)
    }
}
