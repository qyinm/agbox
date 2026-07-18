use std::{
    collections::{BTreeMap, HashSet},
    fmt,
    sync::Arc,
};

use agbox_core::{
    ActivityEventV1, ContentRef, DisclosureClass, EventId, EvidenceId, PrivacyLabel, ProjectId,
    Provider, SessionId, SourceObservation, WorkId, WorkStatus,
};
use rusqlite::{OptionalExtension, Transaction, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use time::{OffsetDateTime, format_description::well_known::Rfc3339};
use tokio::sync::{mpsc, oneshot};
use zeroize::Zeroizing;

use crate::{EvidenceContext, EvidenceOwnerRef, EvidenceVault, StoreError};

pub const MAX_BATCH_BYTES: usize = agbox_core::limits::MAX_BATCH_SEMANTIC_BYTES;
pub const MAX_BATCH_RECORDS: usize = agbox_core::limits::MAX_BATCH_RECORDS;
pub const WRITER_QUEUE_CAPACITY: usize = 32;
pub const MAX_GRAPH_FACTS: usize = agbox_core::limits::MAX_BATCH_RECORDS;

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct GraphRunRow {
    pub run_id: String,
    pub project_id: ProjectId,
    pub provider: Provider,
    pub session_id: SessionId,
    pub observed_at: OffsetDateTime,
    pub finished: bool,
    pub succeeded: Option<bool>,
    pub evidence_event_id: EventId,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct GraphSessionContextRow {
    pub context_run_id: String,
    pub project_id: ProjectId,
    pub session_id: SessionId,
    pub provider: Provider,
    pub branch_hash: Option<String>,
    pub observed_at: OffsetDateTime,
    pub evidence_event_id: EventId,
}

#[derive(Clone, Eq, PartialEq, Serialize)]
pub struct GraphActionRow {
    pub project_id: ProjectId,
    pub session_id: SessionId,
    pub native_action_id: String,
    pub request_event_id: EventId,
    pub tool_name: String,
    pub input_hash: String,
    pub redacted_command: Option<String>,
}

impl fmt::Debug for GraphActionRow {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("GraphActionRow")
            .field("project_id", &self.project_id)
            .field("session_id", &self.session_id)
            .field("native_action_id", &self.native_action_id)
            .field("request_event_id", &self.request_event_id)
            .field("tool_name", &self.tool_name)
            .field("input_hash", &self.input_hash)
            .field(
                "redacted_command_bytes",
                &self.redacted_command.as_ref().map_or(0, String::len),
            )
            .finish()
    }
}

#[derive(Clone, Serialize)]
pub struct GraphArtifactRow {
    pub artifact_id: String,
    pub work_id: WorkId,
    pub project_id: ProjectId,
    pub path_hash: String,
    pub project_relative_path: Option<String>,
    pub content_hash: Option<String>,
    pub operation: String,
    pub observed_at: OffsetDateTime,
    pub evidence_event_id: EventId,
}

impl fmt::Debug for GraphArtifactRow {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("GraphArtifactRow")
            .field("artifact_id", &self.artifact_id)
            .field("work_id", &self.work_id)
            .field("project_id", &self.project_id)
            .field("path_hash", &self.path_hash)
            .field(
                "project_relative_path_bytes",
                &self.project_relative_path.as_ref().map_or(0, String::len),
            )
            .field("content_hash", &self.content_hash)
            .field("operation", &self.operation)
            .field("observed_at", &self.observed_at)
            .field("evidence_event_id", &self.evidence_event_id)
            .finish()
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct GraphFinishRow {
    pub verification_id: String,
    pub project_id: ProjectId,
    pub session_id: SessionId,
    pub native_action_id: String,
    pub succeeded: bool,
    pub basis: String,
    pub finish_event_id: EventId,
    pub observed_at: OffsetDateTime,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct GraphObservedFinishRow {
    pub project_id: ProjectId,
    pub session_id: SessionId,
    pub native_action_id: String,
    pub succeeded: bool,
    pub finish_event_id: EventId,
    pub observed_at: OffsetDateTime,
}

#[derive(Clone, Serialize)]
pub struct GraphWriteBatch {
    pub reducer_name: String,
    pub expected_event_seq: u64,
    pub next_event_seq: u64,
    pub next_event_id: EventId,
    pub runs: Vec<GraphRunRow>,
    pub contexts: Vec<GraphSessionContextRow>,
    pub actions: Vec<GraphActionRow>,
    pub artifacts: Vec<GraphArtifactRow>,
    pub observed_finishes: Vec<GraphObservedFinishRow>,
    pub finishes: Vec<GraphFinishRow>,
}

impl fmt::Debug for GraphWriteBatch {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("GraphWriteBatch")
            .field("reducer_name", &self.reducer_name)
            .field("expected_event_seq", &self.expected_event_seq)
            .field("next_event_seq", &self.next_event_seq)
            .field("runs", &self.runs.len())
            .field("contexts", &self.contexts.len())
            .field("actions", &self.actions.len())
            .field("artifacts", &self.artifacts.len())
            .field("observed_finishes", &self.observed_finishes.len())
            .field("finishes", &self.finishes.len())
            .finish_non_exhaustive()
    }
}

impl GraphWriteBatch {
    /// Revalidates graph cardinality, watermark, identifiers, and retained
    /// semantic byte bounds before writer submission.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::InvalidBatch`] when any bound or normalized value
    /// is invalid.
    pub fn validate(&self) -> Result<(), StoreError> {
        let fact_count = self
            .runs
            .len()
            .checked_add(self.contexts.len())
            .and_then(|value| value.checked_add(self.actions.len()))
            .and_then(|value| value.checked_add(self.artifacts.len()))
            .and_then(|value| value.checked_add(self.observed_finishes.len()))
            .and_then(|value| value.checked_add(self.finishes.len()))
            .ok_or(StoreError::InvalidBatch)?;
        if fact_count > MAX_GRAPH_FACTS
            || !bounded_identifier(&self.reducer_name)
            || self.next_event_seq == 0
            || self.next_event_seq < self.expected_event_seq
            || self.next_event_seq > i64::MAX as u64
            || self.expected_event_seq > i64::MAX as u64
            || !bounded_identifier(self.next_event_id.as_str())
        {
            return Err(StoreError::InvalidBatch);
        }
        for row in &self.runs {
            validate_graph_identity(&row.project_id, &row.session_id, &row.evidence_event_id)?;
            if !bounded_identifier(&row.run_id) || row.finished != row.succeeded.is_some() {
                return Err(StoreError::InvalidBatch);
            }
            let _ = format_timestamp(row.observed_at)?;
        }
        for row in &self.contexts {
            validate_graph_identity(&row.project_id, &row.session_id, &row.evidence_event_id)?;
            if !bounded_identifier(&row.context_run_id)
                || row
                    .branch_hash
                    .as_ref()
                    .is_some_and(|value| !bounded_metadata(value))
            {
                return Err(StoreError::InvalidBatch);
            }
            let _ = format_timestamp(row.observed_at)?;
        }
        for row in &self.actions {
            validate_graph_identity(&row.project_id, &row.session_id, &row.request_event_id)?;
            if !bounded_identifier(&row.native_action_id)
                || !bounded_metadata(&row.tool_name)
                || !bounded_metadata(&row.input_hash)
                || row
                    .redacted_command
                    .as_ref()
                    .is_some_and(|value| value.len() > agbox_core::limits::MAX_PREVIEW_BYTES)
            {
                return Err(StoreError::InvalidBatch);
            }
        }
        for row in &self.artifacts {
            validate_graph_event_identity(&row.project_id, &row.evidence_event_id)?;
            if !bounded_identifier(&row.artifact_id)
                || !bounded_identifier(row.work_id.as_str())
                || !bounded_metadata(&row.path_hash)
                || !bounded_metadata(&row.operation)
                || row
                    .content_hash
                    .as_ref()
                    .is_some_and(|value| !bounded_metadata(value))
                || row
                    .project_relative_path
                    .as_ref()
                    .is_some_and(|value| value.len() > agbox_core::limits::MAX_PREVIEW_BYTES)
            {
                return Err(StoreError::InvalidBatch);
            }
            let _ = format_timestamp(row.observed_at)?;
        }
        for row in &self.observed_finishes {
            validate_graph_identity(&row.project_id, &row.session_id, &row.finish_event_id)?;
            if !bounded_identifier(&row.native_action_id) {
                return Err(StoreError::InvalidBatch);
            }
            let _ = format_timestamp(row.observed_at)?;
        }
        for row in &self.finishes {
            validate_graph_identity(&row.project_id, &row.session_id, &row.finish_event_id)?;
            if !bounded_identifier(&row.verification_id)
                || !bounded_identifier(&row.native_action_id)
                || !bounded_metadata(&row.basis)
            {
                return Err(StoreError::InvalidBatch);
            }
            let _ = format_timestamp(row.observed_at)?;
        }
        if graph_semantic_bytes(self)? > MAX_BATCH_BYTES {
            return Err(StoreError::InvalidBatch);
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct GraphApplyReceipt {
    pub through_event_seq: u64,
    pub replayed: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct WorkEdgeRow {
    pub from_work_id: WorkId,
    pub to_work_id: WorkId,
    pub kind: String,
}

#[derive(Clone, Serialize)]
pub struct WorkContractRow {
    pub contract_id: agbox_core::ContractId,
    pub revision: u64,
    pub contract_json: String,
    pub extractor_version: String,
    pub objective: Option<String>,
    pub summary: String,
    pub completed_steps: Vec<String>,
    pub next_actions: Vec<String>,
    pub blockers: Vec<String>,
    pub artifacts: Vec<String>,
    pub verification: Vec<String>,
}

/// The store deliberately deserializes the immutable contract independently
/// from the publication projections.  Keeping this DTO private prevents the
/// persistence API from becoming coupled to the pure workgraph crate while
/// still requiring the complete contract wire shape at the trust boundary.
#[derive(Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd)]
#[serde(rename_all = "snake_case")]
enum ContractFieldDto {
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

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ContractProjectionDto {
    contract_id: agbox_core::ContractId,
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
    evidence_refs: Vec<EventId>,
    field_evidence: BTreeMap<ContractFieldDto, Vec<EventId>>,
    evidence_truncated: bool,
    confidence_basis_points: u16,
    created_at: OffsetDateTime,
    extractor_version: String,
    #[serde(default)]
    fact_set_digest: String,
    material_content_hash: String,
    projection_state: serde_json::Value,
}

impl fmt::Debug for WorkContractRow {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("WorkContractRow")
            .field("contract_id", &self.contract_id)
            .field("revision", &self.revision)
            .field("contract_json_bytes", &self.contract_json.len())
            .field("extractor_version", &self.extractor_version)
            .field(
                "objective_bytes",
                &self.objective.as_ref().map_or(0, String::len),
            )
            .field("summary_bytes", &self.summary.len())
            .field("completed_steps", &self.completed_steps.len())
            .field("next_actions", &self.next_actions.len())
            .field("blockers", &self.blockers.len())
            .field("artifacts", &self.artifacts.len())
            .field("verification", &self.verification.len())
            .finish()
    }
}

#[derive(Clone, Serialize)]
pub struct WorkWriteBatch {
    pub visibility_name: String,
    pub expected_event_seq: u64,
    pub next_event_seq: u64,
    pub next_event_id: EventId,
    pub project_id: ProjectId,
    pub work_id: WorkId,
    pub status: String,
    pub observed_at: OffsetDateTime,
    pub evidence_event_ids: Vec<EventId>,
    pub artifact_ids: Vec<String>,
    pub edges: Vec<WorkEdgeRow>,
    pub contract: WorkContractRow,
}

/// Store-owned semantic extractor publication.  The optional contract is
/// inserted in the same transaction as the immutable extractor-run record;
/// failures therefore leave the current provisional revision untouched.
#[derive(Clone, Serialize)]
pub struct ExtractorWriteBatch {
    pub extractor_run_id: String,
    pub project_id: ProjectId,
    pub work_id: WorkId,
    pub extractor_version: String,
    pub input_event_watermark: String,
    pub status: String,
    pub bounded_error: Option<String>,
    pub observed_at: OffsetDateTime,
    pub refined_contract: Option<WorkContractRow>,
}

type ExistingExtractorRun = (
    String,
    String,
    String,
    String,
    String,
    Option<String>,
    String,
);

impl fmt::Debug for ExtractorWriteBatch {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ExtractorWriteBatch")
            .field("extractor_run_id", &self.extractor_run_id)
            .field("project_id", &self.project_id)
            .field("work_id", &self.work_id)
            .field("extractor_version", &self.extractor_version)
            .field(
                "input_event_watermark_bytes",
                &self.input_event_watermark.len(),
            )
            .field("status", &self.status)
            .field(
                "bounded_error_bytes",
                &self.bounded_error.as_ref().map_or(0, String::len),
            )
            .field("refined_contract", &self.refined_contract)
            .finish_non_exhaustive()
    }
}

impl ExtractorWriteBatch {
    /// Revalidates the bounded extractor-run and optional contract payload.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::InvalidBatch`] for invalid identifiers, status,
    /// payload bounds, or a contract whose immutable projections disagree.
    pub fn validate(&self) -> Result<(), StoreError> {
        if !bounded_identifier(&self.extractor_run_id)
            || !bounded_identifier(self.project_id.as_str())
            || !bounded_identifier(self.work_id.as_str())
            || !bounded_metadata(&self.extractor_version)
            || !bounded_metadata(&self.input_event_watermark)
            || !matches!(self.status.as_str(), "succeeded" | "failed")
            || self
                .bounded_error
                .as_ref()
                .is_some_and(|error| error.len() > agbox_core::limits::MAX_PREVIEW_BYTES)
            || (self.status == "failed") != self.bounded_error.is_some()
            || (self.status == "failed" && self.refined_contract.is_some())
            || (self.status == "succeeded" && self.refined_contract.is_none())
        {
            return Err(StoreError::InvalidBatch);
        }
        let _ = format_timestamp(self.observed_at)?;
        if let Some(contract) = &self.refined_contract {
            if !bounded_identifier(contract.contract_id.as_str())
                || contract.revision == 0
                || !bounded_metadata(&contract.extractor_version)
                || contract.contract_json.len() > agbox_core::limits::MAX_CONTRACT_SERIALIZED_BYTES
            {
                return Err(StoreError::InvalidBatch);
            }
            validate_extractor_contract(self, contract)?;
        }
        if serde_json::to_vec(self)?.len() > MAX_BATCH_BYTES {
            return Err(StoreError::InvalidBatch);
        }
        Ok(())
    }
}

fn validate_extractor_contract(
    batch: &ExtractorWriteBatch,
    row: &WorkContractRow,
) -> Result<(), StoreError> {
    let contract: ContractProjectionDto = serde_json::from_str(&row.contract_json)?;
    if contract.contract_id != row.contract_id
        || contract.work_id != batch.work_id
        || contract.revision != row.revision
        || contract.project_id != batch.project_id
        || contract.extractor_version != row.extractor_version
        || contract.created_at != batch.observed_at
        || contract.objective != row.objective
        || contract.summary != row.summary
        || contract.completed_steps != row.completed_steps
        || contract.next_actions != row.next_actions
        || contract.blockers != row.blockers
        || contract.artifacts != row.artifacts
        || contract.verification != row.verification
        || contract.confidence_basis_points > 10_000
        || !contract.projection_state.is_object()
        || !bounded_metadata(&contract.material_content_hash)
    {
        return Err(StoreError::InvalidBatch);
    }
    if contract.evidence_refs.is_empty()
        || contract.evidence_refs.len() > agbox_core::limits::MAX_CONTRACT_EVIDENCE_REFS
        || contract.field_evidence.len() > 10
        || !bounded_contract_field_list(&contract.completed_steps)
        || !bounded_contract_field_list(&contract.next_actions)
        || !bounded_contract_field_list(&contract.blockers)
        || !bounded_contract_field_list(&contract.constraints)
        || !bounded_contract_field_list(&contract.completion_criteria)
        || !bounded_contract_field_list(&contract.artifacts)
        || !bounded_contract_field_list(&contract.verification)
    {
        return Err(StoreError::InvalidBatch);
    }
    Ok(())
}

impl fmt::Debug for WorkWriteBatch {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("WorkWriteBatch")
            .field("visibility_name", &self.visibility_name)
            .field("expected_event_seq", &self.expected_event_seq)
            .field("next_event_seq", &self.next_event_seq)
            .field("project_id", &self.project_id)
            .field("work_id", &self.work_id)
            .field("status", &self.status)
            .field("evidence_event_ids", &self.evidence_event_ids.len())
            .field("artifact_ids", &self.artifact_ids.len())
            .field("edges", &self.edges.len())
            .field("contract", &self.contract)
            .finish_non_exhaustive()
    }
}

impl WorkWriteBatch {
    /// Revalidates the bounded store-owned publication DTO.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::InvalidBatch`] for invalid identifiers, bounds,
    /// watermark state, status values, or serialized contract content.
    pub fn validate(&self) -> Result<(), StoreError> {
        if !bounded_identifier(&self.visibility_name)
            || !bounded_identifier(self.project_id.as_str())
            || !bounded_identifier(self.work_id.as_str())
            || !bounded_identifier(self.next_event_id.as_str())
            || self.next_event_seq == 0
            || self.next_event_seq < self.expected_event_seq
            || self.next_event_seq > i64::MAX as u64
            || self.expected_event_seq > i64::MAX as u64
            || !matches!(
                self.status.as_str(),
                "observed" | "active" | "blocked" | "completed" | "abandoned"
            )
            || self.evidence_event_ids.len() > MAX_BATCH_RECORDS
            || self.artifact_ids.len() > MAX_BATCH_RECORDS
            || self.edges.len() > MAX_BATCH_RECORDS
            || self.contract.revision == 0
            || self.contract.revision > i64::MAX as u64
            || !bounded_identifier(self.contract.contract_id.as_str())
            || !bounded_metadata(&self.contract.extractor_version)
            || self.contract.contract_json.len() > agbox_core::limits::MAX_CONTRACT_SERIALIZED_BYTES
        {
            return Err(StoreError::InvalidBatch);
        }
        let _ = format_timestamp(self.observed_at)?;
        if self
            .evidence_event_ids
            .iter()
            .any(|event_id| !bounded_identifier(event_id.as_str()))
            || self
                .artifact_ids
                .iter()
                .any(|artifact_id| !bounded_identifier(artifact_id))
            || self.edges.iter().any(|edge| {
                !bounded_identifier(edge.from_work_id.as_str())
                    || !bounded_identifier(edge.to_work_id.as_str())
                    || !matches!(
                        edge.kind.as_str(),
                        "continues"
                            | "depends_on"
                            | "blocked_by"
                            | "produces"
                            | "validated_by"
                            | "supersedes"
                    )
                    || edge.from_work_id == edge.to_work_id
            })
        {
            return Err(StoreError::InvalidBatch);
        }
        for value in self
            .contract
            .objective
            .iter()
            .chain(std::iter::once(&self.contract.summary))
            .chain(self.contract.completed_steps.iter())
            .chain(self.contract.next_actions.iter())
            .chain(self.contract.blockers.iter())
            .chain(self.contract.artifacts.iter())
            .chain(self.contract.verification.iter())
        {
            if value.len() > agbox_core::limits::MAX_INLINE_BYTES {
                return Err(StoreError::InvalidBatch);
            }
        }
        validate_contract_projection(self)?;
        if serde_json::to_vec(self)?.len() > MAX_BATCH_BYTES {
            return Err(StoreError::InvalidBatch);
        }
        Ok(())
    }
}

fn validate_contract_projection(batch: &WorkWriteBatch) -> Result<(), StoreError> {
    let contract: ContractProjectionDto = serde_json::from_str(&batch.contract.contract_json)?;
    if contract.contract_id != batch.contract.contract_id
        || contract.work_id != batch.work_id
        || contract.revision != batch.contract.revision
        || contract.project_id != batch.project_id
        || contract.extractor_version != batch.contract.extractor_version
        || contract.created_at != batch.observed_at
        || work_status_name(contract.status) != batch.status
        || contract.objective != batch.contract.objective
        || contract.summary != batch.contract.summary
        || contract.completed_steps != batch.contract.completed_steps
        || contract.next_actions != batch.contract.next_actions
        || contract.blockers != batch.contract.blockers
        || contract.artifacts != batch.contract.artifacts
        || contract.verification != batch.contract.verification
        || !bounded_metadata(&contract.material_content_hash)
    {
        return Err(StoreError::InvalidBatch);
    }
    if contract.confidence_basis_points > 10_000
        || contract.evidence_refs.len() > agbox_core::limits::MAX_CONTRACT_EVIDENCE_REFS
        || contract
            .evidence_refs
            .iter()
            .any(|event_id| !bounded_identifier(event_id.as_str()))
        || contract.field_evidence.len() > 10
        || contract.field_evidence.values().any(|references| {
            references.len() > agbox_core::limits::MAX_CONTRACT_EVIDENCE_REFS
                || references
                    .iter()
                    .any(|event_id| !bounded_identifier(event_id.as_str()))
        })
        || !bounded_contract_field_list(&contract.completed_steps)
        || !bounded_contract_field_list(&contract.next_actions)
        || !bounded_contract_field_list(&contract.blockers)
        || !bounded_contract_field_list(&contract.constraints)
        || !bounded_contract_field_list(&contract.completion_criteria)
        || !bounded_contract_field_list(&contract.artifacts)
        || !bounded_contract_field_list(&contract.verification)
        || !contract.projection_state.is_object()
        || (!contract.fact_set_digest.is_empty() && !bounded_metadata(&contract.fact_set_digest))
        || (contract.evidence_truncated
            && contract.evidence_refs.len() != agbox_core::limits::MAX_CONTRACT_EVIDENCE_REFS)
    {
        return Err(StoreError::InvalidBatch);
    }
    Ok(())
}

fn bounded_contract_field_list(values: &[String]) -> bool {
    values.len() <= agbox_core::limits::MAX_CONTRACT_ITEMS_PER_FIELD
        && values
            .iter()
            .all(|value| value.len() <= agbox_core::limits::MAX_INLINE_BYTES)
}

fn work_status_name(status: WorkStatus) -> &'static str {
    match status {
        WorkStatus::Observed => "observed",
        WorkStatus::Active => "active",
        WorkStatus::Blocked => "blocked",
        WorkStatus::Completed => "completed",
        WorkStatus::Abandoned => "abandoned",
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WorkApplyReceipt {
    pub through_event_seq: u64,
    pub replayed: bool,
    pub revision_inserted: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ExtractorApplyReceipt {
    pub replayed: bool,
    pub revision_inserted: bool,
}

#[derive(Clone, Debug)]
pub struct WorkCandidateQuery {
    pub project_id: ProjectId,
    pub explicit_work_id: Option<WorkId>,
    pub continuation_work_id: Option<WorkId>,
    pub artifact_hashes: Vec<String>,
    pub command_hashes: Vec<String>,
    pub observed_at: OffsetDateTime,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StoredWorkCandidate {
    pub work_id: WorkId,
    pub project_id: ProjectId,
    pub provider: Option<String>,
    pub repository_hash: Option<String>,
    pub branch_hash: Option<String>,
    pub artifact_hashes: Vec<String>,
    pub command_hashes: Vec<String>,
    pub minutes_since_activity: u32,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorkCandidatePage {
    pub candidates: Vec<StoredWorkCandidate>,
    pub truncated: bool,
}

#[derive(Clone)]
pub struct SourceRegistration {
    pub project_id: ProjectId,
    pub repository_identity: String,
    pub project_root: Zeroizing<Vec<u8>>,
    pub source_id: String,
    pub provider: Provider,
    pub root_class: String,
    pub source_path: Zeroizing<Vec<u8>>,
    pub file_identity: String,
    pub generation: u64,
    pub size_bytes: u64,
    pub mtime: OffsetDateTime,
    pub session_time: Option<OffsetDateTime>,
    pub initial_cursor: u64,
}

impl fmt::Debug for SourceRegistration {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SourceRegistration")
            .field("project_id", &self.project_id)
            .field("source_id_bytes", &self.source_id.len())
            .field("provider", &self.provider)
            .field("generation", &self.generation)
            .field("size_bytes", &self.size_bytes)
            .field("initial_cursor", &self.initial_cursor)
            .finish_non_exhaustive()
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SourceRegistrationReceipt {
    pub source_id: String,
    pub generation: u64,
    pub initial_cursor: u64,
}

#[derive(Clone, Eq, PartialEq)]
pub struct CursorState {
    pub source_id: String,
    pub generation: u64,
    pub offset: u64,
    pub parser_state: Vec<u8>,
}

impl fmt::Debug for CursorState {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CursorState")
            .field("source_id", &self.source_id)
            .field("generation", &self.generation)
            .field("offset", &self.offset)
            .field("parser_state_bytes", &self.parser_state.len())
            .finish()
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EvidenceLink {
    pub event_id: String,
    pub observation_id: String,
    pub evidence_id: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum EvidenceOwner {
    Event(EventId),
    Work(WorkId),
}

#[derive(Clone)]
pub struct EvidenceWrite {
    pub evidence_id: EvidenceId,
    pub project_id: ProjectId,
    pub owner: EvidenceOwner,
    pub content_hash: String,
    pub media_type: String,
    pub privacy: PrivacyLabel,
    pub disclosure_class: DisclosureClass,
    pub redacted_excerpt: String,
    pub expires_at: Option<OffsetDateTime>,
    pub plaintext: Zeroizing<Vec<u8>>,
}

impl fmt::Debug for EvidenceWrite {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("EvidenceWrite")
            .field("evidence_id", &self.evidence_id)
            .field("project_id", &self.project_id)
            .field("owner", &self.owner)
            .field("privacy", &self.privacy)
            .field("disclosure_class", &self.disclosure_class)
            .field("redacted_excerpt_bytes", &self.redacted_excerpt.len())
            .field("plaintext_bytes", &self.plaintext.len())
            .finish_non_exhaustive()
    }
}

#[derive(Clone, Debug)]
pub struct ContentRefWrite {
    pub content_ref_id: String,
    pub project_id: ProjectId,
    pub content: ContentRef,
    pub privacy: PrivacyLabel,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SchemaFingerprintUpdate {
    pub provider: String,
    pub format: String,
    pub fingerprint: String,
    pub observed_at: OffsetDateTime,
}

#[derive(Clone, Eq, PartialEq)]
pub struct IngestionFault {
    pub fault_id: String,
    pub source_id: String,
    pub generation: u64,
    pub byte_start: u64,
    pub byte_end: u64,
    pub class: String,
    pub bounded_detail: String,
}

impl fmt::Debug for IngestionFault {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("IngestionFault")
            .field("fault_id", &self.fault_id)
            .field("source_id", &self.source_id)
            .field("generation", &self.generation)
            .field("byte_start", &self.byte_start)
            .field("byte_end", &self.byte_end)
            .field("class", &self.class)
            .field("bounded_detail_bytes", &self.bounded_detail.len())
            .finish()
    }
}

#[derive(Clone)]
pub struct IngestionChunk {
    pub expected_cursor: CursorState,
    pub next_cursor: CursorState,
    pub observations: Vec<SourceObservation>,
    pub events: Vec<ActivityEventV1>,
    pub evidence: Vec<EvidenceWrite>,
    pub evidence_links: Vec<EvidenceLink>,
    pub content_refs: Vec<ContentRefWrite>,
    pub fingerprints: Vec<SchemaFingerprintUpdate>,
    pub faults: Vec<IngestionFault>,
}

impl fmt::Debug for IngestionChunk {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("IngestionChunk")
            .field("source_id", &self.expected_cursor.source_id)
            .field("generation", &self.expected_cursor.generation)
            .field("expected_offset", &self.expected_cursor.offset)
            .field("next_offset", &self.next_cursor.offset)
            .field(
                "expected_parser_state_bytes",
                &self.expected_cursor.parser_state.len(),
            )
            .field(
                "next_parser_state_bytes",
                &self.next_cursor.parser_state.len(),
            )
            .field("observations", &self.observations.len())
            .field("events", &self.events.len())
            .field("evidence", &self.evidence.len())
            .field("evidence_links", &self.evidence_links.len())
            .field("content_refs", &self.content_refs.len())
            .field("fingerprints", &self.fingerprints.len())
            .field("faults", &self.faults.len())
            .finish()
    }
}

impl IngestionChunk {
    /// Revalidates all batch bounds and immutable normalized values.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::InvalidBatch`] when a cardinality, byte, cursor,
    /// identity, or normalized-value invariant is violated.
    #[allow(clippy::too_many_lines)]
    pub fn validate(&self) -> Result<(), StoreError> {
        let event_capacity = self
            .observations
            .len()
            .checked_mul(agbox_core::limits::MAX_EVENTS_PER_RECORD)
            .ok_or(StoreError::InvalidBatch)?;
        let evidence_capacity = self
            .observations
            .len()
            .checked_mul(agbox_core::limits::MAX_EVIDENCE_PER_RECORD)
            .ok_or(StoreError::InvalidBatch)?;
        let content_per_record = agbox_core::limits::MAX_EVENTS_PER_RECORD
            .checked_add(agbox_core::limits::MAX_EVIDENCE_PER_RECORD)
            .and_then(|value| value.checked_add(1))
            .ok_or(StoreError::InvalidBatch)?;
        let content_ref_capacity = self
            .observations
            .len()
            .checked_mul(content_per_record)
            .ok_or(StoreError::InvalidBatch)?;

        if self.observations.len() > MAX_BATCH_RECORDS
            || self.events.len() > event_capacity
            || self.evidence.len() > evidence_capacity
            || self.evidence_links.len() > evidence_capacity
            || self.content_refs.len() > content_ref_capacity
            || self.fingerprints.len() > self.observations.len()
            || self.faults.len() > self.observations.len()
            || self.expected_cursor.parser_state.len() > agbox_core::limits::MAX_DECODER_STATE_BYTES
            || self.next_cursor.parser_state.len() > agbox_core::limits::MAX_DECODER_STATE_BYTES
            || self.expected_cursor.source_id != self.next_cursor.source_id
            || self.expected_cursor.generation != self.next_cursor.generation
            || self.expected_cursor.generation == 0
            || self.next_cursor.offset < self.expected_cursor.offset
            || !bounded_identifier(&self.expected_cursor.source_id)
            || self.expected_cursor.offset > i64::MAX as u64
            || self.next_cursor.offset > i64::MAX as u64
            || self.expected_cursor.generation > i64::MAX as u64
        {
            return Err(StoreError::InvalidBatch);
        }

        for observation in &self.observations {
            observation
                .validate()
                .map_err(|_| StoreError::InvalidBatch)?;
            if observation.source().source_generation() != self.expected_cursor.generation
                || observation.range().end() < observation.range().start()
                || observation.range().end() > i64::MAX as u64
            {
                return Err(StoreError::InvalidBatch);
            }
        }
        for event in &self.events {
            event.validate().map_err(|_| StoreError::InvalidBatch)?;
            if event.source().source_generation() != self.expected_cursor.generation {
                return Err(StoreError::InvalidBatch);
            }
        }
        for item in &self.evidence {
            if item.plaintext.len() > agbox_core::limits::MAX_INLINE_BYTES
                || item.redacted_excerpt.len() > agbox_core::limits::MAX_PREVIEW_BYTES
                || !bounded_identifier(item.evidence_id.as_str())
                || !bounded_identifier(item.project_id.as_str())
                || !bounded_metadata(&item.content_hash)
                || !bounded_metadata(&item.media_type)
            {
                return Err(StoreError::InvalidBatch);
            }
        }
        for item in &self.evidence_links {
            if !bounded_identifier(&item.event_id)
                || !bounded_identifier(&item.observation_id)
                || !bounded_identifier(&item.evidence_id)
            {
                return Err(StoreError::InvalidBatch);
            }
        }
        for item in &self.content_refs {
            item.content
                .validate()
                .map_err(|_| StoreError::InvalidBatch)?;
            if item.content_ref_id != stable_content_ref_id(&item.project_id, &item.content)?
                || !bounded_identifier(&item.content_ref_id)
            {
                return Err(StoreError::InvalidContentRefId);
            }
        }
        for item in &self.fingerprints {
            if !bounded_metadata(&item.provider)
                || !bounded_metadata(&item.format)
                || !bounded_metadata(&item.fingerprint)
            {
                return Err(StoreError::InvalidBatch);
            }
        }
        for fault in &self.faults {
            if fault.source_id != self.expected_cursor.source_id
                || fault.generation != self.expected_cursor.generation
                || fault.byte_end < fault.byte_start
                || fault.byte_end > i64::MAX as u64
                || !bounded_identifier(&fault.fault_id)
                || !bounded_identifier(&fault.source_id)
                || !bounded_metadata(&fault.class)
                || fault.bounded_detail.len() > agbox_core::limits::MAX_PREVIEW_BYTES
            {
                return Err(StoreError::InvalidBatch);
            }
        }
        if self.measured_semantic_bytes()? > MAX_BATCH_BYTES {
            return Err(StoreError::InvalidBatch);
        }
        Ok(())
    }

    /// Measures all retained semantic data with checked arithmetic.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::InvalidBatch`] on arithmetic overflow or a
    /// serialization failure.
    pub fn measured_semantic_bytes(&self) -> Result<usize, StoreError> {
        let mut total = 0_usize;
        add_len(&mut total, self.expected_cursor.source_id.len())?;
        add_len(&mut total, self.expected_cursor.parser_state.len())?;
        add_len(&mut total, self.next_cursor.source_id.len())?;
        add_len(&mut total, self.next_cursor.parser_state.len())?;
        add_len(
            &mut total,
            size_of::<u64>()
                .checked_mul(4)
                .ok_or(StoreError::InvalidBatch)?,
        )?;

        for observation in &self.observations {
            add_len(&mut total, serde_json::to_vec(observation)?.len())?;
        }
        for event in &self.events {
            add_len(&mut total, serde_json::to_vec(event)?.len())?;
        }
        for item in &self.evidence {
            add_len(&mut total, item.evidence_id.as_str().len())?;
            add_len(&mut total, item.project_id.as_str().len())?;
            add_len(&mut total, owner_kind(&item.owner).len())?;
            add_len(&mut total, owner_id(&item.owner).len())?;
            add_len(&mut total, item.content_hash.len())?;
            add_len(&mut total, item.media_type.len())?;
            add_len(&mut total, privacy(item.privacy).len())?;
            add_len(&mut total, disclosure(item.disclosure_class).len())?;
            add_len(&mut total, item.redacted_excerpt.len())?;
            add_len(&mut total, 1)?;
            if let Some(expires_at) = item.expires_at {
                add_len(&mut total, format_timestamp(expires_at)?.len())?;
            }
            add_len(&mut total, item.plaintext.len())?;
        }
        for item in &self.evidence_links {
            add_len(&mut total, item.event_id.len())?;
            add_len(&mut total, item.observation_id.len())?;
            add_len(&mut total, item.evidence_id.len())?;
        }
        for item in &self.content_refs {
            add_len(&mut total, item.content_ref_id.len())?;
            add_len(&mut total, item.project_id.as_str().len())?;
            add_len(&mut total, serde_json::to_vec(&item.content)?.len())?;
            add_len(&mut total, privacy(item.privacy).len())?;
        }
        for item in &self.fingerprints {
            add_len(&mut total, item.provider.len())?;
            add_len(&mut total, item.format.len())?;
            add_len(&mut total, item.fingerprint.len())?;
            add_len(&mut total, format_timestamp(item.observed_at)?.len())?;
        }
        for fault in &self.faults {
            add_len(&mut total, fault.fault_id.len())?;
            add_len(&mut total, fault.source_id.len())?;
            add_len(&mut total, fault.class.len())?;
            add_len(&mut total, fault.bounded_detail.len())?;
            add_len(
                &mut total,
                size_of::<u64>()
                    .checked_mul(3)
                    .ok_or(StoreError::InvalidBatch)?,
            )?;
        }
        Ok(total)
    }

    fn commit_digest(&self) -> Result<String, StoreError> {
        let mut manifest = ManifestHasher::new();
        manifest.bytes(
            "expected.source_id",
            self.expected_cursor.source_id.as_bytes(),
        )?;
        manifest.u64("expected.generation", self.expected_cursor.generation)?;
        manifest.u64("expected.offset", self.expected_cursor.offset)?;
        manifest.bytes("expected.parser_state", &self.expected_cursor.parser_state)?;
        manifest.bytes("next.source_id", self.next_cursor.source_id.as_bytes())?;
        manifest.u64("next.generation", self.next_cursor.generation)?;
        manifest.u64("next.offset", self.next_cursor.offset)?;
        manifest.bytes("next.parser_state", &self.next_cursor.parser_state)?;

        manifest.vector_len("observations", self.observations.len())?;
        for item in &self.observations {
            manifest.bytes("observation", &serde_json::to_vec(item)?)?;
        }
        manifest.vector_len("events", self.events.len())?;
        for item in &self.events {
            manifest.bytes("event", &serde_json::to_vec(item)?)?;
        }
        manifest.vector_len("evidence", self.evidence.len())?;
        for item in &self.evidence {
            manifest.bytes("evidence.id", item.evidence_id.as_str().as_bytes())?;
            manifest.bytes("evidence.project", item.project_id.as_str().as_bytes())?;
            manifest.bytes("evidence.owner_kind", owner_kind(&item.owner).as_bytes())?;
            manifest.bytes("evidence.owner_id", owner_id(&item.owner).as_bytes())?;
            manifest.bytes("evidence.content_hash", item.content_hash.as_bytes())?;
            manifest.bytes("evidence.media_type", item.media_type.as_bytes())?;
            manifest.bytes("evidence.privacy", privacy(item.privacy).as_bytes())?;
            manifest.bytes(
                "evidence.disclosure",
                disclosure(item.disclosure_class).as_bytes(),
            )?;
            manifest.bytes(
                "evidence.redacted_excerpt",
                item.redacted_excerpt.as_bytes(),
            )?;
            manifest.u64(
                "evidence.byte_length",
                u64::try_from(item.plaintext.len()).map_err(|_| StoreError::InvalidBatch)?,
            )?;
            match item.expires_at {
                Some(value) => {
                    manifest.u64("evidence.expires.present", 1)?;
                    manifest.bytes(
                        "evidence.expires.value",
                        format_timestamp(value)?.as_bytes(),
                    )?;
                }
                None => manifest.u64("evidence.expires.present", 0)?,
            }
        }
        manifest.vector_len("evidence_links", self.evidence_links.len())?;
        for item in &self.evidence_links {
            manifest.bytes("link.event", item.event_id.as_bytes())?;
            manifest.bytes("link.observation", item.observation_id.as_bytes())?;
            manifest.bytes("link.evidence", item.evidence_id.as_bytes())?;
        }
        manifest.vector_len("content_refs", self.content_refs.len())?;
        for item in &self.content_refs {
            manifest.bytes("content.id", item.content_ref_id.as_bytes())?;
            manifest.bytes("content.project", item.project_id.as_str().as_bytes())?;
            manifest.bytes("content.value", &serde_json::to_vec(&item.content)?)?;
            manifest.bytes("content.privacy", privacy(item.privacy).as_bytes())?;
        }
        manifest.vector_len("fingerprints", self.fingerprints.len())?;
        for item in &self.fingerprints {
            manifest.bytes("fingerprint.provider", item.provider.as_bytes())?;
            manifest.bytes("fingerprint.format", item.format.as_bytes())?;
            manifest.bytes("fingerprint.value", item.fingerprint.as_bytes())?;
            manifest.bytes(
                "fingerprint.observed_at",
                format_timestamp(item.observed_at)?.as_bytes(),
            )?;
        }
        manifest.vector_len("faults", self.faults.len())?;
        for item in &self.faults {
            manifest.bytes("fault.id", item.fault_id.as_bytes())?;
            manifest.bytes("fault.source", item.source_id.as_bytes())?;
            manifest.u64("fault.generation", item.generation)?;
            manifest.u64("fault.byte_start", item.byte_start)?;
            manifest.u64("fault.byte_end", item.byte_end)?;
            manifest.bytes("fault.class", item.class.as_bytes())?;
            manifest.bytes("fault.detail", item.bounded_detail.as_bytes())?;
        }
        Ok(manifest.finish())
    }
}

struct ManifestHasher {
    hasher: blake3::Hasher,
}

impl ManifestHasher {
    fn new() -> Self {
        let mut hasher = blake3::Hasher::new();
        hasher.update(b"agbox.ingestion.commit.v1");
        Self { hasher }
    }

    fn bytes(&mut self, label: &str, value: &[u8]) -> Result<(), StoreError> {
        hash_part(&mut self.hasher, label.as_bytes())?;
        hash_part(&mut self.hasher, value)
    }

    fn u64(&mut self, label: &str, value: u64) -> Result<(), StoreError> {
        self.bytes(label, &value.to_le_bytes())
    }

    fn vector_len(&mut self, label: &str, value: usize) -> Result<(), StoreError> {
        self.u64(
            label,
            u64::try_from(value).map_err(|_| StoreError::InvalidBatch)?,
        )
    }

    fn finish(self) -> String {
        self.hasher.finalize().to_hex().to_string()
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CommitReceipt {
    pub source_id: String,
    pub generation: u64,
    pub cursor_offset: u64,
    pub inserted_events: usize,
}

pub(crate) enum WriteCommand {
    RegisterSource {
        registration: Box<SourceRegistration>,
        reply: oneshot::Sender<Result<SourceRegistrationReceipt, StoreError>>,
    },
    Commit {
        chunk: Box<IngestionChunk>,
        reply: oneshot::Sender<Result<CommitReceipt, StoreError>>,
    },
    ApplyGraph {
        batch: Box<GraphWriteBatch>,
        reply: oneshot::Sender<Result<GraphApplyReceipt, StoreError>>,
    },
    ApplyWork {
        batch: Box<WorkWriteBatch>,
        reply: oneshot::Sender<Result<WorkApplyReceipt, StoreError>>,
    },
    ApplyExtractor {
        batch: Box<ExtractorWriteBatch>,
        reply: oneshot::Sender<Result<ExtractorApplyReceipt, StoreError>>,
    },
    LoadWorkCandidates {
        query: Box<WorkCandidateQuery>,
        reply: oneshot::Sender<Result<WorkCandidatePage, StoreError>>,
    },
    LoadLatestWorkContract {
        project_id: ProjectId,
        work_id: WorkId,
        reply: oneshot::Sender<Result<Option<String>, StoreError>>,
    },
    Shutdown {
        reply: oneshot::Sender<()>,
    },
    #[cfg(feature = "test-support")]
    TestBarrier {
        entered: oneshot::Sender<()>,
        release: std::sync::mpsc::Receiver<()>,
    },
}

#[derive(Clone)]
pub struct WriterHandle {
    pub(crate) sender: mpsc::Sender<WriteCommand>,
}

impl fmt::Debug for WriterHandle {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("WriterHandle")
            .finish_non_exhaustive()
    }
}

/// One submitted ingestion command awaiting its sole-writer receipt.
pub struct CommitSubmission {
    receive: oneshot::Receiver<Result<CommitReceipt, StoreError>>,
}

impl fmt::Debug for CommitSubmission {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CommitSubmission")
            .finish_non_exhaustive()
    }
}

impl CommitSubmission {
    /// Awaits the sole writer's atomic commit result.
    ///
    /// # Errors
    ///
    /// Returns the writer's validation/database result or `WriterStopped`.
    pub async fn receive(self) -> Result<CommitReceipt, StoreError> {
        self.receive.await.map_err(|_| StoreError::WriterStopped)?
    }
}

impl WriterHandle {
    /// Atomically registers one project, source, generation, and initial cursor.
    ///
    /// # Errors
    ///
    /// Returns an error for invalid, conflicting, regressing, partially
    /// associated, unencryptable, or unpersistable registration state.
    pub async fn register_source(
        &self,
        registration: SourceRegistration,
    ) -> Result<SourceRegistrationReceipt, StoreError> {
        validate_registration(&registration)?;
        let (reply, receive) = oneshot::channel();
        self.sender
            .send(WriteCommand::RegisterSource {
                registration: Box::new(registration),
                reply,
            })
            .await
            .map_err(|_| StoreError::WriterStopped)?;
        receive.await.map_err(|_| StoreError::WriterStopped)?
    }

    /// Atomically commits a validated bounded ingestion chunk.
    ///
    /// # Errors
    ///
    /// Returns a validation, cursor, immutable-row, evidence, writer, or
    /// database error without advancing the cursor.
    pub async fn commit_ingestion(
        &self,
        chunk: IngestionChunk,
    ) -> Result<CommitReceipt, StoreError> {
        self.submit_ingestion(chunk).await?.receive().await
    }

    /// Validates and submits one chunk without awaiting its writer receipt.
    ///
    /// # Errors
    ///
    /// Returns validation or writer-channel failure before submission.
    pub async fn submit_ingestion(
        &self,
        chunk: IngestionChunk,
    ) -> Result<CommitSubmission, StoreError> {
        chunk.validate()?;
        let (reply, receive) = oneshot::channel();
        self.sender
            .send(WriteCommand::Commit {
                chunk: Box::new(chunk),
                reply,
            })
            .await
            .map_err(|_| StoreError::WriterStopped)?;
        Ok(CommitSubmission { receive })
    }

    /// Atomically applies one store-owned graph batch and advances its reducer
    /// watermark in the same immediate transaction.
    ///
    /// # Errors
    ///
    /// Returns validation, immutable-row, watermark, writer, or database
    /// errors without partially applying the graph batch.
    pub async fn apply_graph(
        &self,
        batch: GraphWriteBatch,
    ) -> Result<GraphApplyReceipt, StoreError> {
        batch.validate()?;
        let (reply, receive) = oneshot::channel();
        self.sender
            .send(WriteCommand::ApplyGraph {
                batch: Box::new(batch),
                reply,
            })
            .await
            .map_err(|_| StoreError::WriterStopped)?;
        receive.await.map_err(|_| StoreError::WriterStopped)?
    }

    /// Publishes one provisional work revision and advances its visibility
    /// watermark in the same immediate transaction.
    ///
    /// # Errors
    ///
    /// Returns validation, project-scoping, immutable-row, watermark, writer,
    /// or database errors without partially publishing work.
    pub async fn apply_work(&self, batch: WorkWriteBatch) -> Result<WorkApplyReceipt, StoreError> {
        batch.validate()?;
        let (reply, receive) = oneshot::channel();
        self.sender
            .send(WriteCommand::ApplyWork {
                batch: Box::new(batch),
                reply,
            })
            .await
            .map_err(|_| StoreError::WriterStopped)?;
        receive.await.map_err(|_| StoreError::WriterStopped)?
    }

    /// Atomically records one semantic extractor run and, on success, its
    /// immutable refined revision. Failed runs never replace the provisional
    /// contract.
    ///
    /// # Errors
    ///
    /// Returns a store error when the batch is invalid, the work/project
    /// reference is missing, or an immutable run/revision conflicts.
    pub async fn apply_extractor(
        &self,
        batch: ExtractorWriteBatch,
    ) -> Result<ExtractorApplyReceipt, StoreError> {
        batch.validate()?;
        let (reply, receive) = oneshot::channel();
        self.sender
            .send(WriteCommand::ApplyExtractor {
                batch: Box::new(batch),
                reply,
            })
            .await
            .map_err(|_| StoreError::WriterStopped)?;
        receive.await.map_err(|_| StoreError::WriterStopped)?
    }

    /// Loads at most 64 same-project correlation candidates in explicit,
    /// continuation, artifact, command, and recent priority order.
    ///
    /// # Errors
    ///
    /// Returns validation, writer, or database errors. Truncation is reported
    /// explicitly in the returned page.
    pub async fn load_work_candidates(
        &self,
        query: WorkCandidateQuery,
    ) -> Result<WorkCandidatePage, StoreError> {
        validate_work_candidate_query(&query)?;
        let (reply, receive) = oneshot::channel();
        self.sender
            .send(WriteCommand::LoadWorkCandidates {
                query: Box::new(query),
                reply,
            })
            .await
            .map_err(|_| StoreError::WriterStopped)?;
        receive.await.map_err(|_| StoreError::WriterStopped)?
    }

    /// Loads the latest immutable contract JSON for a same-project work item.
    ///
    /// # Errors
    ///
    /// Returns validation, writer, project-scope, or database errors.
    pub async fn latest_work_contract(
        &self,
        project_id: ProjectId,
        work_id: WorkId,
    ) -> Result<Option<String>, StoreError> {
        if !bounded_identifier(project_id.as_str()) || !bounded_identifier(work_id.as_str()) {
            return Err(StoreError::InvalidBatch);
        }
        let (reply, receive) = oneshot::channel();
        self.sender
            .send(WriteCommand::LoadLatestWorkContract {
                project_id,
                work_id,
                reply,
            })
            .await
            .map_err(|_| StoreError::WriterStopped)?;
        receive.await.map_err(|_| StoreError::WriterStopped)?
    }

    #[cfg(feature = "test-support")]
    #[doc(hidden)]
    #[must_use]
    pub fn available_capacity_for_test(&self) -> usize {
        self.sender.capacity()
    }

    #[cfg(feature = "test-support")]
    #[doc(hidden)]
    pub async fn pause_for_test(&self) -> Result<std::sync::mpsc::Sender<()>, StoreError> {
        let (entered, receive_entered) = oneshot::channel();
        let (release, receive_release) = std::sync::mpsc::channel();
        self.sender
            .send(WriteCommand::TestBarrier {
                entered,
                release: receive_release,
            })
            .await
            .map_err(|_| StoreError::WriterStopped)?;
        receive_entered
            .await
            .map_err(|_| StoreError::WriterStopped)?;
        Ok(release)
    }
}

#[allow(clippy::needless_pass_by_value)]
pub(crate) fn run_writer(
    mut connection: rusqlite::Connection,
    vault: Arc<EvidenceVault>,
    mut commands: mpsc::Receiver<WriteCommand>,
) {
    while let Some(command) = commands.blocking_recv() {
        match command {
            WriteCommand::RegisterSource {
                registration,
                reply,
            } => {
                let _ = reply.send(register_source(&mut connection, &vault, &registration));
            }
            WriteCommand::Commit { chunk, reply } => {
                let result = commit(&mut connection, &vault, &chunk);
                let _ = reply.send(result);
            }
            WriteCommand::ApplyGraph { batch, reply } => {
                let result = apply_graph(&mut connection, &vault, &batch);
                let _ = reply.send(result);
            }
            WriteCommand::ApplyWork { batch, reply } => {
                let result = apply_work(&mut connection, &batch);
                let _ = reply.send(result);
            }
            WriteCommand::ApplyExtractor { batch, reply } => {
                let result = apply_extractor(&mut connection, &batch);
                let _ = reply.send(result);
            }
            WriteCommand::LoadWorkCandidates { query, reply } => {
                let result = load_work_candidates(&connection, &query);
                let _ = reply.send(result);
            }
            WriteCommand::LoadLatestWorkContract {
                project_id,
                work_id,
                reply,
            } => {
                let result = latest_work_contract(&connection, &project_id, &work_id);
                let _ = reply.send(result);
            }
            WriteCommand::Shutdown { reply } => {
                let _ = reply.send(());
                break;
            }
            #[cfg(feature = "test-support")]
            WriteCommand::TestBarrier { entered, release } => {
                let _ = entered.send(());
                let _ = release.recv();
            }
        }
    }
}

fn validate_registration(registration: &SourceRegistration) -> Result<(), StoreError> {
    if !bounded_identifier(registration.project_id.as_str())
        || !valid_source_id(&registration.source_id)
        || !valid_repository_identity(&registration.repository_identity)
        || !valid_file_identity(&registration.file_identity)
        || !matches!(registration.root_class.as_str(), "active" | "archive")
        || registration.generation == 0
        || registration.generation > i64::MAX as u64
        || registration.size_bytes > i64::MAX as u64
        || registration.initial_cursor > i64::MAX as u64
        || (registration.initial_cursor != 0
            && registration.initial_cursor != registration.size_bytes)
        || registration.project_root.is_empty()
        || registration.source_path.is_empty()
        || registration.project_root.len() > 32 * 1024
        || registration.source_path.len() > 32 * 1024
    {
        return Err(StoreError::InvalidBatch);
    }
    let _ = format_timestamp(registration.mtime)?;
    if let Some(session_time) = registration.session_time {
        let _ = format_timestamp(session_time)?;
    }
    Ok(())
}

fn valid_source_id(value: &str) -> bool {
    value.len() == 39
        && value.strip_prefix("source_").is_some_and(|digest| {
            digest
                .bytes()
                .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
        })
}

fn valid_repository_identity(value: &str) -> bool {
    value
        .strip_prefix("repo-fs-v1:")
        .and_then(|suffix| suffix.split_once(':'))
        .is_some_and(|(device, inode)| canonical_u64(device) && canonical_u64(inode))
}

fn valid_file_identity(value: &str) -> bool {
    value
        .strip_prefix("unix:")
        .and_then(|suffix| suffix.split_once(':'))
        .is_some_and(|(device, inode)| canonical_u64(device) && canonical_u64(inode))
}

fn canonical_u64(value: &str) -> bool {
    !value.is_empty()
        && (value == "0" || !value.starts_with('0'))
        && value.bytes().all(|byte| byte.is_ascii_digit())
        && value.parse::<u64>().is_ok()
}

fn register_source(
    connection: &mut rusqlite::Connection,
    vault: &EvidenceVault,
    registration: &SourceRegistration,
) -> Result<SourceRegistrationReceipt, StoreError> {
    validate_registration(registration)?;
    let project_aad = registration_aad(
        b"agbox.db.project-root.v1",
        &[
            registration.project_id.as_str().as_bytes(),
            registration.repository_identity.as_bytes(),
        ],
    )?;
    let source_aad = registration_aad(
        b"agbox.db.source-path.v1",
        &[
            registration.project_id.as_str().as_bytes(),
            registration.source_id.as_bytes(),
            &registration.generation.to_le_bytes(),
        ],
    )?;
    // Both sensitive fields are sealed before SQLite sees any value from the
    // registration. Plaintext remains in its original Zeroizing allocation.
    let encrypted_root =
        vault.seal_database_field(&project_aad, registration.project_root.as_slice())?;
    let encrypted_source =
        vault.seal_database_field(&source_aad, registration.source_path.as_slice())?;

    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    validate_registration_associations(&transaction, registration)?;
    let mtime = format_timestamp(registration.mtime)?;
    let session_time = registration
        .session_time
        .map(format_timestamp)
        .transpose()?;

    insert_source_registration(
        &transaction,
        registration,
        &encrypted_root,
        &encrypted_source,
        &mtime,
        session_time.as_deref(),
    )?;
    transaction.commit()?;
    Ok(SourceRegistrationReceipt {
        source_id: registration.source_id.clone(),
        generation: registration.generation,
        initial_cursor: registration.initial_cursor,
    })
}

fn insert_source_registration(
    transaction: &Transaction<'_>,
    registration: &SourceRegistration,
    encrypted_root: &[u8],
    encrypted_source: &[u8],
    mtime: &str,
    session_time: Option<&str>,
) -> Result<(), StoreError> {
    transaction.execute(
        "INSERT INTO projects(
             project_id, repository_identity, encrypted_root_path, created_at, updated_at
         ) VALUES (?1, ?2, ?3,
             strftime('%Y-%m-%dT%H:%M:%fZ', 'now'),
             strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
         ON CONFLICT(project_id) DO UPDATE SET
             encrypted_root_path = excluded.encrypted_root_path,
             updated_at = excluded.updated_at",
        params![
            registration.project_id.as_str(),
            registration.repository_identity,
            encrypted_root,
        ],
    )?;
    transaction.execute(
        "INSERT INTO sources(
             source_id, project_id, provider, root_class, encrypted_path,
             file_identity, created_at, updated_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6,
             strftime('%Y-%m-%dT%H:%M:%fZ', 'now'),
             strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
         ON CONFLICT(source_id) DO UPDATE SET
             encrypted_path = excluded.encrypted_path,
             file_identity = excluded.file_identity,
             updated_at = excluded.updated_at",
        params![
            registration.source_id,
            registration.project_id.as_str(),
            registration.provider.as_str(),
            registration.root_class,
            encrypted_source,
            registration.file_identity,
        ],
    )?;
    transaction.execute(
        "INSERT INTO source_generations(
             source_id, generation, size_bytes, mtime, session_time,
             schema_fingerprint, status
         ) VALUES (?1, ?2, ?3, ?4, ?5, NULL, 'active')
         ON CONFLICT(source_id, generation) DO NOTHING",
        params![
            registration.source_id,
            to_i64(registration.generation)?,
            to_i64(registration.size_bytes)?,
            mtime,
            session_time,
        ],
    )?;
    transaction.execute(
        "INSERT INTO source_generation_identities(
             source_id, generation, file_identity
         ) VALUES (?1, ?2, ?3)
         ON CONFLICT(source_id, generation) DO NOTHING",
        params![
            registration.source_id,
            to_i64(registration.generation)?,
            registration.file_identity,
        ],
    )?;
    let registration_digest = registration_digest(registration)?;
    transaction.execute(
        "INSERT INTO source_cursors(
             source_id, generation, cursor_offset, parser_state,
             last_commit_digest, updated_at
         ) VALUES (?1, ?2, ?3, X'', ?4,
             strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
         ON CONFLICT(source_id, generation) DO NOTHING",
        params![
            registration.source_id,
            to_i64(registration.generation)?,
            to_i64(registration.initial_cursor)?,
            registration_digest,
        ],
    )?;
    Ok(())
}

#[allow(clippy::too_many_lines)]
fn validate_registration_associations(
    transaction: &Transaction<'_>,
    registration: &SourceRegistration,
) -> Result<(), StoreError> {
    let project: Option<String> = transaction
        .query_row(
            "SELECT repository_identity FROM projects WHERE project_id = ?1",
            [registration.project_id.as_str()],
            |row| row.get(0),
        )
        .optional()?;
    if project
        .as_deref()
        .is_some_and(|identity| identity != registration.repository_identity)
    {
        return Err(StoreError::ImmutableConflict);
    }
    let repository_owner: Option<String> = transaction
        .query_row(
            "SELECT project_id FROM projects WHERE repository_identity = ?1 LIMIT 1",
            [registration.repository_identity.as_str()],
            |row| row.get(0),
        )
        .optional()?;
    if repository_owner
        .as_deref()
        .is_some_and(|project_id| project_id != registration.project_id.as_str())
    {
        return Err(StoreError::ImmutableConflict);
    }

    let generation = to_i64(registration.generation)?;
    let maximum: Option<i64> = transaction.query_row(
        "SELECT max(generation) FROM source_generations WHERE source_id = ?1",
        [registration.source_id.as_str()],
        |row| row.get(0),
    )?;
    if maximum.is_some_and(|maximum| generation < maximum) {
        return Err(StoreError::ImmutableConflict);
    }

    let source: Option<(String, String, String, String)> = transaction
        .query_row(
            "SELECT project_id, provider, root_class, file_identity
             FROM sources WHERE source_id = ?1",
            [registration.source_id.as_str()],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .optional()?;
    let source_exists = source.is_some();
    if let Some((project, provider, root_class, file_identity)) = source {
        let is_next_replacement = maximum
            .and_then(|value| value.checked_add(1))
            .is_some_and(|next| next == generation);
        if project != registration.project_id.as_str()
            || provider != registration.provider.as_str()
            || root_class != registration.root_class
            || (file_identity != registration.file_identity && !is_next_replacement)
        {
            return Err(StoreError::ImmutableConflict);
        }
    }
    let file_owner: Option<String> = transaction
        .query_row(
            "SELECT source_id
             FROM source_generation_identities
             WHERE file_identity = ?1
             LIMIT 1",
            [registration.file_identity.as_str()],
            |row| row.get(0),
        )
        .optional()?;
    if file_owner
        .as_deref()
        .is_some_and(|source_id| source_id != registration.source_id)
    {
        return Err(StoreError::ImmutableConflict);
    }

    if source_exists && maximum.is_none() {
        return Err(StoreError::ImmutableConflict);
    }
    let existing: Option<(i64, String, Option<String>, i64, String)> = transaction
        .query_row(
            "SELECT source_generations.size_bytes, source_generations.mtime,
                    source_generations.session_time, source_cursors.cursor_offset,
                    source_generation_identities.file_identity
             FROM source_generations
             INNER JOIN source_cursors USING (source_id, generation)
             INNER JOIN source_generation_identities USING (source_id, generation)
             WHERE source_id = ?1 AND generation = ?2",
            params![registration.source_id, generation],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                ))
            },
        )
        .optional()?;
    let expected_mtime = format_timestamp(registration.mtime)?;
    let expected_session = registration
        .session_time
        .map(format_timestamp)
        .transpose()?;
    if let Some((size, mtime, session_time, cursor, file_identity)) = existing {
        if size != to_i64(registration.size_bytes)?
            || mtime != expected_mtime
            || session_time != expected_session
            || cursor != to_i64(registration.initial_cursor)?
            || file_identity != registration.file_identity
        {
            return Err(StoreError::ImmutableConflict);
        }
        return Ok(());
    }

    match maximum {
        None if registration.generation == 1 => Ok(()),
        Some(previous) if previous.checked_add(1) == Some(generation) => Ok(()),
        _ => Err(StoreError::ImmutableConflict),
    }
}

fn registration_aad(domain: &[u8], parts: &[&[u8]]) -> Result<Vec<u8>, StoreError> {
    let mut aad = Vec::with_capacity(256);
    append_aad_part(&mut aad, domain)?;
    for part in parts {
        append_aad_part(&mut aad, part)?;
    }
    if aad.len() > 32 * 1024 {
        return Err(StoreError::InvalidBatch);
    }
    Ok(aad)
}

fn append_aad_part(aad: &mut Vec<u8>, part: &[u8]) -> Result<(), StoreError> {
    aad.extend_from_slice(
        &u64::try_from(part.len())
            .map_err(|_| StoreError::InvalidBatch)?
            .to_le_bytes(),
    );
    aad.extend_from_slice(part);
    Ok(())
}

fn registration_digest(registration: &SourceRegistration) -> Result<String, StoreError> {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"agbox.source.registration.v1");
    for part in [
        registration.source_id.as_bytes(),
        &registration.generation.to_le_bytes(),
        &registration.initial_cursor.to_le_bytes(),
    ] {
        hasher.update(
            &u64::try_from(part.len())
                .map_err(|_| StoreError::InvalidBatch)?
                .to_le_bytes(),
        );
        hasher.update(part);
    }
    Ok(hasher.finalize().to_hex().to_string())
}

fn apply_graph(
    connection: &mut rusqlite::Connection,
    vault: &EvidenceVault,
    batch: &GraphWriteBatch,
) -> Result<GraphApplyReceipt, StoreError> {
    batch.validate()?;
    let sealed_paths = batch
        .artifacts
        .iter()
        .map(|row| {
            let aad = registration_aad(
                b"agbox.db.graph-artifact-path.v1",
                &[
                    row.project_id.as_str().as_bytes(),
                    row.artifact_id.as_bytes(),
                    row.path_hash.as_bytes(),
                ],
            )?;
            let plaintext = row
                .project_relative_path
                .as_deref()
                .unwrap_or_default()
                .as_bytes();
            Ok::<_, StoreError>((
                row.artifact_id.as_str(),
                vault.seal_database_field(&aad, plaintext)?,
            ))
        })
        .collect::<Result<Vec<_>, _>>()?;

    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let current: Option<(i64, String)> = transaction
        .query_row(
            "SELECT through_event_seq, through_event_id
             FROM reducer_watermarks
             WHERE reducer_name = ?1",
            [batch.reducer_name.as_str()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    let current_seq = current
        .as_ref()
        .map(|(sequence, _)| u64::try_from(*sequence).map_err(|_| StoreError::InvalidBatch))
        .transpose()?
        .unwrap_or(0);
    let replayed = current.as_ref().is_some_and(|(sequence, event_id)| {
        u64::try_from(*sequence).ok() == Some(batch.next_event_seq)
            && event_id == batch.next_event_id.as_str()
    });
    if !replayed && current_seq != batch.expected_event_seq {
        return Err(StoreError::ReducerWatermarkConflict);
    }
    verify_watermark_event(&transaction, batch.next_event_seq, &batch.next_event_id)?;
    verify_graph_batch_audit(&transaction, batch, replayed)?;

    for row in &batch.contexts {
        verify_graph_event(&transaction, &row.project_id, &row.evidence_event_id)?;
        apply_graph_context(&transaction, row)?;
    }
    for row in &batch.runs {
        verify_graph_event(&transaction, &row.project_id, &row.evidence_event_id)?;
        apply_graph_run(&transaction, row)?;
    }
    for row in &batch.actions {
        verify_graph_event(&transaction, &row.project_id, &row.request_event_id)?;
        insert_graph_action(&transaction, row)?;
    }
    for (row, (_, encrypted_path)) in batch.artifacts.iter().zip(&sealed_paths) {
        verify_graph_event(&transaction, &row.project_id, &row.evidence_event_id)?;
        insert_graph_artifact(&transaction, row, encrypted_path)?;
    }
    for row in &batch.observed_finishes {
        verify_graph_event(&transaction, &row.project_id, &row.finish_event_id)?;
    }
    for row in &batch.finishes {
        verify_graph_event(&transaction, &row.project_id, &row.finish_event_id)?;
        apply_graph_finish(&transaction, row)?;
    }
    transaction.execute(
        "INSERT INTO reducer_watermarks(
             reducer_name, through_event_seq, through_event_id, updated_at
         ) VALUES (?1, ?2, ?3, strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
         ON CONFLICT(reducer_name) DO UPDATE SET
             through_event_seq = excluded.through_event_seq,
             through_event_id = excluded.through_event_id,
             updated_at = excluded.updated_at",
        params![
            batch.reducer_name,
            to_i64(batch.next_event_seq)?,
            batch.next_event_id.as_str()
        ],
    )?;
    transaction.commit()?;
    Ok(GraphApplyReceipt {
        through_event_seq: batch.next_event_seq,
        replayed,
    })
}

#[allow(clippy::too_many_lines)]
fn apply_work(
    connection: &mut rusqlite::Connection,
    batch: &WorkWriteBatch,
) -> Result<WorkApplyReceipt, StoreError> {
    batch.validate()?;
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let current: Option<(i64, String)> = transaction
        .query_row(
            "SELECT through_event_seq, through_event_id
             FROM reducer_watermarks
             WHERE reducer_name = ?1",
            [batch.visibility_name.as_str()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    let current_seq = current
        .as_ref()
        .map(|(sequence, _)| u64::try_from(*sequence).map_err(|_| StoreError::InvalidBatch))
        .transpose()?
        .unwrap_or(0);
    let replayed = current.as_ref().is_some_and(|(sequence, event_id)| {
        u64::try_from(*sequence).ok() == Some(batch.next_event_seq)
            && event_id == batch.next_event_id.as_str()
    });
    if !replayed && current_seq != batch.expected_event_seq {
        return Err(StoreError::ReducerWatermarkConflict);
    }
    verify_watermark_event(&transaction, batch.next_event_seq, &batch.next_event_id)?;
    for event_id in &batch.evidence_event_ids {
        verify_graph_event(&transaction, &batch.project_id, event_id)?;
    }
    verify_work_batch_audit(&transaction, batch, replayed)?;

    let observed_at = format_timestamp(batch.observed_at)?;
    transaction.execute(
        "INSERT INTO work_items(work_id, project_id, status, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?4)
         ON CONFLICT(work_id) DO NOTHING",
        params![
            batch.work_id.as_str(),
            batch.project_id.as_str(),
            batch.status,
            observed_at
        ],
    )?;
    if !exists(
        &transaction,
        "SELECT EXISTS(
             SELECT 1 FROM work_items WHERE work_id = ?1 AND project_id = ?2
         )",
        params![batch.work_id.as_str(), batch.project_id.as_str()],
    )? {
        return Err(StoreError::ImmutableConflict);
    }
    transaction.execute(
        "UPDATE work_items SET status = ?1, updated_at = ?2 WHERE work_id = ?3",
        params![batch.status, observed_at, batch.work_id.as_str()],
    )?;

    for event_id in &batch.evidence_event_ids {
        transaction.execute(
            "INSERT OR IGNORE INTO work_evidence(
                 work_id, assertion_id, event_id, evidence_id
             )
             SELECT ?1, NULL, event_id, evidence_id
             FROM event_evidence
             WHERE event_id = ?2",
            params![batch.work_id.as_str(), event_id.as_str()],
        )?;
        transaction.execute(
            "UPDATE verification_facts SET work_id = ?1
             WHERE project_id = ?2 AND event_id = ?3
               AND (work_id IS NULL OR work_id = ?1)",
            params![
                batch.work_id.as_str(),
                batch.project_id.as_str(),
                event_id.as_str()
            ],
        )?;
        if exists(
            &transaction,
            "SELECT EXISTS(
                 SELECT 1 FROM verification_facts
                 WHERE project_id = ?1 AND event_id = ?2
                   AND work_id IS NOT NULL AND work_id <> ?3
             )",
            params![
                batch.project_id.as_str(),
                event_id.as_str(),
                batch.work_id.as_str()
            ],
        )? {
            return Err(StoreError::ImmutableConflict);
        }
    }

    for artifact_id in &batch.artifact_ids {
        let changed = transaction.execute(
            "UPDATE artifacts SET work_id = ?1
             WHERE artifact_id = ?2
               AND EXISTS(
                   SELECT 1 FROM work_items AS prior
                   WHERE prior.work_id = artifacts.work_id
                     AND prior.project_id = ?3
               )",
            params![
                batch.work_id.as_str(),
                artifact_id,
                batch.project_id.as_str()
            ],
        )?;
        if changed == 0 {
            return Err(StoreError::InvalidReference);
        }
    }

    for edge in &batch.edges {
        if !work_exists_in_project(&transaction, edge.from_work_id.as_str(), &batch.project_id)?
            || !work_exists_in_project(&transaction, edge.to_work_id.as_str(), &batch.project_id)?
        {
            return Err(StoreError::InvalidReference);
        }
        transaction.execute(
            "INSERT OR IGNORE INTO work_edges(
                 from_work_id, to_work_id, kind, created_at
             ) VALUES (?1, ?2, ?3, ?4)",
            params![
                edge.from_work_id.as_str(),
                edge.to_work_id.as_str(),
                edge.kind,
                observed_at
            ],
        )?;
    }

    let revision_inserted = insert_work_contract_revision(&transaction, batch, &observed_at)?;
    replace_work_search(&transaction, batch)?;
    transaction.execute(
        "INSERT INTO reducer_watermarks(
             reducer_name, through_event_seq, through_event_id, updated_at
         ) VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT(reducer_name) DO UPDATE SET
             through_event_seq = excluded.through_event_seq,
             through_event_id = excluded.through_event_id,
             updated_at = excluded.updated_at",
        params![
            batch.visibility_name,
            to_i64(batch.next_event_seq)?,
            batch.next_event_id.as_str(),
            observed_at
        ],
    )?;
    transaction.commit()?;
    Ok(WorkApplyReceipt {
        through_event_seq: batch.next_event_seq,
        replayed,
        revision_inserted,
    })
}

#[allow(clippy::too_many_lines)]
fn apply_extractor(
    connection: &mut rusqlite::Connection,
    batch: &ExtractorWriteBatch,
) -> Result<ExtractorApplyReceipt, StoreError> {
    batch.validate()?;
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    if !work_exists_in_project(&transaction, batch.work_id.as_str(), &batch.project_id)? {
        return Err(StoreError::InvalidReference);
    }
    let observed_at = format_timestamp(batch.observed_at)?;
    let batch_digest = blake3::hash(&serde_json::to_vec(batch)?)
        .to_hex()
        .to_string();
    let audit_id = extractor_audit_id(&batch.extractor_run_id);
    let existing: Option<ExistingExtractorRun> = transaction
        .query_row(
            "SELECT extractor_runs.work_id, projects.project_id,
                    extractor_runs.extractor_version, extractor_runs.input_event_watermark,
                    extractor_runs.status, extractor_runs.bounded_error,
                    extractor_runs.created_at
             FROM extractor_runs
             INNER JOIN work_items ON work_items.work_id = extractor_runs.work_id
             INNER JOIN projects ON projects.project_id = work_items.project_id
             WHERE extractor_runs.extractor_run_id = ?1",
            [batch.extractor_run_id.as_str()],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                    row.get(6)?,
                ))
            },
        )
        .optional()?;
    if let Some((
        stored_work_id,
        stored_project_id,
        version,
        watermark,
        status,
        error,
        created_at,
    )) = existing
    {
        let same = stored_work_id == batch.work_id.as_str()
            && stored_project_id == batch.project_id.as_str()
            && version == batch.extractor_version
            && watermark == batch.input_event_watermark
            && status == batch.status
            && error == batch.bounded_error
            && created_at == observed_at;
        if !same {
            return Err(StoreError::ImmutableConflict);
        }
        let stored_detail: Option<String> = transaction
            .query_row(
                "SELECT detail_json FROM audit_events WHERE audit_id = ?1",
                [audit_id.as_str()],
                |row| row.get(0),
            )
            .optional()?;
        if stored_detail.as_deref()
            != Some(format!(r#"{{"batch_digest":"{batch_digest}"}}"#).as_str())
        {
            return Err(StoreError::ImmutableConflict);
        }
        transaction.commit()?;
        return Ok(ExtractorApplyReceipt {
            replayed: true,
            revision_inserted: false,
        });
    }
    transaction.execute(
        "INSERT INTO extractor_runs(
             extractor_run_id, work_id, extractor_version, input_event_watermark,
             status, bounded_error, created_at, finished_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7)",
        params![
            batch.extractor_run_id.as_str(),
            batch.work_id.as_str(),
            batch.extractor_version.as_str(),
            batch.input_event_watermark.as_str(),
            batch.status.as_str(),
            batch.bounded_error.as_deref(),
            observed_at.as_str()
        ],
    )?;
    transaction.execute(
        "INSERT INTO audit_events(
             audit_id, kind, project_id, work_id, actor, detail_json, created_at
         ) VALUES (?1, 'semantic.extractor_batch', ?2, ?3, 'system', ?4, ?5)",
        params![
            audit_id,
            batch.project_id.as_str(),
            batch.work_id.as_str(),
            format!(r#"{{"batch_digest":"{batch_digest}"}}"#),
            observed_at.as_str()
        ],
    )?;
    let mut revision_inserted = false;
    if let Some(contract) = &batch.refined_contract {
        let stored: Option<String> = transaction
            .query_row(
                "SELECT contract_json FROM work_contract_revisions
                 WHERE work_id = ?1 AND revision = ?2",
                params![batch.work_id.as_str(), to_i64(contract.revision)?],
                |row| row.get(0),
            )
            .optional()?;
        if let Some(stored) = stored {
            if stored != contract.contract_json {
                return Err(StoreError::ImmutableConflict);
            }
        } else {
            let maximum: i64 = transaction.query_row(
                "SELECT coalesce(max(revision), 0) FROM work_contract_revisions WHERE work_id = ?1",
                [batch.work_id.as_str()],
                |row| row.get(0),
            )?;
            if to_i64(contract.revision)? != maximum + 1 {
                return Err(StoreError::ImmutableConflict);
            }
            transaction.execute(
                "INSERT INTO work_contract_revisions(
                     contract_id, work_id, revision, contract_json,
                     extractor_version, created_at
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    contract.contract_id.as_str(),
                    batch.work_id.as_str(),
                    to_i64(contract.revision)?,
                    contract.contract_json.as_str(),
                    contract.extractor_version.as_str(),
                    observed_at.as_str()
                ],
            )?;
            revision_inserted = true;
            transaction.execute(
                "DELETE FROM work_search WHERE work_id = ?1 AND project_id = ?2",
                params![batch.work_id.as_str(), batch.project_id.as_str()],
            )?;
            transaction.execute(
                "INSERT INTO work_search(
                     work_id, project_id, objective, summary, completed_steps,
                     next_actions, blockers, artifacts, verification
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                params![
                    batch.work_id.as_str(),
                    batch.project_id.as_str(),
                    contract.objective.as_deref().unwrap_or_default(),
                    contract.summary.as_str(),
                    contract.completed_steps.join("\n"),
                    contract.next_actions.join("\n"),
                    contract.blockers.join("\n"),
                    contract.artifacts.join("\n"),
                    contract.verification.join("\n")
                ],
            )?;
        }
    }
    transaction.commit()?;
    Ok(ExtractorApplyReceipt {
        replayed: false,
        revision_inserted,
    })
}

