use std::time::Duration;

use time::OffsetDateTime;

pub const HISTORY_DAYS: i64 = 90;
const FUTURE_SKEW_DAYS: i64 = 1;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HistoryDecision {
    ReplayFrom(u64),
    BaselineAt(u64),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct HistoryPolicy;

impl HistoryPolicy {
    #[must_use]
    pub const fn new(_retention: Duration) -> Self {
        Self
    }

    #[must_use]
    pub fn decide(
        self,
        session_time: Option<OffsetDateTime>,
        now: OffsetDateTime,
        file_size: u64,
    ) -> HistoryDecision {
        let Some(oldest) = now.checked_sub(time::Duration::days(HISTORY_DAYS)) else {
            return HistoryDecision::BaselineAt(file_size);
        };
        let Some(latest) = now.checked_add(time::Duration::days(FUTURE_SKEW_DAYS)) else {
            return HistoryDecision::BaselineAt(file_size);
        };
        match session_time {
            Some(value) if value >= oldest && value <= latest => HistoryDecision::ReplayFrom(0),
            _ => HistoryDecision::BaselineAt(file_size),
        }
    }
}

impl Default for HistoryPolicy {
    fn default() -> Self {
        Self
    }
}
