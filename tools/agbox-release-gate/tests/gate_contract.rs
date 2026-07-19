#![allow(clippy::unwrap_used)]
use agbox_release_gate::{GateReport, Thresholds};
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