fn extractor_audit_id(run_id: &str) -> String {
    let digest = blake3::hash(run_id.as_bytes()).to_hex();
    format!("audit_extractor_{}", &digest[..24])
}

fn validate_work_candidate_query(query: &WorkCandidateQuery) -> Result<(), StoreError> {
    if !bounded_identifier(query.project_id.as_str())
        || query.artifact_hashes.len() > 64
        || query.command_hashes.len() > 32
        || query
            .artifact_hashes
            .iter()
            .chain(&query.command_hashes)
            .any(|value| !bounded_metadata(value))
    {
        return Err(StoreError::InvalidBatch);
    }
    let _ = format_timestamp(query.observed_at)?;
    Ok(())
}

fn load_work_candidates(
    connection: &rusqlite::Connection,
    query: &WorkCandidateQuery,
) -> Result<WorkCandidatePage, StoreError> {
    validate_work_candidate_query(query)?;
    let mut work_ids = Vec::<String>::new();
    let mut seen = HashSet::<String>::new();
    for work_id in [
        query.explicit_work_id.as_ref(),
        query.continuation_work_id.as_ref(),
    ]
    .into_iter()
    .flatten()
    {
        if work_exists_in_project(connection, work_id.as_str(), &query.project_id)?
            && seen.insert(work_id.as_str().to_owned())
        {
            work_ids.push(work_id.as_str().to_owned());
        }
    }
    for path_hash in &query.artifact_hashes {
        if work_ids.len() > 64 {
            break;
        }
        append_candidate_ids(
            connection,
            "SELECT DISTINCT artifacts.work_id
             FROM artifacts
             INNER JOIN work_items USING(work_id)
             WHERE work_items.project_id = ?1 AND artifacts.path_hash = ?2
               AND EXISTS(
                   SELECT 1 FROM work_contract_revisions
                   WHERE work_contract_revisions.work_id = artifacts.work_id
               )
             ORDER BY work_items.updated_at DESC, artifacts.work_id
             LIMIT 65",
            query.project_id.as_str(),
            path_hash,
            &mut work_ids,
            &mut seen,
        )?;
    }
    for input_hash in &query.command_hashes {
        if work_ids.len() > 64 {
            break;
        }
        append_candidate_ids(
            connection,
            "SELECT DISTINCT work_evidence.work_id
             FROM action_facts INDEXED BY action_facts_project_input
             INNER JOIN work_evidence
                 ON work_evidence.event_id = action_facts.request_event_id
             INNER JOIN work_items ON work_items.work_id = work_evidence.work_id
             WHERE action_facts.project_id = ?1
               AND action_facts.input_hash = ?2
               AND work_items.project_id = ?1
               AND EXISTS(
                   SELECT 1 FROM work_contract_revisions
                   WHERE work_contract_revisions.work_id = work_evidence.work_id
               )
             ORDER BY work_items.updated_at DESC, work_evidence.work_id
             LIMIT 65",
            query.project_id.as_str(),
            input_hash,
            &mut work_ids,
            &mut seen,
        )?;
    }
    if work_ids.len() <= 64 {
        let mut recent = connection.prepare_cached(
            "SELECT work_id FROM work_items
             WHERE project_id = ?1 AND status IN ('active', 'blocked')
               AND EXISTS(
                   SELECT 1 FROM work_contract_revisions
                   WHERE work_contract_revisions.work_id = work_items.work_id
               )
             ORDER BY updated_at DESC, work_id
             LIMIT 65",
        )?;
        let rows = recent.query_map([query.project_id.as_str()], |row| row.get::<_, String>(0))?;
        for row in rows {
            let work_id = row?;
            if seen.insert(work_id.clone()) {
                work_ids.push(work_id);
                if work_ids.len() > 64 {
                    break;
                }
            }
        }
    }
    let truncated = work_ids.len() > 64;
    work_ids.truncate(64);
    let candidates = work_ids
        .into_iter()
        .map(|work_id| load_work_candidate(connection, query, &work_id))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(WorkCandidatePage {
        candidates,
        truncated,
    })
}

