#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::{
    io::{Read, Write},
    path::PathBuf,
};

use agbox_adapters::RecordSource;
use agbox_ingest::{
    GenerationError, RecordScanner, ScanOutcome, SourceSnapshot, reconcile_generation,
};

#[test]
fn moves_keep_source_and_generation_while_truncation_and_replacement_increment() {
    let previous = SourceSnapshot::fixture("source_a", "unix:11:12", "/root/a.jsonl", 900, 3);
    let moved = SourceSnapshot::fixture("ignored", "unix:11:12", "/root/b.jsonl", 901, 1);
    let truncated = SourceSnapshot::fixture("ignored", "unix:11:12", "/root/a.jsonl", 100, 1);
    let replaced = SourceSnapshot::fixture("ignored", "unix:11:13", "/root/a.jsonl", 100, 1);

    let moved = reconcile_generation(&previous, &moved).unwrap();
    assert_eq!(moved.source_id, "source_a");
    assert_eq!(moved.generation, 3);
    assert!(moved.moved);
    assert!(!moved.replaced);
    assert!(!moved.truncated);

    let truncated = reconcile_generation(&previous, &truncated).unwrap();
    assert_eq!(truncated.source_id, "source_a");
    assert_eq!(truncated.generation, 4);
    assert!(truncated.truncated);

    let replaced = reconcile_generation(&previous, &replaced).unwrap();
    assert_eq!(replaced.source_id, "source_a");
    assert_eq!(replaced.generation, 4);
    assert!(replaced.replaced);

    assert_eq!(previous.generation, 3);
    assert_eq!(previous.path, PathBuf::from("/root/a.jsonl"));
}

#[test]
fn generation_increment_never_wraps() {
    let previous =
        SourceSnapshot::fixture("source_a", "unix:11:12", "/root/a.jsonl", 900, u64::MAX);
    let truncated = SourceSnapshot::fixture("ignored", "unix:11:12", "/root/a.jsonl", 100, 1);
    assert_eq!(
        reconcile_generation(&previous, &truncated),
        Err(GenerationError::Overflow)
    );
}

#[test]
fn record_window_still_implements_adapter_record_source_and_checks_terminal_integrity() {
    fn assert_record_source(_: &dyn RecordSource) {}

    let mut file = tempfile::tempfile().unwrap();
    file.write_all(b"original\n").unwrap();
    let mutator = file.try_clone().unwrap();
    let mut scanner = RecordScanner::new(file, 0, 9).unwrap();
    let ScanOutcome::Complete(window) = scanner.next().unwrap() else {
        panic!("expected record window");
    };
    assert_record_source(&window);
    assert_eq!(RecordSource::start(&window), 0);
    assert_eq!(RecordSource::end(&window), 8);

    std::os::unix::fs::FileExt::write_at(&mutator, b"mutated!", 0).unwrap();
    let error = RecordSource::open(&window)
        .unwrap()
        .read_to_end(&mut Vec::new())
        .unwrap_err();
    assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
}
