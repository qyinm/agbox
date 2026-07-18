//! Store-owned retention and explicit forget value types.

use agbox_core::{ProjectId, WorkId};

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ForgetTarget {
    Work(WorkId),
    Project(ProjectId),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ForgetOutcome {
    pub deletion_job_id: String,
    pub deleted_rows: u64,
    pub pending_blobs: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RetentionTick {
    pub attempted: u64,
    pub deleted: u64,
    pub failed: u64,
}
