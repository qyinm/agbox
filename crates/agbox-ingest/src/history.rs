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
        match session_time {
            Some(value)
                if value >= now - time::Duration::days(HISTORY_DAYS)
                    && value <= now + time::Duration::days(FUTURE_SKEW_DAYS) =>
            {
                HistoryDecision::ReplayFrom(0)
            }
            _ => HistoryDecision::BaselineAt(file_size),
        }
    }
}

impl Default for HistoryPolicy {
    fn default() -> Self {
        Self
    }
}
