#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::io::{Read, Seek, SeekFrom, Write};

use agbox_ingest::{READ_BUFFER_BYTES, RecordScanner, ScanOutcome};
use proptest::prelude::*;

fn scanner_for(bytes: &[u8], cursor: u64, target_size: u64) -> RecordScanner {
    let mut file = tempfile::tempfile().unwrap();
    file.write_all(bytes).unwrap();
    RecordScanner::new(file, cursor, target_size).unwrap()
}

fn complete(outcome: ScanOutcome) -> agbox_ingest::RecordWindow {
    match outcome {
        ScanOutcome::Complete(record) => record,
        other => panic!("expected complete record, got {other:?}"),
    }
}

fn read_window(record: &agbox_ingest::RecordWindow) -> Vec<u8> {
    let mut bytes = Vec::new();
    record.open().unwrap().read_to_end(&mut bytes).unwrap();
    bytes
}

#[test]
fn scanner_does_not_read_content_when_cursor_is_at_eof() {
    for cursor_past_end in [false, true] {
        let file = tempfile::tempfile().unwrap();
        file.set_len(838 * 1024 * 1024).unwrap();
        let size = file.metadata().unwrap().len();
        let cursor = size + u64::from(cursor_past_end);
        let mut scanner = RecordScanner::new(file, cursor, size).unwrap();

        assert!(matches!(scanner.next().unwrap(), ScanOutcome::Eof));
        assert_eq!(scanner.bytes_read(), 0);
        assert_eq!(scanner.buffer_capacity(), READ_BUFFER_BYTES);
    }
}

