#![allow(clippy::unwrap_used)]
use agbox_release_gate::{
    GateReport, ReleaseArtifact, Thresholds,
    corpus::{CorpusSpec, manifest},
};
#[test]
fn release_thresholds_match_the_approved_spec() {
    let thresholds = Thresholds::release();
    assert_eq!(thresholds.logical_corpus_bytes, 5 * 1024 * 1024 * 1024);
    assert!(thresholds.minimum_sources >= 2_500);
    assert_eq!(thresholds.append_records_per_second, 50);
    assert_eq!(thresholds.append_duration_seconds, 60);
    assert!(thresholds.minimum_visible_records >= 3_000);
    assert_eq!(thresholds.ingestion_p95_ms, 100);
    assert_eq!(thresholds.ingestion_p99_ms, 200);
    assert_eq!(thresholds.peak_rss_bytes, 256 * 1024 * 1024);
    assert_eq!(thresholds.eof_probe_bytes_read, 0);
    assert_eq!(thresholds.mcp_current_work_p95_ms, 200);
}
#[test]
fn any_failed_measurement_fails_the_report() {
    let report = GateReport::fixture_with_peak_rss(256 * 1024 * 1024 + 1);
    assert!(!report.evaluate(&Thresholds::release()).passed);
}

#[test]
fn release_artifact_rejects_short_or_unbound_reports() {
    let corpus = manifest(&CorpusSpec::release());
    let report = GateReport::fixture_with_peak_rss(1);
    let evaluation = report.evaluate(&Thresholds::release());
    let artifact = ReleaseArtifact {
        schema_version: 1,
        profile: "release".into(),
        duration_seconds: 600,
        commit_sha: "candidate".into(),
        target: "aarch64-apple-darwin".into(),
        binary_sha256: "binary".into(),
        corpus_manifest_hash: corpus.hash,
        thresholds: Thresholds::release(),
        report,
        evaluation,
    };
    assert_eq!(
        artifact.verify_for_cutover("candidate", "binary"),
        Err("duration")
    );
}