fn append_candidate_ids(
    connection: &rusqlite::Connection,
    sql: &str,
    project_id: &str,
    hash: &str,
    work_ids: &mut Vec<String>,
    seen: &mut HashSet<String>,
) -> Result<(), StoreError> {
    if work_ids.len() > 64 {
        return Ok(());
    }
    let mut statement = connection.prepare_cached(sql)?;
    let rows = statement.query_map(params![project_id, hash], |row| row.get::<_, String>(0))?;
    for row in rows {
        let work_id = row?;
        if seen.insert(work_id.clone()) {
            work_ids.push(work_id);
            if work_ids.len() > 64 {
                break;
            }
        }
    }
    Ok(())
}

fn load_work_candidate(
    connection: &rusqlite::Connection,
    query: &WorkCandidateQuery,
    work_id: &str,
) -> Result<StoredWorkCandidate, StoreError> {
    let (repository_hash, updated_at): (String, String) = connection.query_row(
        "SELECT projects.repository_identity, work_items.updated_at
         FROM work_items
         INNER JOIN projects USING(project_id)
         WHERE work_items.work_id = ?1 AND work_items.project_id = ?2",
        params![work_id, query.project_id.as_str()],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    let mut artifact_statement = connection.prepare_cached(
        "SELECT DISTINCT path_hash FROM artifacts
         WHERE work_id = ?1 ORDER BY path_hash LIMIT 65",
    )?;
    let artifact_hashes = artifact_statement
        .query_map([work_id], |row| row.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    let mut command_statement = connection.prepare_cached(
        "SELECT DISTINCT action_facts.input_hash
         FROM work_evidence
         INNER JOIN action_facts
             ON action_facts.request_event_id = work_evidence.event_id
         WHERE work_evidence.work_id = ?1
         ORDER BY action_facts.input_hash LIMIT 33",
    )?;
    let command_hashes = command_statement
        .query_map([work_id], |row| row.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    let run: Option<(String, Option<String>)> = connection
        .query_row(
            "SELECT agent_runs.provider, agent_runs.branch_hash
             FROM agent_runs
             WHERE agent_runs.project_id = ?1
               AND EXISTS(
                   SELECT 1 FROM work_evidence
                   INNER JOIN activity_events
                       ON activity_events.event_id = work_evidence.event_id
                   WHERE work_evidence.work_id = ?2
                     AND activity_events.project_id = ?1
                     AND activity_events.session_id = agent_runs.native_session_id
               )
             ORDER BY agent_runs.started_at DESC, agent_runs.run_id
             LIMIT 1",
            params![query.project_id.as_str(), work_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    let updated_at =
        OffsetDateTime::parse(&updated_at, &Rfc3339).map_err(|_| StoreError::InvalidBatch)?;
    let elapsed = query.observed_at - updated_at;
    let minutes_since_activity = if elapsed.is_negative() {
        0
    } else {
        u32::try_from(elapsed.whole_minutes()).unwrap_or(u32::MAX)
    };
    Ok(StoredWorkCandidate {
        work_id: WorkId::parse_wire(work_id).ok_or(StoreError::InvalidBatch)?,
        project_id: query.project_id.clone(),
        provider: run.as_ref().map(|(provider, _)| provider.clone()),
        repository_hash: Some(repository_hash),
        branch_hash: run.and_then(|(_, branch)| branch),
        artifact_hashes,
        command_hashes,
        minutes_since_activity,
    })
}

fn latest_work_contract(
    connection: &rusqlite::Connection,
    project_id: &ProjectId,
    work_id: &WorkId,
) -> Result<Option<String>, StoreError> {
    connection
        .query_row(
            "SELECT work_contract_revisions.contract_json
             FROM work_contract_revisions
             INNER JOIN work_items USING(work_id)
             WHERE work_contract_revisions.work_id = ?1
               AND work_items.project_id = ?2
             ORDER BY revision DESC
             LIMIT 1",
            params![work_id.as_str(), project_id.as_str()],
            |row| row.get(0),
        )
        .optional()
        .map_err(StoreError::from)
}

fn verify_work_batch_audit(
    transaction: &Transaction<'_>,
    batch: &WorkWriteBatch,
    replayed: bool,
) -> Result<(), StoreError> {
    let digest = blake3::hash(&serde_json::to_vec(batch)?)
        .to_hex()
        .to_string();
    let audit_id = stable_audit_id(
        b"agbox.work.batch-audit.v1",
        &batch.visibility_name,
        batch.next_event_seq,
        &batch.next_event_id,
    );
    let detail = format!(r#"{{"batch_digest":"{digest}"}}"#);
    if replayed {
        let stored: Option<String> = transaction
            .query_row(
                "SELECT detail_json FROM audit_events WHERE audit_id = ?1",
                [audit_id.as_str()],
                |row| row.get(0),
            )
            .optional()?;
        if stored.as_deref() != Some(detail.as_str()) {
            return Err(StoreError::ReducerWatermarkConflict);
        }
        return Ok(());
    }
    let changed = transaction.execute(
        "INSERT OR IGNORE INTO audit_events(
             audit_id, kind, project_id, work_id, actor, detail_json, created_at
         ) VALUES (?1, 'work.publication_batch', ?2, ?3, 'system', ?4,
             strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))",
        params![
            audit_id,
            batch.project_id.as_str(),
            batch.work_id.as_str(),
            detail
        ],
    )?;
    if changed == 0 {
        return Err(StoreError::ReducerWatermarkConflict);
    }
    Ok(())
}

fn stable_audit_id(domain: &[u8], name: &str, event_seq: u64, event_id: &EventId) -> String {
    let mut identity = blake3::Hasher::new();
    identity.update(domain);
    identity.update(name.as_bytes());
    identity.update(&event_seq.to_le_bytes());
    identity.update(event_id.as_str().as_bytes());
    format!("audit_work_{}", &identity.finalize().to_hex()[..24])
}

fn insert_work_contract_revision(
    transaction: &Transaction<'_>,
    batch: &WorkWriteBatch,
    observed_at: &str,
) -> Result<bool, StoreError> {
    let stored: Option<String> = transaction
        .query_row(
            "SELECT contract_json
             FROM work_contract_revisions
             WHERE work_id = ?1 AND revision = ?2",
            params![batch.work_id.as_str(), to_i64(batch.contract.revision)?],
            |row| row.get(0),
        )
        .optional()?;
    if let Some(stored) = stored {
        if stored == batch.contract.contract_json {
            return Ok(false);
        }
        return Err(StoreError::ImmutableConflict);
    }
    let maximum: i64 = transaction.query_row(
        "SELECT coalesce(max(revision), 0)
         FROM work_contract_revisions WHERE work_id = ?1",
        [batch.work_id.as_str()],
        |row| row.get(0),
    )?;
    if to_i64(batch.contract.revision)? != maximum + 1 {
        return Err(StoreError::ImmutableConflict);
    }
    transaction.execute(
        "INSERT INTO work_contract_revisions(
             contract_id, work_id, revision, contract_json,
             extractor_version, created_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            batch.contract.contract_id.as_str(),
            batch.work_id.as_str(),
            to_i64(batch.contract.revision)?,
            batch.contract.contract_json,
            batch.contract.extractor_version,
            observed_at
        ],
    )?;
    Ok(true)
}

fn replace_work_search(
    transaction: &Transaction<'_>,
    batch: &WorkWriteBatch,
) -> Result<(), StoreError> {
    transaction.execute(
        "DELETE FROM work_search WHERE work_id = ?1 AND project_id = ?2",
        params![batch.work_id.as_str(), batch.project_id.as_str()],
    )?;
    transaction.execute(
        "INSERT INTO work_search(
             work_id, project_id, objective, summary, completed_steps,
             next_actions, blockers, artifacts, verification
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        params![
            batch.work_id.as_str(),
            batch.project_id.as_str(),
            batch.contract.objective.as_deref().unwrap_or_default(),
            batch.contract.summary,
            batch.contract.completed_steps.join("\n"),
            batch.contract.next_actions.join("\n"),
            batch.contract.blockers.join("\n"),
            batch.contract.artifacts.join("\n"),
            batch.contract.verification.join("\n")
        ],
    )?;
    Ok(())
}

fn verify_graph_batch_audit(
    transaction: &Transaction<'_>,
    batch: &GraphWriteBatch,
    replayed: bool,
) -> Result<(), StoreError> {
    let encoded = serde_json::to_vec(batch)?;
    let digest = blake3::hash(&encoded).to_hex().to_string();
    let mut identity = blake3::Hasher::new();
    identity.update(b"agbox.graph.batch-audit.v1");
    identity.update(batch.reducer_name.as_bytes());
    identity.update(&batch.next_event_seq.to_le_bytes());
    identity.update(batch.next_event_id.as_str().as_bytes());
    let audit_id = format!("audit_graph_{}", &identity.finalize().to_hex()[..24]);
    let detail = format!(r#"{{"batch_digest":"{digest}"}}"#);

    if replayed {
        let stored: Option<String> = transaction
            .query_row(
                "SELECT detail_json FROM audit_events WHERE audit_id = ?1",
                [audit_id.as_str()],
                |row| row.get(0),
            )
            .optional()?;
        if stored.as_deref() != Some(detail.as_str()) {
            return Err(StoreError::ReducerWatermarkConflict);
        }
        return Ok(());
    }

    let changed = transaction.execute(
        "INSERT OR IGNORE INTO audit_events(
             audit_id, kind, project_id, work_id, actor, detail_json, created_at
         ) VALUES (?1, 'graph.reducer_batch', NULL, NULL, 'system', ?2,
             strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))",
        params![audit_id, detail],
    )?;
    if changed == 0 {
        return Err(StoreError::ReducerWatermarkConflict);
    }
    Ok(())
}

fn verify_watermark_event(
    transaction: &Transaction<'_>,
    event_seq: u64,
    event_id: &EventId,
) -> Result<(), StoreError> {
    let stored: Option<String> = transaction
        .query_row(
            "SELECT event_id FROM activity_events WHERE event_seq = ?1",
            [to_i64(event_seq)?],
            |row| row.get(0),
        )
        .optional()?;
    if stored.as_deref() != Some(event_id.as_str()) {
        return Err(StoreError::InvalidReference);
    }
    Ok(())
}

fn verify_graph_event(
    transaction: &Transaction<'_>,
    project_id: &ProjectId,
    event_id: &EventId,
) -> Result<(), StoreError> {
    let stored: Option<String> = transaction
        .query_row(
            "SELECT project_id FROM activity_events WHERE event_id = ?1",
            [event_id.as_str()],
            |row| row.get(0),
        )
        .optional()?;
    if stored.as_deref() != Some(project_id.as_str()) {
        return Err(StoreError::InvalidReference);
    }
    Ok(())
}

fn apply_graph_run(transaction: &Transaction<'_>, row: &GraphRunRow) -> Result<(), StoreError> {
    let observed_at = format_timestamp(row.observed_at)?;
    let branch_hash: Option<String> = transaction
        .query_row(
            "SELECT branch_hash
             FROM agent_runs
             WHERE project_id = ?1 AND native_session_id = ?2
               AND provider = ?3
               AND run_id LIKE 'context_%'
             ORDER BY started_at DESC, run_id
             LIMIT 1",
            params![
                row.project_id.as_str(),
                row.session_id.as_str(),
                row.provider.as_str()
            ],
            |record| record.get(0),
        )
        .optional()?
        .flatten();
    let status = if row.finished {
        if row.succeeded == Some(true) {
            "succeeded"
        } else {
            "failed"
        }
    } else {
        "running"
    };
    let changed = transaction.execute(
        "INSERT OR IGNORE INTO agent_runs(
             run_id, project_id, provider, native_session_id, branch_hash,
             started_at, finished_at, status
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        params![
            row.run_id,
            row.project_id.as_str(),
            row.provider.as_str(),
            row.session_id.as_str(),
            branch_hash,
            observed_at,
            row.finished.then_some(observed_at.as_str()),
            status
        ],
    )?;
    if changed == 0 {
        let immutable_matches: bool = transaction.query_row(
            "SELECT EXISTS(
                 SELECT 1 FROM agent_runs
                 WHERE run_id = ?1 AND project_id = ?2
                   AND provider = ?3 AND native_session_id = ?4
             )",
            params![
                row.run_id,
                row.project_id.as_str(),
                row.provider.as_str(),
                row.session_id.as_str()
            ],
            |record| record.get(0),
        )?;
        if !immutable_matches {
            return Err(StoreError::ImmutableConflict);
        }
        if row.finished {
            transaction.execute(
                "UPDATE agent_runs
                 SET finished_at = ?1, status = ?2
                 WHERE run_id = ?3
                   AND (finished_at IS NULL OR (finished_at = ?1 AND status = ?2))",
                params![observed_at, status, row.run_id],
            )?;
            let finish_matches: bool = transaction.query_row(
                "SELECT EXISTS(
                     SELECT 1 FROM agent_runs
                     WHERE run_id = ?1 AND finished_at = ?2 AND status = ?3
                 )",
                params![row.run_id, observed_at, status],
                |record| record.get(0),
            )?;
            if !finish_matches {
                return Err(StoreError::ImmutableConflict);
            }
        }
    }
    Ok(())
}

