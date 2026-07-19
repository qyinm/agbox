//! Exact-count recovery verdicts retained by the release artifact.

use serde::{Deserialize, Serialize};

/// Count-only crash/restart result; it never embeds events or evidence.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct RecoveryCounts {
    pub expected_events: u64,
    pub observed_events: u64,
    pub expected_cursors: u64,
    pub observed_cursors: u64,
}

impl RecoveryCounts {
    #[must_use]
    pub const fn exact(self) -> bool {
        self.expected_events == self.observed_events
            && self.expected_cursors == self.observed_cursors
    }
}
