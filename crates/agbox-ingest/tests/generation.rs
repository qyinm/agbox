#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::{
    io::{Read, Write},
    path::PathBuf,
};

use agbox_adapters::RecordSource;
use agbox_ingest::{
    GenerationError, RecordScanner, ScanOutcome, SourceSnapshot, reconcile_generation,
};
use time::OffsetDateTime;

#[test]
fn moves_keep_source_and_generation_while_truncation_and_replacement_increment() {
    let source = "source_11111111111111111111111111111111";
    let previous = SourceSnapshot::fixture(source, "unix:11:12", "/root/a.jsonl", 900, 3);
    let moved = SourceSnapshot::fixture(source, "unix:11:12", "/root/b.jsonl", 901, 1);
    let truncated = SourceSnapshot::fixture(source, "unix:11:12", "/root/a.jsonl", 100, 1);
    let replaced = SourceSnapshot::fixture(source, "unix:11:13", "/root/a.jsonl", 100, 1);

    let moved = reconcile_generation(&previous, &moved).unwrap();
    assert_eq!(moved.source_id, source);
    assert_eq!(moved.generation, 3);
    assert!(moved.moved);
    assert!(!moved.replaced);
    assert!(!moved.truncated);

    let truncated = reconcile_generation(&previous, &truncated).unwrap();
    assert_eq!(truncated.source_id, source);
    assert_eq!(truncated.generation, 4);
    assert!(truncated.truncated);

    let replaced = reconcile_generation(&previous, &replaced).unwrap();
    assert_eq!(replaced.source_id, source);
    assert_eq!(replaced.generation, 4);
    assert!(replaced.replaced);

    assert_eq!(previous.generation, 3);
    assert_eq!(previous.path, PathBuf::from("/root/a.jsonl"));
}

#[test]
fn generation_increment_never_wraps() {
    let source = "source_11111111111111111111111111111111";
    let previous = SourceSnapshot::fixture(source, "unix:11:12", "/root/a.jsonl", 900, u64::MAX);
    let truncated = SourceSnapshot::fixture(source, "unix:11:12", "/root/a.jsonl", 100, 1);
    assert_eq!(
        reconcile_generation(&previous, &truncated),
        Err(GenerationError::Overflow)
    );
}

#[test]
fn same_size_mtime_change_rolls_generation_but_append_does_not() {
    let source = "source_11111111111111111111111111111111";
    let previous = SourceSnapshot::fixture(source, "unix:11:12", "/root/a.jsonl", 900, 3)
        .with_mtime(OffsetDateTime::UNIX_EPOCH);
    let rewritten = SourceSnapshot::fixture(source, "unix:11:12", "/root/a.jsonl", 900, 1)
        .with_mtime(OffsetDateTime::UNIX_EPOCH + time::Duration::seconds(1));
    let appended = SourceSnapshot::fixture(source, "unix:11:12", "/root/a.jsonl", 901, 1)
        .with_mtime(OffsetDateTime::UNIX_EPOCH + time::Duration::seconds(1));

    let rewritten = reconcile_generation(&previous, &rewritten).unwrap();
    assert_eq!(rewritten.generation, 4);
    assert!(rewritten.modified);
    let appended = reconcile_generation(&previous, &appended).unwrap();
    assert_eq!(appended.generation, 3);
    assert!(!appended.modified);
}

#[test]
fn restored_mtime_cannot_hide_a_same_inode_rewrite_when_ctime_changes() {
    let previous = SourceSnapshot::fixture(
        "source_11111111111111111111111111111111",
        "unix:11:12",
        "/root/a.jsonl",
        900,
        3,
    )
    .with_mtime(OffsetDateTime::UNIX_EPOCH)
    .with_ctime(OffsetDateTime::UNIX_EPOCH);
    let rewritten = SourceSnapshot::fixture(
        "source_11111111111111111111111111111111",
        "unix:11:12",
        "/root/a.jsonl",
        900,
        1,
    )
    .with_mtime(OffsetDateTime::UNIX_EPOCH)
    .with_ctime(OffsetDateTime::UNIX_EPOCH + time::Duration::nanoseconds(1));

    let generation = reconcile_generation(&previous, &rewritten).unwrap();
    assert_eq!(generation.generation, 4);
    assert!(generation.modified);
}

#[test]
fn public_snapshot_and_generation_debug_do_not_disclose_paths_or_identifiers() {
    let snapshot = SourceSnapshot::fixture(
        "source_11111111111111111111111111111111",
        "unix:11:12",
        "/SECRET/path.jsonl",
        1,
        1,
    );
    let generation = reconcile_generation(&snapshot, &snapshot).unwrap();
    let snapshot_debug = format!("{snapshot:?}");
    let generation_debug = format!("{generation:?}");
    assert!(!snapshot_debug.contains("SECRET"));
    assert!(!generation_debug.contains("111111"));
}

#[test]
fn snapshot_rejects_path_like_identity_inputs() {
    assert_eq!(
        SourceSnapshot::new(
            "source_/secret".to_owned(),
            "unix:11:12".to_owned(),
            PathBuf::from("/root/a.jsonl"),
            1,
            1,
        ),
        Err(GenerationError::InvalidIdentity)
    );
    assert_eq!(
        SourceSnapshot::new(
            "source_11111111111111111111111111111111".to_owned(),
            "unix:11:../12".to_owned(),
            PathBuf::from("/root/a.jsonl"),
            1,
            1,
        ),
        Err(GenerationError::InvalidIdentity)
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
