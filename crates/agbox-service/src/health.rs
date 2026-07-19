//! Fixed-memory daemon health accounting.

use std::time::Duration;

use hdrhistogram::Histogram;
use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System};

const MAX_LATENCY_MICROS: u64 = 60_000_000;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LatencyPercentiles {
    pub p50_micros: u64,
    pub p95_micros: u64,
    pub p99_micros: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DaemonHealthSnapshot {
    pub queue_depth: u32,
    pub queue_capacity: u32,
    pub decode: LatencyPercentiles,
    pub commit: LatencyPercentiles,
    pub ipc: LatencyPercentiles,
    pub resident_bytes: u64,
    pub dropped_logs: u64,
}

#[derive(Debug)]
pub struct DaemonHealth {
    queue_depth: u32,
    queue_capacity: u32,
    decode: Histogram<u64>,
    commit: Histogram<u64>,
    ipc: Histogram<u64>,
    dropped_logs: u64,
}

/// Process-only RSS sampler. It intentionally never enumerates all processes.
#[derive(Debug)]
pub struct ProcessMemorySampler {
    system: System,
    pid: Pid,
}

impl ProcessMemorySampler {
    #[must_use]
    pub fn current_process() -> Self {
        Self {
            system: System::new(),
            pid: Pid::from_u32(std::process::id()),
        }
    }

    #[must_use]
    pub fn resident_bytes(&mut self) -> u64 {
        self.system.refresh_processes_specifics(
            ProcessesToUpdate::Some(&[self.pid]),
            true,
            ProcessRefreshKind::nothing().with_memory(),
        );
        self.system
            .process(self.pid)
            .map_or(0, sysinfo::Process::memory)
    }
}

impl DaemonHealth {
    #[must_use]
    pub fn new(queue_capacity: u32) -> Self {
        Self {
            queue_depth: 0,
            queue_capacity,
            decode: histogram(),
            commit: histogram(),
            ipc: histogram(),
            dropped_logs: 0,
        }
    }

    pub fn set_queue_depth(&mut self, depth: u32) {
        self.queue_depth = depth.min(self.queue_capacity);
    }

    pub fn observe_decode(&mut self, latency: Duration) {
        observe(&mut self.decode, latency);
    }
    pub fn observe_commit(&mut self, latency: Duration) {
        observe(&mut self.commit, latency);
    }
    pub fn observe_ipc(&mut self, latency: Duration) {
        observe(&mut self.ipc, latency);
    }
    pub fn increment_dropped_logs(&mut self) {
        self.dropped_logs = self.dropped_logs.saturating_add(1);
    }

    #[must_use]
    pub fn snapshot(&self, resident_bytes: u64) -> DaemonHealthSnapshot {
        DaemonHealthSnapshot {
            queue_depth: self.queue_depth,
            queue_capacity: self.queue_capacity,
            decode: percentiles(&self.decode),
            commit: percentiles(&self.commit),
            ipc: percentiles(&self.ipc),
            resident_bytes,
            dropped_logs: self.dropped_logs,
        }
    }
}

fn histogram() -> Histogram<u64> {
    match Histogram::new_with_bounds(1, MAX_LATENCY_MICROS, 3) {
        Ok(histogram) => histogram,
        Err(_) => unreachable!("constant histogram bounds are valid"),
    }
}

fn observe(histogram: &mut Histogram<u64>, latency: Duration) {
    let micros = u64::try_from(latency.as_micros())
        .unwrap_or(MAX_LATENCY_MICROS)
        .clamp(1, MAX_LATENCY_MICROS);
    let _ = histogram.record(micros);
}

fn percentiles(histogram: &Histogram<u64>) -> LatencyPercentiles {
    LatencyPercentiles {
        p50_micros: histogram.value_at_quantile(0.50),
        p95_micros: histogram.value_at_quantile(0.95),
        p99_micros: histogram.value_at_quantile(0.99),
    }
}
