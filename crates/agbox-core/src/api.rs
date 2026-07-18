//! Stable, wire-safe application request and response bodies.
//!
//! These types deliberately contain no ambient authority.  In particular a
//! request cannot name a project or an actor: those belong to the verified
//! transport scope owned by the application service.

use serde::{Deserialize, Serialize};

use crate::{ContractId, EvidenceId, WorkId, WorkStatus};

#[derive(Clone, Deserialize, Serialize)]
pub enum AppRequest {
    ListWork {
        status: Option<WorkStatus>,
        limit: u16,
    },
    CurrentWork,
    GetWork {
        work_id: WorkId,
    },
    GetEvidence {
        evidence_id: EvidenceId,
        disclosure: EvidenceDisclosure,
    },
    SearchWork {
        query: String,
        limit: u16,
    },
    CorrectWork {
        work_id: WorkId,
        field: CorrectableField,
        value: String,
    },
    ForgetWork {
        work_id: WorkId,
    },
    ForgetProject,
    Health,
}

#[derive(Clone, Deserialize, Serialize)]
pub enum AppResponse {
    WorkList(BoundedPage<WorkSummary>),
    Work(Box<WorkDetail>),
    Evidence(EvidenceView),
    Search(BoundedPage<SearchHit>),
    Health(HealthSnapshot),
    Accepted,
    NotFound,
}

#[derive(Clone, Deserialize, Serialize)]
pub struct BoundedPage<T> {
    pub items: Vec<T>,
    pub truncated: bool,
}

impl<T> std::fmt::Debug for BoundedPage<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BoundedPage")
            .field("items", &self.items.len())
            .field("truncated", &self.truncated)
            .finish()
    }
}

#[derive(Clone, Deserialize, Serialize)]
pub struct WorkSummary {
    pub work_id: WorkId,
    pub contract_id: ContractId,
    pub revision: u64,
    pub status: WorkStatus,
    pub objective: Option<String>,
    pub summary: String,
}

#[derive(Clone, Deserialize, Serialize)]
pub struct WorkDetail {
    pub work_id: WorkId,
    pub contract_id: ContractId,
    pub revision: u64,
    pub status: WorkStatus,
    pub objective: Option<String>,
    pub summary: String,
    pub completed_steps: Vec<String>,
    pub next_actions: Vec<String>,
    pub blockers: Vec<String>,
    pub constraints: Vec<String>,
    pub completion_criteria: Vec<String>,
    pub artifacts: Vec<String>,
    pub verification: Vec<String>,
}

#[derive(Clone, Deserialize, Serialize)]
pub struct SearchHit {
    pub work: WorkSummary,
}

#[derive(Clone, Deserialize, Serialize)]
pub struct EvidenceView {
    pub evidence_id: EvidenceId,
    pub media_type: String,
    pub untrusted_data: bool,
    pub availability: EvidenceAvailability,
    pub redacted_preview: String,
    pub raw: Option<Vec<u8>>,
}

#[derive(Clone, Copy, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceAvailability {
    Available,
    Expired,
    DeletePending,
}

#[derive(Clone, Copy, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceDisclosure {
    Redacted,
    AuthorizedRaw,
}

#[derive(Clone, Copy, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CorrectableField {
    Objective,
    Summary,
    NextAction,
    Blocker,
    Constraint,
    CompletionCriterion,
}

#[derive(Clone, Deserialize, Serialize)]
pub struct HealthSnapshot {
    pub ready: bool,
}

impl std::fmt::Debug for AppRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ListWork { status, limit } => f
                .debug_struct("ListWork")
                .field("status", status)
                .field("limit", limit)
                .finish(),
            Self::CurrentWork => f.write_str("CurrentWork"),
            Self::GetWork { work_id } => {
                f.debug_struct("GetWork").field("work_id", work_id).finish()
            }
            Self::GetEvidence {
                evidence_id,
                disclosure,
            } => f
                .debug_struct("GetEvidence")
                .field("evidence_id", evidence_id)
                .field("disclosure", disclosure)
                .finish(),
            Self::SearchWork { query, limit } => f
                .debug_struct("SearchWork")
                .field("query_bytes", &query.len())
                .field("limit", limit)
                .finish(),
            Self::CorrectWork {
                work_id,
                field,
                value,
            } => f
                .debug_struct("CorrectWork")
                .field("work_id", work_id)
                .field("field", field)
                .field("value_bytes", &value.len())
                .finish(),
            Self::ForgetWork { work_id } => f
                .debug_struct("ForgetWork")
                .field("work_id", work_id)
                .finish(),
            Self::ForgetProject => f.write_str("ForgetProject"),
            Self::Health => f.write_str("Health"),
        }
    }
}

impl std::fmt::Debug for AppResponse {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::WorkList(page) => f
                .debug_struct("WorkList")
                .field("items", &page.items.len())
                .field("truncated", &page.truncated)
                .finish(),
            Self::Work(_) => f.write_str("Work"),
            Self::Evidence(view) => f.debug_tuple("Evidence").field(view).finish(),
            Self::Search(page) => f
                .debug_struct("Search")
                .field("items", &page.items.len())
                .field("truncated", &page.truncated)
                .finish(),
            Self::Health(_) => f.write_str("Health"),
            Self::Accepted => f.write_str("Accepted"),
            Self::NotFound => f.write_str("NotFound"),
        }
    }
}

impl std::fmt::Debug for EvidenceView {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EvidenceView")
            .field("evidence_id", &self.evidence_id)
            .field("media_type", &self.media_type)
            .field("untrusted_data", &self.untrusted_data)
            .field("availability", &self.availability)
            .field("preview_bytes", &self.redacted_preview.len())
            .field("raw_bytes", &self.raw.as_ref().map_or(0, Vec::len))
            .finish()
    }
}

impl std::fmt::Debug for EvidenceAvailability {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Available => "Available",
            Self::Expired => "Expired",
            Self::DeletePending => "DeletePending",
        })
    }
}
impl std::fmt::Debug for EvidenceDisclosure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Redacted => "Redacted",
            Self::AuthorizedRaw => "AuthorizedRaw",
        })
    }
}
impl std::fmt::Debug for CorrectableField {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("CorrectableField")
    }
}
impl std::fmt::Debug for WorkSummary {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WorkSummary")
            .field("work_id", &self.work_id)
            .field("contract_id", &self.contract_id)
            .field("revision", &self.revision)
            .field("status", &self.status)
            .field(
                "objective_bytes",
                &self.objective.as_ref().map_or(0, String::len),
            )
            .field("summary_bytes", &self.summary.len())
            .finish()
    }
}
impl std::fmt::Debug for WorkDetail {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WorkDetail")
            .field("work_id", &self.work_id)
            .field("contract_id", &self.contract_id)
            .field("revision", &self.revision)
            .field("status", &self.status)
            .finish_non_exhaustive()
    }
}
impl std::fmt::Debug for SearchHit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SearchHit")
            .field("work", &self.work)
            .finish()
    }
}
impl std::fmt::Debug for HealthSnapshot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HealthSnapshot")
            .field("ready", &self.ready)
            .finish()
    }
}