fn apply_graph_context(
    transaction: &Transaction<'_>,
    row: &GraphSessionContextRow,
) -> Result<(), StoreError> {
    let observed_at = format_timestamp(row.observed_at)?;
    transaction.execute(
        "INSERT OR IGNORE INTO agent_runs(
             run_id, project_id, provider, native_session_id, branch_hash,
             started_at, finished_at, status
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, NULL, 'observed')",
        params![
            row.context_run_id,
            row.project_id.as_str(),
            row.provider.as_str(),
            row.session_id.as_str(),
            row.branch_hash,
            observed_at
        ],
    )?;
    let immutable_matches: bool = transaction.query_row(
        "SELECT EXISTS(
             SELECT 1 FROM agent_runs
             WHERE run_id = ?1 AND project_id = ?2 AND provider = ?3
               AND native_session_id = ?4 AND status = 'observed'
         )",
        params![
            row.context_run_id,
            row.project_id.as_str(),
            row.provider.as_str(),
            row.session_id.as_str()
        ],
        |record| record.get(0),
    )?;
    if !immutable_matches {
        return Err(StoreError::ImmutableConflict);
    }
    transaction.execute(
        "UPDATE agent_runs
         SET branch_hash = ?1, started_at = ?2
         WHERE run_id = ?3",
        params![row.branch_hash, observed_at, row.context_run_id],
    )?;
    transaction.execute(
        "UPDATE agent_runs
         SET branch_hash = ?1
         WHERE project_id = ?2 AND native_session_id = ?3
           AND provider = ?4 AND run_id <> ?5",
        params![
            row.branch_hash,
            row.project_id.as_str(),
            row.session_id.as_str(),
            row.provider.as_str(),
            row.context_run_id
        ],
    )?;
    Ok(())
}

