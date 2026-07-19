//! Bounded percentile and long-run RSS evaluation helpers.

/// Fixed-capacity latency samples. Callers retain only scalar durations, never
/// query payloads or source content.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Samples {
    values: Vec<u64>,
    capacity: usize,
    dropped: u64,
}

impl Samples {
    #[must_use]
    pub fn new(capacity: usize) -> Self {
        Self {
            values: Vec::with_capacity(capacity),
            capacity,
            dropped: 0,
        }
    }

    pub fn record(&mut self, value: u64) {
        if self.values.len() == self.capacity {
            self.dropped = self.dropped.saturating_add(1);
        } else {
            self.values.push(value);
        }
    }

    #[must_use]
    pub fn percentile(&self, numerator: usize, denominator: usize) -> Option<u64> {
        if self.values.is_empty() || denominator == 0 || numerator > denominator {
            return None;
        }
        let mut sorted = self.values.clone();
        sorted.sort_unstable();
        let index = sorted
            .len()
            .saturating_mul(numerator)
            .div_ceil(denominator)
            .saturating_sub(1)
            .min(sorted.len() - 1);
        sorted.get(index).copied()
    }

    #[must_use]
    pub const fn dropped(&self) -> u64 {
        self.dropped
    }
}

/// Detects the approved sustained-growth condition from one-second RSS samples.
/// It requires a full 12-hour window. Growth is only reported when both the
/// first/final six-hour median gap and a robust post-warmup Theil-Sen slope
/// exceed their approved limits.
#[must_use]
pub fn sustained_rss_growth(samples: &[u64]) -> bool {
    const HOUR: usize = 3_600;
    if samples.len() < 12 * HOUR {
        return true;
    }
    let first = median(&samples[..6 * HOUR]);
    let final_median = median(&samples[samples.len() - 6 * HOUR..]);
    let exceeds_median_gap = final_median > first.saturating_add(16 * 1024 * 1024);
    exceeds_median_gap && robust_slope_bytes_per_hour(&samples[HOUR..]) > 1024 * 1024
}

/// Computes a bounded Theil-Sen slope from one-hour RSS medians after warmup.
#[must_use]
pub fn robust_slope_bytes_per_hour(samples_after_warmup: &[u64]) -> u64 {
    const HOUR: usize = 3_600;
    let hourly = samples_after_warmup
        .chunks_exact(HOUR)
        .map(median)
        .collect::<Vec<_>>();
    if hourly.len() < 2 {
        return 0;
    }
    let mut positive_slopes = Vec::with_capacity(hourly.len().saturating_mul(hourly.len()));
    for (start, value) in hourly.iter().copied().enumerate() {
        for (end, later) in hourly.iter().copied().enumerate().skip(start + 1) {
            if later > value {
                positive_slopes.push(
                    later
                        .saturating_sub(value)
                        .checked_div(u64::try_from(end - start).unwrap_or(u64::MAX))
                        .unwrap_or(0),
                );
            }
        }
    }
    if positive_slopes.is_empty() {
        return 0;
    }
    median(&positive_slopes)
}

fn median(values: &[u64]) -> u64 {
    let mut sorted = values.to_vec();
    sorted.sort_unstable();
    sorted[sorted.len() / 2]
}
