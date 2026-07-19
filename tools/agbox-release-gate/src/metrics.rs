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
/// It requires a full 12-hour window and is intentionally conservative.
#[must_use]
pub fn sustained_rss_growth(samples: &[u64]) -> bool {
    const HOUR: usize = 3_600;
    if samples.len() < 12 * HOUR {
        return true;
    }
    let first = median(&samples[..6 * HOUR]);
    let final_median = median(&samples[samples.len() - 6 * HOUR..]);
    final_median > first.saturating_add(16 * 1024 * 1024)
}

fn median(values: &[u64]) -> u64 {
    let mut sorted = values.to_vec();
    sorted.sort_unstable();
    sorted[sorted.len() / 2]
}