fn insert_graph_action(
    transaction: &Transaction<'_>,
    row: &GraphActionRow,
) -> Result<(), StoreError> {
    let changed = transaction.execute(
        "INSERT OR IGNORE INTO action_facts(
             project_id, session_id, native_action_id, request_event_id,
             finish_event_id, tool_name, input_hash, redacted_command, succeeded
         ) VALUES (?1, ?2, ?3, ?4, NULL, ?5, ?6, ?7, NULL)",
        params![
            row.project_id.as_str(),
            row.session_id.as_str(),
            row.native_action_id,
            row.request_event_id.as_str(),
            row.tool_name,
            row.input_hash,
            row.redacted_command
        ],
    )?;
    if changed == 0
        && !exists(
            transaction,
            "SELECT EXISTS(
                 SELECT 1 FROM action_facts
                 WHERE project_id = ?1 AND session_id = ?2
                   AND native_action_id = ?3 AND request_event_id = ?4
                   AND tool_name = ?5 AND input_hash = ?6
                   AND redacted_command IS ?7
             )",
            params![
                row.project_id.as_str(),
                row.session_id.as_str(),
                row.native_action_id,
                row.request_event_id.as_str(),
                row.tool_name,
                row.input_hash,
                row.redacted_command
            ],
        )?
    {
        return Err(StoreError::ImmutableConflict);
    }
    Ok(())
}

