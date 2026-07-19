//! Machine-readable release threshold evaluation.

pub mod corpus;
pub mod metrics;
pub mod recovery;

use serde::{Deserialize, Serialize};

/// Immutable performance and recovery thresholds for a release candidate.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Thresholds {
    pub logical_corpus_bytes: u64,
    pub minimum_sources: usize,
    pub append_records_per_second: u32,
    pub append_duration_seconds: u32,
    pub minimum_visible_records: usize,
    pub ingestion_p95_ms: u64,
    pub ingestion_p99_ms: u64,
    pub peak_rss_bytes: u64,
    pub eof_probe_bytes_read: u64,
    pub mcp_current_work_p95_ms: u64,
}

impl Thresholds {
    #[must_use]
    pub const fn release() -> Self {
        Self {
            logical_corpus_bytes: 5 * 1024 * 1024 * 1024,
            minimum_sources: 2_500,
            append_records_per_second: 50,
            append_duration_seconds: 60,
            minimum_visible_records: 3_000,
            ingestion_p95_ms: 100,
            ingestion_p99_ms: 200,
            peak_rss_bytes: 256 * 1024 * 1024,
            eof_probe_bytes_read: 0,
            mcp_current_work_p95_ms: 200,
        }
    }
}

/// Measurements emitted by a gate run; all values are bounded scalar summaries.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct GateReport {
    pub logical_corpus_bytes: u64,
    pub sources: usize,
    pub visible_records: usize,
    pub ingestion_p95_ms: u64,
    pub ingestion_p99_ms: u64,
    pub peak_rss_bytes: u64,
    pub eof_probe_bytes_read: u64,
    pub mcp_current_work_p95_ms: u64,
    pub exact_recovery: bool,
    pub sustained_rss_growth: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct GateEvaluation {
    pub passed: bool,
    pub failures: Vec<String>,
}

/// Candidate-bound report consumed by the cutover guard. A report that lacks
/// any one of these immutable bindings is never eligible for Go removal.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ReleaseArtifact {
    pub schema_version: u16,
    pub profile: String,
    pub duration_seconds: u64,
    pub commit_sha: String,
    pub target: String,
    pub binary_sha256: String,
    pub corpus_manifest_hash: String,
    pub thresholds: Thresholds,
    pub report: GateReport,
    pub evaluation: GateEvaluation,
}

impl ReleaseArtifact {
    /// Validates the non-negotiable Task 27 predecessor contract.
    ///
    /// # Errors
    ///
    /// Returns a bounded machine-readable reason if this report cannot prove
    /// the exact candidate is eligible for cutover.
    pub fn verify_for_cutover(
        &self,
        current_commit: &str,
        expected_binary_sha256: &str,
    ) -> Result<(), &'static str> {
        if self.schema_version != 1 {
            return Err("schema_version");
        }
        if self.profile != "release" || self.duration_seconds != 24 * 60 * 60 {
            return Err("duration");
        }
        if self.commit_sha != current_commit || self.binary_sha256 != expected_binary_sha256 {
            return Err("candidate_binding");
        }
        if self.target != "aarch64-apple-darwin" {
            return Err("target");
        }
        if self.thresholds != Thresholds::release() {
            return Err("thresholds");
        }
        if !self.evaluation.passed || !self.report.evaluate(&self.thresholds).passed {
            return Err("failed_measurement");
        }
        if self.corpus_manifest_hash.len() != 64 {
            return Err("corpus_manifest_hash");
        }
        Ok(())
    }
}

impl GateReport {
    #[must_use]
    pub const fn fixture_with_peak_rss(peak_rss_bytes: u64) -> Self {
        Self {
            logical_corpus_bytes: 5 * 1024 * 1024 * 1024,
            sources: 2_500,
            visible_records: 3_000,
            ingestion_p95_ms: 99,
            ingestion_p99_ms: 199,
            peak_rss_bytes,
            eof_probe_bytes_read: 0,
            mcp_current_work_p95_ms: 199,
            exact_recovery: true,
            sustained_rss_growth: false,
        }
    }
    #[must_use]
    pub fn evaluate(&self, thresholds: &Thresholds) -> GateEvaluation {
        let mut failures = Vec::new();
        if self.logical_corpus_bytes < thresholds.logical_corpus_bytes {
            failures.push("logical_corpus_bytes".into());
        }
        if self.sources < thresholds.minimum_sources {
            failures.push("sources".into());
        }
        if self.visible_records < thresholds.minimum_visible_records {
            failures.push("visible_records".into());
        }
        if self.ingestion_p95_ms >= thresholds.ingestion_p95_ms {
            failures.push("ingestion_p95_ms".into());
        }
        if self.ingestion_p99_ms >= thresholds.ingestion_p99_ms {
            failures.push("ingestion_p99_ms".into());
        }
        if self.peak_rss_bytes >= thresholds.peak_rss_bytes {
            failures.push("peak_rss_bytes".into());
        }
        if self.eof_probe_bytes_read != thresholds.eof_probe_bytes_read {
            failures.push("eof_probe_bytes_read".into());
        }
        if self.mcp_current_work_p95_ms >= thresholds.mcp_current_work_p95_ms {
            failures.push("mcp_current_work_p95_ms".into());
        }
        if !self.exact_recovery {
            failures.push("exact_recovery".into());
        }
        if self.sustained_rss_growth {
            failures.push("sustained_rss_growth".into());
        }
        GateEvaluation {
            passed: failures.is_empty(),
            failures,
        }
    }
}
