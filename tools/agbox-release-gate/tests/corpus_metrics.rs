#![allow(clippy::unwrap_used)]

use agbox_release_gate::{
    corpus::{CorpusSpec, manifest, manifest_hash},
    metrics::{Samples, sustained_rss_growth},
};

#[test]
fn release_manifest_is_deterministic_and_distinguishes_sparse_logical_size() {
    let first = manifest(&CorpusSpec::release());
    let second = manifest(&CorpusSpec::release());
    assert_eq!(first, second);
    assert_eq!(first.sources.len(), 2_560);
    assert_eq!(first.logical_bytes, 5 * 1024 * 1024 * 1024);
    assert!(first.physical_bytes < first.logical_bytes);
    assert_eq!(first.hash, manifest_hash(&first));
    assert!(
        first
            .sources
            .iter()
            .any(|source| source.policy == "undated_eof")
    );
}

#[test]
fn samples_are_bounded_and_percentiles_do_not_need_payloads() {
    let mut samples = Samples::new(3);
    for value in [11, 21, 31, 41] {
        samples.record(value);
    }
    assert_eq!(samples.dropped(), 1);
    assert_eq!(samples.percentile(95, 100), Some(31));
}

#[test]
fn growth_requires_a_full_window_and_detects_a_large_final_shift() {
    assert!(sustained_rss_growth(&[1; 100]));
    let mut stable = vec![100 * 1024 * 1024; 12 * 3_600];
    assert!(!sustained_rss_growth(&stable));
    for sample in &mut stable[6 * 3_600..] {
        *sample += 17 * 1024 * 1024;
    }
    assert!(sustained_rss_growth(&stable));
}