fn insert_graph_artifact(
    transaction: &Transaction<'_>,
    row: &GraphArtifactRow,
    encrypted_path: &[u8],
) -> Result<(), StoreError> {
    let observed_at = format_timestamp(row.observed_at)?;
    transaction.execute(
        "INSERT INTO work_items(work_id, project_id, status, created_at, updated_at)
         VALUES (?1, ?2, 'observed', ?3, ?3)
         ON CONFLICT(work_id) DO NOTHING",
        params![row.work_id.as_str(), row.project_id.as_str(), observed_at],
    )?;
    let work_matches: bool = transaction.query_row(
        "SELECT EXISTS(
             SELECT 1 FROM work_items WHERE work_id = ?1 AND project_id = ?2
         )",
        params![row.work_id.as_str(), row.project_id.as_str()],
        |record| record.get(0),
    )?;
    if !work_matches {
        return Err(StoreError::ImmutableConflict);
    }
    let changed = transaction.execute(
        "INSERT OR IGNORE INTO artifacts(
             artifact_id, work_id, path_hash, encrypted_path,
             content_hash, operation, observed_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            row.artifact_id,
            row.work_id.as_str(),
            row.path_hash,
            encrypted_path,
            row.content_hash,
            row.operation,
            observed_at
        ],
    )?;
    if changed == 0
        && !exists(
            transaction,
            "SELECT EXISTS(
                 SELECT 1 FROM artifacts
                 WHERE artifact_id = ?1 AND work_id = ?2 AND path_hash = ?3
                   AND content_hash IS ?4 AND operation = ?5 AND observed_at = ?6
             )",
            params![
                row.artifact_id,
                row.work_id.as_str(),
                row.path_hash,
                row.content_hash,
                row.operation,
                observed_at
            ],
        )?
    {
        return Err(StoreError::ImmutableConflict);
    }
    transaction.execute(
        "INSERT OR IGNORE INTO work_evidence(
             work_id, assertion_id, event_id, evidence_id
         )
         SELECT ?1, NULL, event_id, evidence_id
         FROM event_evidence
         WHERE event_id = ?2",
        params![row.work_id.as_str(), row.evidence_event_id.as_str()],
    )?;
    Ok(())
}

#[allow(clippy::too_many_lines)]
fn apply_graph_finish(
    transaction: &Transaction<'_>,
    row: &GraphFinishRow,
) -> Result<(), StoreError> {
    let request_event_id: Option<String> = transaction
        .query_row(
            "SELECT action_facts.request_event_id
             FROM action_facts
             INNER JOIN activity_events AS request_events
                 ON request_events.event_id = action_facts.request_event_id
             INNER JOIN activity_events AS finish_events
                 ON finish_events.event_id = ?4
             WHERE action_facts.project_id = ?1
               AND action_facts.session_id = ?2
               AND action_facts.native_action_id = ?3
               AND request_events.event_seq <= finish_events.event_seq
             ORDER BY request_events.event_seq DESC
             LIMIT 1",
            params![
                row.project_id.as_str(),
                row.session_id.as_str(),
                row.native_action_id,
                row.finish_event_id.as_str()
            ],
            |record| record.get(0),
        )
        .optional()?;
    let Some(request_event_id) = request_event_id else {
        return Ok(());
    };
    transaction.execute(
        "UPDATE action_facts
         SET finish_event_id = ?1, succeeded = ?2
         WHERE project_id = ?3 AND session_id = ?4
           AND native_action_id = ?5 AND request_event_id = ?6
           AND (finish_event_id IS NULL OR finish_event_id = ?1)",
        params![
            row.finish_event_id.as_str(),
            i64::from(row.succeeded),
            row.project_id.as_str(),
            row.session_id.as_str(),
            row.native_action_id,
            request_event_id
        ],
    )?;
    let action_matches: bool = transaction.query_row(
        "SELECT EXISTS(
             SELECT 1 FROM action_facts
             WHERE project_id = ?1 AND session_id = ?2
               AND native_action_id = ?3 AND request_event_id = ?4
               AND finish_event_id = ?5 AND succeeded = ?6
         )",
        params![
            row.project_id.as_str(),
            row.session_id.as_str(),
            row.native_action_id,
            request_event_id,
            row.finish_event_id.as_str(),
            i64::from(row.succeeded)
        ],
        |record| record.get(0),
    )?;
    if !action_matches {
        return Err(StoreError::ImmutableConflict);
    }
    let observed_at = format_timestamp(row.observed_at)?;
    let changed = transaction.execute(
        "INSERT OR IGNORE INTO verification_facts(
             verification_id, project_id, work_id, session_id,
             native_action_id, succeeded, basis, event_id, observed_at
         ) VALUES (?1, ?2, NULL, ?3, ?4, ?5, ?6, ?7, ?8)",
        params![
            row.verification_id,
            row.project_id.as_str(),
            row.session_id.as_str(),
            row.native_action_id,
            i64::from(row.succeeded),
            row.basis,
            row.finish_event_id.as_str(),
            observed_at
        ],
    )?;
    if changed == 0
        && !exists(
            transaction,
            "SELECT EXISTS(
                 SELECT 1 FROM verification_facts
                 WHERE verification_id = ?1 AND project_id = ?2
                   AND session_id = ?3 AND native_action_id = ?4
                   AND succeeded = ?5 AND basis = ?6
                   AND event_id = ?7 AND observed_at = ?8
             )",
            params![
                row.verification_id,
                row.project_id.as_str(),
                row.session_id.as_str(),
                row.native_action_id,
                i64::from(row.succeeded),
                row.basis,
                row.finish_event_id.as_str(),
                observed_at
            ],
        )?
    {
        return Err(StoreError::ImmutableConflict);
    }
    Ok(())
}

fn commit(
    connection: &mut rusqlite::Connection,
    vault: &EvidenceVault,
    chunk: &IngestionChunk,
) -> Result<CommitReceipt, StoreError> {
    chunk.validate()?;
    let commit_digest = chunk.commit_digest()?;
    let preflight_cursor = load_cursor(connection, &chunk.expected_cursor)?;
    let registered = load_registered_source(connection, chunk)?;
    validate_project_and_provider(chunk, &registered)?;
    validate_evidence_relations(connection, chunk, &registered)?;

    if cursor_matches(preflight_cursor.as_ref(), &chunk.next_cursor) {
        let transaction = connection.transaction()?;
        let current = load_cursor(&transaction, &chunk.expected_cursor)?;
        if !cursor_matches(current.as_ref(), &chunk.next_cursor) {
            return Err(StoreError::CursorConflict);
        }
        if current
            .as_ref()
            .is_none_or(|cursor| cursor.last_commit_digest != commit_digest)
        {
            return Err(StoreError::ImmutableConflict);
        }
        let registered = load_registered_source(&transaction, chunk)?;
        validate_project_and_provider(chunk, &registered)?;
        validate_evidence_relations(&transaction, chunk, &registered)?;
        verify_retry(&transaction, chunk)?;
        transaction.commit()?;
        persist_evidence_blobs(vault, &chunk.evidence)?;
        return Ok(receipt(chunk, 0));
    }

    if !cursor_matches_expected(preflight_cursor.as_ref(), &chunk.expected_cursor) {
        return Err(StoreError::CursorConflict);
    }

    persist_evidence_blobs(vault, &chunk.evidence)?;
    let transaction = connection.transaction()?;
    let current = load_cursor(&transaction, &chunk.expected_cursor)?;
    if !cursor_matches_expected(current.as_ref(), &chunk.expected_cursor) {
        return Err(StoreError::CursorConflict);
    }
    let registered = load_registered_source(&transaction, chunk)?;
    validate_project_and_provider(chunk, &registered)?;
    validate_evidence_relations(&transaction, chunk, &registered)?;

    insert_observations(&transaction, chunk)?;
    let inserted_events = insert_events(&transaction, &chunk.events)?;
    insert_evidence_objects(&transaction, &chunk.evidence)?;
    insert_evidence_links(&transaction, &chunk.evidence_links)?;
    insert_content_refs(&transaction, &chunk.content_refs)?;
    upsert_schema_fingerprints(&transaction, &chunk.fingerprints)?;
    insert_faults(&transaction, &chunk.faults)?;
    transaction.execute(
        "INSERT INTO source_cursors(
             source_id, generation, cursor_offset, parser_state,
             last_commit_digest, updated_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
         ON CONFLICT(source_id, generation) DO UPDATE SET
             cursor_offset = excluded.cursor_offset,
             parser_state = excluded.parser_state,
             last_commit_digest = excluded.last_commit_digest,
             updated_at = excluded.updated_at",
        params![
            chunk.next_cursor.source_id,
            to_i64(chunk.next_cursor.generation)?,
            to_i64(chunk.next_cursor.offset)?,
            chunk.next_cursor.parser_state,
            commit_digest,
        ],
    )?;
    transaction.commit()?;
    Ok(receipt(chunk, inserted_events))
}

fn receipt(chunk: &IngestionChunk, inserted_events: usize) -> CommitReceipt {
    CommitReceipt {
        source_id: chunk.next_cursor.source_id.clone(),
        generation: chunk.next_cursor.generation,
        cursor_offset: chunk.next_cursor.offset,
        inserted_events,
    }
}

struct RegisteredSource {
    project_id: ProjectId,
    provider: Provider,
}

