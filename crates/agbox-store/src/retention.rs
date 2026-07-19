//! Store-owned retention and explicit forget value types.

use agbox_core::{ProjectId, WorkId};

/// Runtime policy supplied to the bounded maintenance hook.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RetentionConfig {
    pub evidence_retention_days: u32,
}

impl RetentionConfig {
    /// Creates a policy, rejecting an unbounded zero-day window.
    #[must_use]
    pub const fn new(evidence_retention_days: u32) -> Option<Self> {
        if evidence_retention_days == 0 {
            None
        } else {
            Some(Self {
                evidence_retention_days,
            })
        }
    }
}

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