#[test]
fn scanner_frames_a_large_record_without_growing_its_buffer() {
    let mut file = tempfile::tempfile().unwrap();
    file.write_all(br#"{"type":"ignored","payload":""#).unwrap();
    let block = vec![b'x'; READ_BUFFER_BYTES];
    for _ in 0..512 {
        file.write_all(&block).unwrap();
    }
    file.write_all(b"\"}\n").unwrap();
    let size = file.seek(SeekFrom::End(0)).unwrap();
    let mut scanner = RecordScanner::new(file, 0, size).unwrap();

    let record = complete(scanner.next().unwrap());
    assert_eq!(record.start(), 0);
    assert_eq!(record.content_end(), size - 1);
    assert_eq!(record.next_offset(), size);
    assert!(record.content_length() > 32 * 1024 * 1024);
    assert_eq!(scanner.buffer_capacity(), READ_BUFFER_BYTES);
}

#[test]
fn repeated_next_on_an_incomplete_line_restores_the_logical_cursor() {
    let bytes = br#"{"type":"assistant""#;
    let mut scanner = scanner_for(bytes, 0, u64::try_from(bytes.len()).unwrap());

    for _ in 0..3 {
        assert!(matches!(
            scanner.next().unwrap(),
            ScanOutcome::Incomplete { retry_from: 0 }
        ));
    }
    assert_eq!(
        scanner.bytes_read(),
        u64::try_from(bytes.len() * 3).unwrap()
    );
}

#[test]
fn reading_a_prior_window_does_not_move_the_scanner() {
    let bytes = b"first\nsecond\n";
    let mut scanner = scanner_for(bytes, 0, u64::try_from(bytes.len()).unwrap());
    let first = complete(scanner.next().unwrap());

    let mut partial = first.open().unwrap();
    let mut prefix = [0_u8; 2];
    partial.read_exact(&mut prefix).unwrap();
    assert_eq!(&prefix, b"fi");

    assert_eq!(read_window(&first), b"first");

    let second = complete(scanner.next().unwrap());
    assert_eq!(read_window(&second), b"second");
    assert!(matches!(scanner.next().unwrap(), ScanOutcome::Eof));
}

#[test]
fn two_windows_can_be_read_interleaved_without_affecting_each_other_or_the_scanner() {
    let bytes = b"alpha\nbeta\ngamma\n";
    let mut scanner = scanner_for(bytes, 0, u64::try_from(bytes.len()).unwrap());
    let first = complete(scanner.next().unwrap());
    let second = complete(scanner.next().unwrap());

    let mut first_reader = first.open().unwrap();
    let mut second_reader = second.open().unwrap();
    let mut first_prefix = [0_u8; 2];
    let mut second_prefix = [0_u8; 1];
    first_reader.read_exact(&mut first_prefix).unwrap();
    second_reader.read_exact(&mut second_prefix).unwrap();
    assert_eq!(&first_prefix, b"al");
    assert_eq!(&second_prefix, b"b");

    let third = complete(scanner.next().unwrap());
    let mut first_tail = Vec::new();
    let mut second_tail = Vec::new();
    first_reader.read_to_end(&mut first_tail).unwrap();
    second_reader.read_to_end(&mut second_tail).unwrap();
    assert_eq!(first_tail, b"pha");
    assert_eq!(second_tail, b"eta");
    assert_eq!(read_window(&third), b"gamma");
    assert!(matches!(scanner.next().unwrap(), ScanOutcome::Eof));
}

#[test]
fn scanner_handles_newlines_at_and_around_the_read_buffer_boundary() {
    for content_length in [
        READ_BUFFER_BYTES - 1,
        READ_BUFFER_BYTES,
        READ_BUFFER_BYTES + 1,
    ] {
        let mut bytes = vec![b'x'; content_length];
        bytes.push(b'\n');
        let mut scanner = scanner_for(&bytes, 0, u64::try_from(bytes.len()).unwrap());

        let record = complete(scanner.next().unwrap());
        assert_eq!(
            record.content_length(),
            u64::try_from(content_length).unwrap()
        );
        assert_eq!(record.content_end(), u64::try_from(content_length).unwrap());
        assert_eq!(
            record.next_offset(),
            u64::try_from(content_length + 1).unwrap()
        );
        assert_eq!(read_window(&record), &bytes[..content_length]);
        assert_eq!(scanner.buffer_capacity(), READ_BUFFER_BYTES);
        assert!(matches!(scanner.next().unwrap(), ScanOutcome::Eof));
    }
}

#[test]
fn scanner_emits_empty_records_and_a_final_complete_record() {
    let bytes = b"\n{}\n";
    let mut scanner = scanner_for(bytes, 0, u64::try_from(bytes.len()).unwrap());

    let empty = complete(scanner.next().unwrap());
    assert_eq!(empty.start(), 0);
    assert_eq!(empty.content_end(), 0);
    assert_eq!(empty.content_length(), 0);
    assert_eq!(empty.next_offset(), 1);
    assert_eq!(read_window(&empty), b"");
    assert_eq!(
        empty.record_hash(),
        format!("b3:{}", blake3::hash(b"").to_hex())
    );

    let final_record = complete(scanner.next().unwrap());
    assert_eq!(final_record.start(), 1);
    assert_eq!(final_record.content_end(), 3);
    assert_eq!(final_record.next_offset(), 4);
    assert_eq!(read_window(&final_record), b"{}");
    assert!(matches!(scanner.next().unwrap(), ScanOutcome::Eof));
}

#[test]
fn multi_window_hash_matches_direct_blake3_without_the_newline() {
    let mut content = vec![b'a'; READ_BUFFER_BYTES * 2 + 17];
    content[READ_BUFFER_BYTES - 1] = b'\\';
    content[READ_BUFFER_BYTES] = b'"';
    let mut bytes = content.clone();
    bytes.push(b'\n');
    let mut scanner = scanner_for(&bytes, 0, u64::try_from(bytes.len()).unwrap());

    let record = complete(scanner.next().unwrap());
    assert_eq!(
        record.record_hash(),
        format!("b3:{}", blake3::hash(&content).to_hex())
    );
    assert_eq!(read_window(&record), content);
}

#[test]
fn target_size_smaller_than_the_physical_file_is_a_hard_boundary() {
    let bytes = b"one\ntwo\n";
    let mut scanner = scanner_for(bytes, 0, 4);

    let record = complete(scanner.next().unwrap());
    assert_eq!(read_window(&record), b"one");
    let reads_after_record = scanner.bytes_read();
    assert!(matches!(scanner.next().unwrap(), ScanOutcome::Eof));
    assert_eq!(scanner.bytes_read(), reads_after_record);

    let mut scanner = scanner_for(bytes, 0, 3);
    assert!(matches!(
        scanner.next().unwrap(),
        ScanOutcome::Incomplete { retry_from: 0 }
    ));
    assert_eq!(scanner.bytes_read(), 3);
}

#[test]
fn target_size_larger_than_the_physical_file_treats_short_reads_as_incomplete() {
    let bytes = b"{\"partial\":true}";
    let physical_size = u64::try_from(bytes.len()).unwrap();
    let mut scanner = scanner_for(bytes, 0, physical_size + 100);

    assert!(matches!(
        scanner.next().unwrap(),
        ScanOutcome::Incomplete { retry_from: 0 }
    ));
    assert_eq!(scanner.bytes_read(), physical_size);
    assert!(matches!(
        scanner.next().unwrap(),
        ScanOutcome::Incomplete { retry_from: 0 }
    ));
    assert_eq!(scanner.bytes_read(), physical_size * 2);
}

fn json_record_at_length(desired_length: usize, escapes: &[u8]) -> Vec<u8> {
    const PREFIX: &[u8] = br#"{"v":""#;
    const SUFFIX: &[u8] = br#""}"#;
    const ESCAPES: [&[u8]; 5] = [br"\\", br#"\""#, br"\n", br"\t", br"\u263a"];

    let escaped_length = escapes
        .iter()
        .map(|index| ESCAPES[usize::from(*index)].len())
        .sum::<usize>();
    let structural_length = PREFIX.len() + escaped_length + SUFFIX.len();
    let mut record = Vec::with_capacity(desired_length.max(structural_length));
    record.extend_from_slice(PREFIX);
    record.resize(
        PREFIX.len() + desired_length.saturating_sub(structural_length),
        b'x',
    );
    for index in escapes {
        record.extend_from_slice(ESCAPES[usize::from(*index)]);
    }
    record.extend_from_slice(SUFFIX);
    record
}

fn record_case_strategy() -> impl Strategy<Value = Vec<u8>> {
    (
        prop::sample::select(vec![
            READ_BUFFER_BYTES - 1,
            READ_BUFFER_BYTES,
            READ_BUFFER_BYTES + 1,
            READ_BUFFER_BYTES * 2 - 1,
            READ_BUFFER_BYTES * 2,
            READ_BUFFER_BYTES * 2 + 1,
        ]),
        prop::collection::vec(0_u8..5_u8, 0..5),
    )
        .prop_map(|(boundary, escapes): (usize, Vec<u8>)| json_record_at_length(boundary, &escapes))
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(32))]

    #[test]
    fn property_reconstructs_complete_records_and_preserves_incomplete_retry(
        records in prop::collection::vec(record_case_strategy(), 1..5),
        incomplete_escapes in prop::collection::vec(0_u8..5, 0..5),
        include_incomplete in any::<bool>(),
    ) {
        let mut bytes = Vec::new();
        for record in &records {
            bytes.extend_from_slice(record);
            bytes.push(b'\n');
        }
        let retry_from = u64::try_from(bytes.len()).unwrap();
        if include_incomplete {
            bytes.extend_from_slice(&json_record_at_length(97, &incomplete_escapes));
        }

        let mut scanner = scanner_for(&bytes, 0, u64::try_from(bytes.len()).unwrap());
        for expected in &records {
            let record = complete(scanner.next().unwrap());
            prop_assert_eq!(record.content_length(), u64::try_from(expected.len()).unwrap());
            prop_assert_eq!(read_window(&record), expected.as_slice());
            prop_assert_eq!(
                record.record_hash(),
                format!("b3:{}", blake3::hash(expected).to_hex())
            );
            prop_assert_eq!(scanner.buffer_capacity(), READ_BUFFER_BYTES);
        }

        if include_incomplete {
            let first_retry = scanner.next().unwrap();
            let second_retry = scanner.next().unwrap();
            let ScanOutcome::Incomplete { retry_from: first_actual } = first_retry else {
                return Err(TestCaseError::fail("expected first incomplete outcome"));
            };
            let ScanOutcome::Incomplete { retry_from: second_actual } = second_retry else {
                return Err(TestCaseError::fail("expected repeated incomplete outcome"));
            };
            prop_assert_eq!(first_actual, retry_from);
            prop_assert_eq!(second_actual, retry_from);
        } else {
            prop_assert!(matches!(scanner.next().unwrap(), ScanOutcome::Eof));
        }
    }
}