fn load_registered_source(
    connection: &rusqlite::Connection,
    chunk: &IngestionChunk,
) -> Result<RegisteredSource, StoreError> {
    let value: Option<(String, String)> = connection
        .query_row(
            "SELECT sources.project_id, sources.provider
             FROM sources
             INNER JOIN source_generations USING (source_id)
             WHERE sources.source_id = ?1 AND source_generations.generation = ?2",
            params![
                chunk.expected_cursor.source_id,
                to_i64(chunk.expected_cursor.generation)?
            ],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    let (project, provider) = value.ok_or(StoreError::SourceNotFound)?;
    let project_id = ProjectId::parse_wire(&project).ok_or(StoreError::InvalidBatch)?;
    let provider = match provider.as_str() {
        "claude" => Provider::Claude,
        "codex" => Provider::Codex,
        _ => return Err(StoreError::InvalidBatch),
    };
    Ok(RegisteredSource {
        project_id,
        provider,
    })
}

fn validate_project_and_provider(
    chunk: &IngestionChunk,
    registered: &RegisteredSource,
) -> Result<(), StoreError> {
    if chunk
        .events
        .iter()
        .any(|event| event.project_id() != &registered.project_id)
        || chunk
            .evidence
            .iter()
            .any(|item| item.project_id != registered.project_id)
        || chunk
            .content_refs
            .iter()
            .any(|item| item.project_id != registered.project_id)
        || chunk
            .events
            .iter()
            .any(|event| event.source().provider() != registered.provider)
        || chunk
            .observations
            .iter()
            .any(|item| item.source().provider() != registered.provider)
        || chunk
            .fingerprints
            .iter()
            .any(|item| item.provider != registered.provider.as_str())
    {
        return Err(StoreError::ProjectMismatch);
    }
    Ok(())
}

fn validate_evidence_relations(
    connection: &rusqlite::Connection,
    chunk: &IngestionChunk,
    registered: &RegisteredSource,
) -> Result<(), StoreError> {
    let chunk_events: HashSet<&str> = chunk
        .events
        .iter()
        .map(|event| event.event_id().as_str())
        .collect();
    let chunk_evidence: HashSet<&str> = chunk
        .evidence
        .iter()
        .map(|evidence| evidence.evidence_id.as_str())
        .collect();
    let chunk_observations: HashSet<&str> = chunk
        .observations
        .iter()
        .map(SourceObservation::observation_id)
        .collect();

    for evidence in &chunk.evidence {
        if !evidence_reference_is_valid(
            connection,
            evidence.evidence_id.as_str(),
            &registered.project_id,
            true,
        )? {
            return Err(StoreError::InvalidReference);
        }
        let owner_is_valid = match &evidence.owner {
            EvidenceOwner::Event(event_id) => {
                let created_in_chunk = chunk_events.contains(event_id.as_str());
                event_reference_is_valid(
                    connection,
                    event_id.as_str(),
                    &registered.project_id,
                    created_in_chunk,
                )?
            }
            EvidenceOwner::Work(work_id) => {
                work_exists_in_project(connection, work_id.as_str(), &registered.project_id)?
            }
        };
        if !owner_is_valid {
            return Err(StoreError::InvalidReference);
        }
    }

    for link in &chunk.evidence_links {
        let event_in_chunk = chunk_events.contains(link.event_id.as_str());
        let event_is_valid = event_reference_is_valid(
            connection,
            &link.event_id,
            &registered.project_id,
            event_in_chunk,
        )?;
        let evidence_in_chunk = chunk_evidence.contains(link.evidence_id.as_str());
        let evidence_is_valid = evidence_reference_is_valid(
            connection,
            &link.evidence_id,
            &registered.project_id,
            evidence_in_chunk,
        )?;
        let observation_in_chunk = chunk_observations.contains(link.observation_id.as_str());
        let observation_is_valid = observation_reference_is_valid(
            connection,
            &link.observation_id,
            &chunk.expected_cursor,
            &registered.project_id,
            observation_in_chunk,
        )?;
        if !event_is_valid || !evidence_is_valid || !observation_is_valid {
            return Err(StoreError::InvalidReference);
        }
    }
    Ok(())
}

fn event_reference_is_valid(
    connection: &rusqlite::Connection,
    event_id: &str,
    project_id: &ProjectId,
    created_in_chunk: bool,
) -> Result<bool, StoreError> {
    let mut statement =
        connection.prepare_cached("SELECT project_id FROM activity_events WHERE event_id = ?1")?;
    let stored_project: Option<String> = statement
        .query_row([event_id], |row| row.get(0))
        .optional()?;
    Ok(stored_project
        .as_deref()
        .map_or(created_in_chunk, |stored| stored == project_id.as_str()))
}

fn work_exists_in_project(
    connection: &rusqlite::Connection,
    work_id: &str,
    project_id: &ProjectId,
) -> Result<bool, StoreError> {
    let mut statement = connection.prepare_cached(
        "SELECT EXISTS(
             SELECT 1 FROM work_items
             WHERE work_id = ?1 AND project_id = ?2
         )",
    )?;
    Ok(statement.query_row(params![work_id, project_id.as_str()], |row| row.get(0))?)
}

fn evidence_reference_is_valid(
    connection: &rusqlite::Connection,
    evidence_id: &str,
    project_id: &ProjectId,
    created_in_chunk: bool,
) -> Result<bool, StoreError> {
    let mut statement = connection
        .prepare_cached("SELECT project_id FROM evidence_objects WHERE evidence_id = ?1")?;
    let stored_project: Option<String> = statement
        .query_row([evidence_id], |row| row.get(0))
        .optional()?;
    Ok(stored_project
        .as_deref()
        .map_or(created_in_chunk, |stored| stored == project_id.as_str()))
}

fn observation_reference_is_valid(
    connection: &rusqlite::Connection,
    observation_id: &str,
    cursor: &CursorState,
    project_id: &ProjectId,
    created_in_chunk: bool,
) -> Result<bool, StoreError> {
    let mut statement = connection.prepare_cached(
        "SELECT source_observations.source_id,
                source_observations.generation,
                sources.project_id
             FROM source_observations
             INNER JOIN sources USING (source_id)
             WHERE observation_id = ?1
         ",
    )?;
    let stored: Option<(String, i64, String)> = statement
        .query_row([observation_id], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?))
        })
        .optional()?;
    stored.map_or_else(
        || Ok(created_in_chunk),
        |(source_id, generation, stored_project)| {
            Ok(source_id == cursor.source_id
                && generation == to_i64(cursor.generation)?
                && stored_project == project_id.as_str())
        },
    )
}

struct StoredCursor {
    offset: u64,
    parser_state: Vec<u8>,
    last_commit_digest: String,
}

fn load_cursor(
    connection: &rusqlite::Connection,
    cursor: &CursorState,
) -> Result<Option<StoredCursor>, StoreError> {
    connection
        .query_row(
            "SELECT cursor_offset, parser_state, last_commit_digest
             FROM source_cursors
             WHERE source_id = ?1 AND generation = ?2",
            params![cursor.source_id, to_i64(cursor.generation)?],
            |row| {
                let offset: i64 = row.get(0)?;
                let parser_state = row.get(1)?;
                let last_commit_digest = row.get(2)?;
                Ok((offset, parser_state, last_commit_digest))
            },
        )
        .optional()?
        .map(|(offset, parser_state, last_commit_digest)| {
            let offset = u64::try_from(offset).map_err(|_| StoreError::InvalidBatch)?;
            Ok(StoredCursor {
                offset,
                parser_state,
                last_commit_digest,
            })
        })
        .transpose()
}

fn cursor_matches(current: Option<&StoredCursor>, wanted: &CursorState) -> bool {
    current.is_some_and(|cursor| {
        cursor.offset == wanted.offset && cursor.parser_state == wanted.parser_state
    })
}

fn cursor_matches_expected(current: Option<&StoredCursor>, wanted: &CursorState) -> bool {
    current.map_or_else(
        || wanted.offset == 0 && wanted.parser_state.is_empty(),
        |cursor| cursor.offset == wanted.offset && cursor.parser_state == wanted.parser_state,
    )
}

fn persist_evidence_blobs(
    vault: &EvidenceVault,
    evidence: &[EvidenceWrite],
) -> Result<(), StoreError> {
    for item in evidence {
        let owner = match &item.owner {
            EvidenceOwner::Event(id) => EvidenceOwnerRef::Event(id),
            EvidenceOwner::Work(id) => EvidenceOwnerRef::Work(id),
        };
        vault.put(
            &item.evidence_id,
            EvidenceContext {
                project_id: &item.project_id,
                owner,
            },
            &item.plaintext,
        )?;
    }
    Ok(())
}

fn insert_observations(
    transaction: &Transaction<'_>,
    chunk: &IngestionChunk,
) -> Result<(), StoreError> {
    for item in &chunk.observations {
        let values = ObservationValues::new(chunk, item)?;
        let changed = transaction.execute(
            "INSERT OR IGNORE INTO source_observations(
                 observation_id, source_id, generation, byte_start, byte_end,
                 record_hash, native_record_type, decode_status,
                 schema_fingerprint, observed_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                values.observation_id,
                values.source_id,
                values.generation,
                values.byte_start,
                values.byte_end,
                values.record_hash,
                values.native_record_type,
                values.decode_status,
                values.schema_fingerprint,
                values.observed_at,
            ],
        )?;
        if changed == 0 && !observation_exact(transaction, &values)? {
            return Err(StoreError::ImmutableConflict);
        }
    }
    Ok(())
}

struct ObservationValues<'a> {
    observation_id: &'a str,
    source_id: &'a str,
    generation: i64,
    byte_start: i64,
    byte_end: i64,
    record_hash: &'a str,
    native_record_type: &'a str,
    decode_status: &'static str,
    schema_fingerprint: &'a str,
    observed_at: String,
}

impl<'a> ObservationValues<'a> {
    fn new(chunk: &'a IngestionChunk, item: &'a SourceObservation) -> Result<Self, StoreError> {
        Ok(Self {
            observation_id: item.observation_id(),
            source_id: &chunk.expected_cursor.source_id,
            generation: to_i64(chunk.expected_cursor.generation)?,
            byte_start: to_i64(item.range().start())?,
            byte_end: to_i64(item.range().end())?,
            record_hash: item.source().record_hash(),
            native_record_type: item.source().native_record_type(),
            decode_status: decode_status(item.status()),
            schema_fingerprint: item.schema_fingerprint(),
            observed_at: format_timestamp(item.observed_at())?,
        })
    }
}

fn observation_exact(
    transaction: &Transaction<'_>,
    value: &ObservationValues<'_>,
) -> Result<bool, StoreError> {
    exists(
        transaction,
        "SELECT EXISTS(
             SELECT 1 FROM source_observations
             WHERE observation_id = ?1 AND source_id = ?2 AND generation = ?3
               AND byte_start = ?4 AND byte_end = ?5 AND record_hash = ?6
               AND native_record_type = ?7 AND decode_status = ?8
               AND schema_fingerprint = ?9 AND observed_at = ?10
         )",
        params![
            value.observation_id,
            value.source_id,
            value.generation,
            value.byte_start,
            value.byte_end,
            value.record_hash,
            value.native_record_type,
            value.decode_status,
            value.schema_fingerprint,
            value.observed_at,
        ],
    )
}

fn insert_events(
    transaction: &Transaction<'_>,
    events: &[ActivityEventV1],
) -> Result<usize, StoreError> {
    let mut inserted = 0_usize;
    for item in events {
        let values = EventValues::new(item)?;
        let changed = transaction.execute(
            "INSERT OR IGNORE INTO activity_events(
                 event_id, semantic_key, schema_version, occurred_at, observed_at,
                 project_id, session_id, turn_id, actor, correlation_id,
                 causation_id, source_json, payload_json, privacy
             ) VALUES (
                 ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14
             )",
            params![
                values.event_id,
                values.semantic_key,
                values.schema_version,
                values.occurred_at,
                values.observed_at,
                values.project_id,
                values.session_id,
                values.turn_id,
                values.actor,
                values.correlation_id,
                values.causation_id,
                values.source_json,
                values.payload_json,
                values.privacy,
            ],
        )?;
        if changed == 0 && !event_exact(transaction, &values)? {
            return Err(StoreError::ImmutableConflict);
        }
        inserted = inserted
            .checked_add(changed)
            .ok_or(StoreError::InvalidBatch)?;
    }
    Ok(inserted)
}

struct EventValues<'a> {
    event_id: &'a str,
    semantic_key: &'a str,
    schema_version: i64,
    occurred_at: String,
    observed_at: String,
    project_id: &'a str,
    session_id: &'a str,
    turn_id: Option<&'a str>,
    actor: &'static str,
    correlation_id: Option<&'a str>,
    causation_id: Option<&'a str>,
    source_json: String,
    payload_json: String,
    privacy: &'static str,
}

impl<'a> EventValues<'a> {
    fn new(item: &'a ActivityEventV1) -> Result<Self, StoreError> {
        Ok(Self {
            event_id: item.event_id().as_str(),
            semantic_key: item.semantic_key().as_str(),
            schema_version: i64::from(item.schema_version()),
            occurred_at: format_timestamp(item.occurred_at())?,
            observed_at: format_timestamp(item.observed_at())?,
            project_id: item.project_id().as_str(),
            session_id: item.session_id().as_str(),
            turn_id: item.turn_id(),
            actor: actor(item.actor()),
            correlation_id: item.correlation_id(),
            causation_id: item.causation_id(),
            source_json: serde_json::to_string(item.source())?,
            payload_json: serde_json::to_string(item.payload())?,
            privacy: privacy(item.privacy()),
        })
    }
}

fn event_exact(transaction: &Transaction<'_>, value: &EventValues<'_>) -> Result<bool, StoreError> {
    exists(
        transaction,
        "SELECT EXISTS(
             SELECT 1 FROM activity_events
             WHERE event_id = ?1 AND semantic_key = ?2 AND schema_version = ?3
               AND occurred_at = ?4 AND observed_at = ?5 AND project_id = ?6
               AND session_id = ?7 AND turn_id IS ?8 AND actor = ?9
               AND correlation_id IS ?10 AND causation_id IS ?11
               AND source_json = ?12 AND payload_json = ?13 AND privacy = ?14
         )",
        params![
            value.event_id,
            value.semantic_key,
            value.schema_version,
            value.occurred_at,
            value.observed_at,
            value.project_id,
            value.session_id,
            value.turn_id,
            value.actor,
            value.correlation_id,
            value.causation_id,
            value.source_json,
            value.payload_json,
            value.privacy,
        ],
    )
}

fn insert_evidence_objects(
    transaction: &Transaction<'_>,
    evidence: &[EvidenceWrite],
) -> Result<(), StoreError> {
    for item in evidence {
        let values = EvidenceValues::new(item)?;
        let changed = transaction.execute(
            "INSERT OR IGNORE INTO evidence_objects(
                 evidence_id, project_id, owner_kind, owner_id, content_hash,
                 media_type, privacy, byte_length, redacted_excerpt,
                 disclosure_class, blob_state, created_at, expires_at, retired_at
             ) VALUES (
                 ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, 'available',
                 strftime('%Y-%m-%dT%H:%M:%fZ', 'now'), ?11, NULL
             )",
            params![
                values.evidence_id,
                values.project_id,
                values.owner_kind,
                values.owner_id,
                values.content_hash,
                values.media_type,
                values.privacy,
                values.byte_length,
                values.redacted_excerpt,
                values.disclosure_class,
                values.expires_at,
            ],
        )?;
        if changed == 0 && !evidence_exact(transaction, &values)? {
            return Err(StoreError::ImmutableConflict);
        }
    }
    Ok(())
}

struct EvidenceValues<'a> {
    evidence_id: &'a str,
    project_id: &'a str,
    owner_kind: &'static str,
    owner_id: &'a str,
    content_hash: &'a str,
    media_type: &'a str,
    privacy: &'static str,
    byte_length: i64,
    redacted_excerpt: &'a str,
    disclosure_class: &'static str,
    expires_at: Option<String>,
}

impl<'a> EvidenceValues<'a> {
    fn new(item: &'a EvidenceWrite) -> Result<Self, StoreError> {
        Ok(Self {
            evidence_id: item.evidence_id.as_str(),
            project_id: item.project_id.as_str(),
            owner_kind: owner_kind(&item.owner),
            owner_id: owner_id(&item.owner),
            content_hash: &item.content_hash,
            media_type: &item.media_type,
            privacy: privacy(item.privacy),
            byte_length: i64::try_from(item.plaintext.len())
                .map_err(|_| StoreError::InvalidBatch)?,
            redacted_excerpt: &item.redacted_excerpt,
            disclosure_class: disclosure(item.disclosure_class),
            expires_at: item.expires_at.map(format_timestamp).transpose()?,
        })
    }
}

fn evidence_exact(
    transaction: &Transaction<'_>,
    value: &EvidenceValues<'_>,
) -> Result<bool, StoreError> {
    exists(
        transaction,
        "SELECT EXISTS(
             SELECT 1 FROM evidence_objects
             WHERE evidence_id = ?1 AND project_id = ?2 AND owner_kind = ?3
               AND owner_id = ?4 AND content_hash = ?5 AND media_type = ?6
               AND privacy = ?7 AND byte_length = ?8 AND redacted_excerpt = ?9
               AND disclosure_class = ?10 AND blob_state = 'available'
               AND expires_at IS ?11 AND retired_at IS NULL
         )",
        params![
            value.evidence_id,
            value.project_id,
            value.owner_kind,
            value.owner_id,
            value.content_hash,
            value.media_type,
            value.privacy,
            value.byte_length,
            value.redacted_excerpt,
            value.disclosure_class,
            value.expires_at,
        ],
    )
}

fn insert_evidence_links(
    transaction: &Transaction<'_>,
    links: &[EvidenceLink],
) -> Result<(), StoreError> {
    for item in links {
        transaction.execute(
            "INSERT OR IGNORE INTO event_evidence(event_id, observation_id, evidence_id)
             VALUES (?1, ?2, ?3)",
            params![item.event_id, item.observation_id, item.evidence_id],
        )?;
    }
    Ok(())
}

fn insert_content_refs(
    transaction: &Transaction<'_>,
    content_refs: &[ContentRefWrite],
) -> Result<(), StoreError> {
    for item in content_refs {
        let values = ContentValues::new(item)?;
        let changed = transaction.execute(
            "INSERT OR IGNORE INTO content_refs(
                 content_ref_id, project_id, content_hash, byte_length, media_type,
                 local_locator, redacted_excerpt, truncated, privacy, disclosure_class
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                values.content_ref_id,
                values.project_id,
                values.content_hash,
                values.byte_length,
                values.media_type,
                values.local_locator,
                values.redacted_excerpt,
                values.truncated,
                values.privacy,
                values.disclosure_class,
            ],
        )?;
        if changed == 0 && !content_exact(transaction, &values)? {
            return Err(StoreError::ImmutableConflict);
        }
    }
    Ok(())
}

