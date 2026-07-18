//! Typed, deliberately small audit records.

use agbox_core::{ProjectId, WorkId};
use time::OffsetDateTime;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AuditRecord {
    pub kind: &'static str,
    pub project_id: ProjectId,
    pub work_id: Option<WorkId>,
    pub actor: &'static str,
    pub result: &'static str,
    pub observed_at: OffsetDateTime,
}