struct ContentValues<'a> {
    content_ref_id: &'a str,
    project_id: &'a str,
    content_hash: &'a str,
    byte_length: i64,
    media_type: &'a str,
    local_locator: Option<Vec<u8>>,
    redacted_excerpt: Option<&'a str>,
    truncated: i64,
    privacy: &'static str,
    disclosure_class: &'static str,
}

impl<'a> ContentValues<'a> {
    fn new(item: &'a ContentRefWrite) -> Result<Self, StoreError> {
        Ok(Self {
            content_ref_id: &item.content_ref_id,
            project_id: item.project_id.as_str(),
            content_hash: item.content.hash(),
            byte_length: to_i64(item.content.byte_length())?,
            media_type: item.content.media_type(),
            local_locator: item
                .content
                .local_locator()
                .map(serde_json::to_vec)
                .transpose()?,
            redacted_excerpt: item.content.redacted_excerpt(),
            truncated: i64::from(item.content.is_truncated()),
            privacy: privacy(item.privacy),
            disclosure_class: disclosure(item.content.disclosure_class()),
        })
    }
}

fn content_exact(
    transaction: &Transaction<'_>,
    value: &ContentValues<'_>,
) -> Result<bool, StoreError> {
    exists(
        transaction,
        "SELECT EXISTS(
             SELECT 1 FROM content_refs
             WHERE content_ref_id = ?1 AND project_id = ?2 AND content_hash = ?3
               AND byte_length = ?4 AND media_type = ?5 AND local_locator IS ?6
               AND redacted_excerpt IS ?7 AND truncated = ?8 AND privacy = ?9
               AND disclosure_class = ?10
         )",
        params![
            value.content_ref_id,
            value.project_id,
            value.content_hash,
            value.byte_length,
            value.media_type,
            value.local_locator,
            value.redacted_excerpt,
            value.truncated,
            value.privacy,
            value.disclosure_class,
        ],
    )
}

fn upsert_schema_fingerprints(
    transaction: &Transaction<'_>,
    fingerprints: &[SchemaFingerprintUpdate],
) -> Result<(), StoreError> {
    for item in fingerprints {
        let observed_at = format_timestamp(item.observed_at)?;
        let current: Option<i64> = transaction
            .query_row(
                "SELECT count FROM schema_fingerprints
                 WHERE provider = ?1 AND format = ?2 AND fingerprint = ?3",
                params![item.provider, item.format, item.fingerprint],
                |row| row.get(0),
            )
            .optional()?;
        if let Some(current) = current {
            if current < 0 {
                return Err(StoreError::InvalidBatch);
            }
            let next = current.checked_add(1).ok_or(StoreError::InvalidBatch)?;
            transaction.execute(
                "UPDATE schema_fingerprints
                 SET last_seen_at = ?4, count = ?5
                 WHERE provider = ?1 AND format = ?2 AND fingerprint = ?3",
                params![
                    item.provider,
                    item.format,
                    item.fingerprint,
                    observed_at,
                    next
                ],
            )?;
        } else {
            transaction.execute(
                "INSERT INTO schema_fingerprints(
                     provider, format, fingerprint, first_seen_at, last_seen_at, count
                 ) VALUES (?1, ?2, ?3, ?4, ?4, 1)",
                params![item.provider, item.format, item.fingerprint, observed_at],
            )?;
        }
    }
    Ok(())
}

fn insert_faults(
    transaction: &Transaction<'_>,
    faults: &[IngestionFault],
) -> Result<(), StoreError> {
    for item in faults {
        let changed = transaction.execute(
            "INSERT OR IGNORE INTO ingestion_faults(
                 fault_id, source_id, generation, byte_start, byte_end,
                 class, bounded_detail, created_at
             ) VALUES (
                 ?1, ?2, ?3, ?4, ?5, ?6, ?7,
                 strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
             )",
            params![
                item.fault_id,
                item.source_id,
                to_i64(item.generation)?,
                to_i64(item.byte_start)?,
                to_i64(item.byte_end)?,
                item.class,
                item.bounded_detail,
            ],
        )?;
        if changed == 0 && !fault_exact(transaction, item)? {
            return Err(StoreError::ImmutableConflict);
        }
    }
    Ok(())
}

fn fault_exact(transaction: &Transaction<'_>, item: &IngestionFault) -> Result<bool, StoreError> {
    exists(
        transaction,
        "SELECT EXISTS(
             SELECT 1 FROM ingestion_faults
             WHERE fault_id = ?1 AND source_id = ?2 AND generation = ?3
               AND byte_start = ?4 AND byte_end = ?5 AND class = ?6
               AND bounded_detail = ?7
         )",
        params![
            item.fault_id,
            item.source_id,
            to_i64(item.generation)?,
            to_i64(item.byte_start)?,
            to_i64(item.byte_end)?,
            item.class,
            item.bounded_detail,
        ],
    )
}

fn verify_retry(transaction: &Transaction<'_>, chunk: &IngestionChunk) -> Result<(), StoreError> {
    for item in &chunk.observations {
        if !observation_exact(transaction, &ObservationValues::new(chunk, item)?)? {
            return Err(StoreError::ImmutableConflict);
        }
    }
    for item in &chunk.events {
        if !event_exact(transaction, &EventValues::new(item)?)? {
            return Err(StoreError::ImmutableConflict);
        }
    }
    for item in &chunk.evidence {
        if !evidence_exact(transaction, &EvidenceValues::new(item)?)? {
            return Err(StoreError::ImmutableConflict);
        }
    }
    for item in &chunk.evidence_links {
        if !exists(
            transaction,
            "SELECT EXISTS(
                 SELECT 1 FROM event_evidence
                 WHERE event_id = ?1 AND observation_id = ?2 AND evidence_id = ?3
             )",
            params![item.event_id, item.observation_id, item.evidence_id],
        )? {
            return Err(StoreError::ImmutableConflict);
        }
    }
    for item in &chunk.content_refs {
        if !content_exact(transaction, &ContentValues::new(item)?)? {
            return Err(StoreError::ImmutableConflict);
        }
    }
    for (index, item) in chunk.fingerprints.iter().enumerate() {
        if chunk.fingerprints[index + 1..].iter().any(|later| {
            later.provider == item.provider
                && later.format == item.format
                && later.fingerprint == item.fingerprint
        }) {
            continue;
        }
        if !exists(
            transaction,
            "SELECT EXISTS(
                 SELECT 1 FROM schema_fingerprints
                 WHERE provider = ?1 AND format = ?2 AND fingerprint = ?3
                   AND count > 0
             )",
            params![item.provider, item.format, item.fingerprint],
        )? {
            return Err(StoreError::ImmutableConflict);
        }
    }
    for item in &chunk.faults {
        if !fault_exact(transaction, item)? {
            return Err(StoreError::ImmutableConflict);
        }
    }
    Ok(())
}

fn exists<P: rusqlite::Params>(
    connection: &rusqlite::Connection,
    sql: &str,
    parameters: P,
) -> Result<bool, StoreError> {
    Ok(connection.query_row(sql, parameters, |row| row.get(0))?)
}

/// Computes the project-scoped stable ID for a retained content reference.
///
/// # Errors
///
/// Returns [`StoreError::InvalidBatch`] if length conversion or locator
/// serialization fails.
pub fn stable_content_ref_id(
    project_id: &ProjectId,
    content: &ContentRef,
) -> Result<String, StoreError> {
    let locator = serde_json::to_vec(&content.local_locator())?;
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"content_ref");
    hash_part(&mut hasher, project_id.as_str().as_bytes())?;
    hash_part(&mut hasher, content.hash().as_bytes())?;
    hash_part(&mut hasher, &locator)?;
    Ok(format!("cref_{}", &hasher.finalize().to_hex()[..24]))
}

fn hash_part(hasher: &mut blake3::Hasher, value: &[u8]) -> Result<(), StoreError> {
    let length = u64::try_from(value.len()).map_err(|_| StoreError::InvalidBatch)?;
    hasher.update(&length.to_le_bytes());
    hasher.update(value);
    Ok(())
}

fn add_len(total: &mut usize, value: usize) -> Result<(), StoreError> {
    *total = total.checked_add(value).ok_or(StoreError::InvalidBatch)?;
    Ok(())
}

fn validate_graph_identity(
    project_id: &ProjectId,
    session_id: &SessionId,
    event_id: &EventId,
) -> Result<(), StoreError> {
    validate_graph_event_identity(project_id, event_id)?;
    if !bounded_identifier(session_id.as_str()) {
        return Err(StoreError::InvalidBatch);
    }
    Ok(())
}

fn validate_graph_event_identity(
    project_id: &ProjectId,
    event_id: &EventId,
) -> Result<(), StoreError> {
    if !bounded_identifier(project_id.as_str()) || !bounded_identifier(event_id.as_str()) {
        return Err(StoreError::InvalidBatch);
    }
    Ok(())
}

fn graph_semantic_bytes(batch: &GraphWriteBatch) -> Result<usize, StoreError> {
    let mut total = size_of::<u64>()
        .checked_mul(2)
        .ok_or(StoreError::InvalidBatch)?;
    add_len(&mut total, batch.reducer_name.len())?;
    add_len(&mut total, batch.next_event_id.as_str().len())?;
    for row in &batch.runs {
        add_len(&mut total, row.run_id.len())?;
        add_len(&mut total, row.project_id.as_str().len())?;
        add_len(&mut total, row.session_id.as_str().len())?;
        add_len(&mut total, row.evidence_event_id.as_str().len())?;
        add_len(&mut total, format_timestamp(row.observed_at)?.len())?;
        add_len(&mut total, 3)?;
    }
    for row in &batch.contexts {
        add_len(&mut total, row.context_run_id.len())?;
        add_len(&mut total, row.project_id.as_str().len())?;
        add_len(&mut total, row.session_id.as_str().len())?;
        add_len(&mut total, row.evidence_event_id.as_str().len())?;
        add_len(&mut total, format_timestamp(row.observed_at)?.len())?;
        if let Some(branch_hash) = &row.branch_hash {
            add_len(&mut total, branch_hash.len())?;
        }
    }
    for row in &batch.actions {
        add_len(&mut total, row.project_id.as_str().len())?;
        add_len(&mut total, row.session_id.as_str().len())?;
        add_len(&mut total, row.native_action_id.len())?;
        add_len(&mut total, row.request_event_id.as_str().len())?;
        add_len(&mut total, row.tool_name.len())?;
        add_len(&mut total, row.input_hash.len())?;
        if let Some(command) = &row.redacted_command {
            add_len(&mut total, command.len())?;
        }
    }
    for row in &batch.artifacts {
        add_len(&mut total, row.artifact_id.len())?;
        add_len(&mut total, row.work_id.as_str().len())?;
        add_len(&mut total, row.project_id.as_str().len())?;
        add_len(&mut total, row.path_hash.len())?;
        add_len(&mut total, row.operation.len())?;
        add_len(&mut total, row.evidence_event_id.as_str().len())?;
        add_len(&mut total, format_timestamp(row.observed_at)?.len())?;
        if let Some(path) = &row.project_relative_path {
            add_len(&mut total, path.len())?;
        }
        if let Some(content_hash) = &row.content_hash {
            add_len(&mut total, content_hash.len())?;
        }
    }
    for row in &batch.observed_finishes {
        add_len(&mut total, row.project_id.as_str().len())?;
        add_len(&mut total, row.session_id.as_str().len())?;
        add_len(&mut total, row.native_action_id.len())?;
        add_len(&mut total, row.finish_event_id.as_str().len())?;
        add_len(&mut total, format_timestamp(row.observed_at)?.len())?;
        add_len(&mut total, 1)?;
    }
    for row in &batch.finishes {
        add_len(&mut total, row.verification_id.len())?;
        add_len(&mut total, row.project_id.as_str().len())?;
        add_len(&mut total, row.session_id.as_str().len())?;
        add_len(&mut total, row.native_action_id.len())?;
        add_len(&mut total, row.basis.len())?;
        add_len(&mut total, row.finish_event_id.as_str().len())?;
        add_len(&mut total, format_timestamp(row.observed_at)?.len())?;
        add_len(&mut total, 1)?;
    }
    Ok(total)
}

fn bounded_metadata(value: &str) -> bool {
    !value.is_empty() && value.len() <= 128
}

fn bounded_identifier(value: &str) -> bool {
    bounded_metadata(value) && value.bytes().all(|byte| byte.is_ascii_graphic())
}

fn to_i64(value: u64) -> Result<i64, StoreError> {
    i64::try_from(value).map_err(|_| StoreError::InvalidBatch)
}

fn format_timestamp(value: OffsetDateTime) -> Result<String, StoreError> {
    value.format(&Rfc3339).map_err(|_| StoreError::InvalidBatch)
}

fn owner_kind(owner: &EvidenceOwner) -> &'static str {
    match owner {
        EvidenceOwner::Event(_) => "event",
        EvidenceOwner::Work(_) => "work",
    }
}

fn owner_id(owner: &EvidenceOwner) -> &str {
    match owner {
        EvidenceOwner::Event(id) => id.as_str(),
        EvidenceOwner::Work(id) => id.as_str(),
    }
}

fn privacy(value: PrivacyLabel) -> &'static str {
    match value {
        PrivacyLabel::RestrictedLocal => "restricted_local",
        PrivacyLabel::PrivateLocal => "private_local",
        PrivacyLabel::DerivedLocal => "derived_local",
        PrivacyLabel::SyncEligible => "sync_eligible",
    }
}

fn disclosure(value: DisclosureClass) -> &'static str {
    match value {
        DisclosureClass::HumanIntent => "human_intent",
        DisclosureClass::AgentStatement => "agent_statement",
        DisclosureClass::ObservedState => "observed_state",
        DisclosureClass::ToolResult => "tool_result",
        DisclosureClass::Reasoning => "reasoning",
        DisclosureClass::SystemInstruction => "system_instruction",
        DisclosureClass::DeveloperInstruction => "developer_instruction",
        DisclosureClass::DerivedText => "derived_text",
    }
}

fn actor(value: agbox_core::Actor) -> &'static str {
    match value {
        agbox_core::Actor::Human => "human",
        agbox_core::Actor::Agent => "agent",
        agbox_core::Actor::Tool => "tool",
        agbox_core::Actor::System => "system",
    }
}

fn decode_status(value: agbox_core::DecodeStatus) -> &'static str {
    match value {
        agbox_core::DecodeStatus::Known => "known",
        agbox_core::DecodeStatus::UnknownType => "unknown_type",
        agbox_core::DecodeStatus::Malformed => "malformed",
        agbox_core::DecodeStatus::Oversized => "oversized",
    }
}

#[cfg(feature = "test-support")]
impl IngestionChunk {
    #[must_use]
    pub fn fixture(
        source_id: &str,
        generation: u64,
        expected_offset: u64,
        next_offset: u64,
        event_count: usize,
    ) -> Self {
        use agbox_core::{
            ActivityEventDraft, ByteRange, DecodeStatus, EventId, SemanticKey, SourceIdentity,
            SourceObservationDraft, SourceRef, SourceRefDraft,
        };

        let source = SourceRef::new(SourceRefDraft {
            provider: Provider::Codex,
            format: "jsonl".into(),
            native_session_id: "native-session-fixture".into(),
            native_record_type: "message".into(),
            native_record_id: Some("message-fixture".into()),
            source_generation: generation,
            byte_offset: expected_offset,
            ordinal: Some(1),
            record_hash: format!("b3:fixture-record-{expected_offset}-{next_offset}"),
            decoder_version: "fixture-v1".into(),
        })
        .unwrap_or_else(|_| unreachable!("fixed fixture source is valid"));
        let observation = SourceObservation::new(SourceObservationDraft {
            observation_id: format!("obs_{source_id}_{generation}_{expected_offset}_{next_offset}"),
            source: source.clone(),
            range: ByteRange::new(expected_offset, next_offset)
                .unwrap_or_else(|_| unreachable!("fixed fixture range is valid")),
            observed_at: OffsetDateTime::UNIX_EPOCH,
            status: DecodeStatus::Known,
            bounded_record: None,
            schema_fingerprint: "fixture-fingerprint".into(),
        })
        .unwrap_or_else(|_| unreachable!("fixed fixture observation is valid"));

        let events = (0..event_count)
            .map(|index| {
                let identity = SourceIdentity {
                    provider: Provider::Codex,
                    source_id: source_id.into(),
                    generation,
                    byte_offset: expected_offset,
                    record_hash: source.record_hash().into(),
                };
                let mut draft: ActivityEventDraft = ActivityEventV1::fixture_message_draft();
                draft.event_id = EventId::from_source(
                    &identity,
                    u32::try_from(index)
                        .unwrap_or_else(|_| unreachable!("fixture count is bounded")),
                );
                draft.semantic_key = SemanticKey::from_native(
                    Provider::Codex,
                    "native-session-fixture",
                    "message",
                    &format!("{expected_offset}-{next_offset}-{index}"),
                );
                draft.source = source.clone();
                ActivityEventV1::new(draft)
                    .unwrap_or_else(|_| unreachable!("fixed fixture event is valid"))
            })
            .collect();

        Self {
            expected_cursor: CursorState {
                source_id: source_id.into(),
                generation,
                offset: expected_offset,
                parser_state: Vec::new(),
            },
            next_cursor: CursorState {
                source_id: source_id.into(),
                generation,
                offset: next_offset,
                parser_state: Vec::new(),
            },
            observations: vec![observation],
            events,
            evidence: Vec::new(),
            evidence_links: Vec::new(),
            content_refs: Vec::new(),
            fingerprints: Vec::new(),
            faults: Vec::new(),
        }
    }
}
