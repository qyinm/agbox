#![allow(clippy::unwrap_used)]

use std::io::{self, Cursor, Read, Seek, SeekFrom, Write};

use agbox_adapters::{
    BoundedJsonReader, DecodeDisposition, DecodedEvidence, DecoderState, MAX_CAPTURE_BYTES,
    MAX_DECODER_STATE_BYTES, MAX_EVENTS_PER_RECORD, MAX_RECORD_SEMANTIC_BYTES, MemoryRecordSource,
    RecordSource,
};
use agbox_core::{ActivityEventV1, ContentRef, DisclosureClass, EvidenceId};
use agbox_ingest::{RecordScanner, ScanOutcome};
use proptest::prelude::*;

#[test]
fn unknown_top_level_type_is_preserved_as_drift() {
    let source =
        MemoryRecordSource::new(br#"{"type":"future-record","nested":{"value":1}}"#.to_vec());
    let decoded = agbox_adapters::decode_fixture("claude", &source).unwrap();
    assert!(matches!(
        decoded.disposition,
        DecodeDisposition::UnknownType { ref native_type }
            if native_type == "future-record"
    ));
    assert!(decoded.events.is_empty());
    assert!(!decoded.observation.schema_fingerprint().is_empty());
    assert!(!format!("{:?}", decoded.disposition).contains("future-record"));
}

#[test]
fn native_type_allowlist_replaces_non_ascii_and_overlong_values_without_debug_leaks() {
    for native_type in ["sëcret-native-type".to_owned(), "s".repeat(129)] {
        let source = MemoryRecordSource::new(format!(r#"{{"type":"{native_type}"}}"#).into_bytes());
        let decoded = agbox_adapters::decode_fixture("claude", &source).unwrap();
        assert!(matches!(
            decoded.disposition,
            DecodeDisposition::UnknownType { ref native_type }
                if native_type == "invalid-native-type"
        ));
        assert!(!format!("{decoded:?}").contains(&native_type));
    }
}

#[test]
fn selected_string_capture_is_bounded_but_hashes_the_whole_value() {
    let total = 8 * 1024 * 1024;
    let input = format!(r#"{{"message":"{}"}}"#, "x".repeat(total));
    let mut reader = BoundedJsonReader::new(input.as_bytes());
    let captured = reader.capture_string(&["message"]).unwrap().unwrap();
    assert_eq!(captured.bytes.len(), MAX_CAPTURE_BYTES);
    assert_eq!(captured.total_bytes, total as u64);
    assert_eq!(
        captured.hash,
        blake3::hash(&vec![b'x'; total]).to_hex().to_string()
    );
    assert!(captured.truncated);
}

#[test]
fn capture_boundary_escapes_and_utf8_are_exact() {
    let prefix = "a".repeat(MAX_CAPTURE_BYTES - 2);
    let input = format!(r#"{{"message":"{prefix}\n🦀z"}}"#);
    let mut reader = BoundedJsonReader::new(input.as_bytes());
    let captured = reader.capture_string(&["message"]).unwrap().unwrap();
    assert!(captured.truncated);
    assert!(std::str::from_utf8(&captured.bytes).is_ok());
    assert_eq!(captured.bytes.len(), MAX_CAPTURE_BYTES - 1);
    let whole = format!("{prefix}\n🦀z");
    assert_eq!(captured.total_bytes, whole.len() as u64);
    assert_eq!(
        captured.hash,
        blake3::hash(whole.as_bytes()).to_hex().to_string()
    );
}

#[test]
fn depth_128_succeeds_and_depth_129_is_rejected() {
    let at_limit = format!("{}\"ok\"{}", "[".repeat(128), "]".repeat(128));
    let mut reader = BoundedJsonReader::new(at_limit.as_bytes());
    assert!(reader.capture_string(&["missing"]).unwrap().is_none());

    let over = format!("{}\"no\"{}", "[".repeat(129), "]".repeat(129));
    let mut reader = BoundedJsonReader::new(over.as_bytes());
    assert!(reader.capture_string(&["missing"]).is_err());
}

#[test]
fn malformed_inputs_and_trailing_json_fail_closed() {
    for input in [
        b"{\"message\":\"\xFF\"}".as_slice(),
        br#"{"message":"\u12"}"#,
        br#"{"message":"\uD800"}"#,
        br#"{"message":"\uDC00"}"#,
        br#"{"message":"\uD800\u0041"}"#,
        br#"{"message":[}"#,
        br#"{"message":"ok"} false"#,
        br#"{"n":01}"#,
        br#"{"n":1.}"#,
        br#"{"n":1e}"#,
        br#"{"n":+1}"#,
    ] {
        let mut reader = BoundedJsonReader::new(input);
        assert!(reader.capture_string(&["message"]).is_err());
    }
}

#[test]
fn additive_fields_are_skipped_without_changing_selected_value() {
    let expected = br#"{"message":"selected"}"#;
    let additive = br#"{"unknown":{"huge":["ignored",1,true]},"message":"selected","later":null}"#;
    let mut a = BoundedJsonReader::new(expected.as_slice());
    let mut b = BoundedJsonReader::new(additive.as_slice());
    assert_eq!(
        a.capture_string(&["message"]).unwrap().unwrap().bytes,
        b.capture_string(&["message"]).unwrap().unwrap().bytes
    );
}

#[test]
fn duplicate_selected_identity_is_rejected_without_tracking_unselected_keys() {
    let mut selected_duplicate =
        BoundedJsonReader::new(br#"{"type":"first","type":"second"}"#.as_slice());
    assert!(selected_duplicate.capture_string(&["type"]).is_err());

    let source = MemoryRecordSource::new(br#"{"type":"first","type":"second"}"#.to_vec());
    assert!(agbox_adapters::decode_fixture("claude", &source).is_err());

    let mut unselected_duplicate =
        BoundedJsonReader::new(br#"{"other":1,"other":2,"type":"safe"}"#.as_slice());
    assert_eq!(
        unselected_duplicate
            .capture_string(&["type"])
            .unwrap()
            .unwrap()
            .bytes,
        b"safe"
    );
}

#[test]
fn selected_non_string_and_oversized_selected_scalar_are_rejected() {
    for input in [br#"{"message":[]}"#.as_slice(), br#"{"message":{}}"#] {
        let mut reader = BoundedJsonReader::new(input);
        assert!(reader.capture_string(&["message"]).is_err());
    }
    let input = format!(r#"{{"number":{}}}"#, "1".repeat(MAX_CAPTURE_BYTES + 1));
    let mut reader = BoundedJsonReader::new(input.as_bytes());
    assert!(
        reader
            .capture_scalar(&["number"], MAX_CAPTURE_BYTES)
            .is_err()
    );

    let mut skipped = BoundedJsonReader::new(input.as_bytes());
    assert!(skipped.capture_string(&["missing"]).is_ok());

    let mut small = BoundedJsonReader::new(br#"{"number":1234}"#.as_slice());
    assert!(small.capture_scalar(&["number"], 3).is_err());
    let mut exact = BoundedJsonReader::new(br#"{"number":1234}"#.as_slice());
    assert_eq!(
        exact.capture_scalar(&["number"], 4).unwrap().as_deref(),
        Some("1234")
    );
}

#[test]
fn schema_fingerprint_uses_shape_names_and_types_not_scalar_values() {
    fn fingerprint(input: &[u8]) -> String {
        let mut reader = BoundedJsonReader::new(input);
        let _ = reader.capture_string(&["message"]).unwrap();
        reader.schema_fingerprint().unwrap().to_owned()
    }
    assert_eq!(
        fingerprint(br#"{"message":"a","n":1}"#),
        fingerprint(br#"{"message":"b","n":42}"#)
    );
    assert_ne!(
        fingerprint(br#"{"message":"a","n":1}"#),
        fingerprint(br#"{"renamed":"a","n":1}"#)
    );
    assert_ne!(
        fingerprint(br#"{"message":"a","n":1}"#),
        fingerprint(br#"{"message":"a","n":"1"}"#)
    );
}

#[test]
fn overlong_field_names_are_streamed_and_debug_is_sanitized() {
    let secret = "secret-field-name".repeat(10_000);
    let input = format!(r#"{{"{secret}":"value","message":"safe"}}"#);
    let mut reader = BoundedJsonReader::new(input.as_bytes());
    let captured = reader.capture_string(&["message"]).unwrap().unwrap();
    assert_eq!(captured.bytes, b"safe");
    let debug = format!("{reader:?} {captured:?}");
    assert!(!debug.contains(&secret));
    assert!(!debug.contains("safe"));
    assert!(debug.len() < 512);
}

#[test]
fn decoder_state_rejection_preserves_prior_state() {
    let mut state = DecoderState::default();
    state.replace(b"prior".to_vec()).unwrap();
    let before = state.clone();
    assert!(
        state
            .replace(vec![b'x'; MAX_DECODER_STATE_BYTES + 1])
            .is_err()
    );
    assert_eq!(state, before);
    assert!(!format!("{state:?}").contains("prior"));
}

#[test]
fn normalized_output_limits_discard_partial_results_and_preserve_identity_state() {
    let source = MemoryRecordSource::new(br#"{"type":"future-record"}"#.to_vec());
    let base = agbox_adapters::decode_fixture("claude", &source).unwrap();
    let observation = base.observation.clone();
    let mut prior = DecoderState::default();
    prior.replace(b"prior-state".to_vec()).unwrap();
    let event = ActivityEventV1::fixture_message();

    let oversized_events = agbox_adapters::DecodedRecord {
        events: vec![event.clone(); MAX_EVENTS_PER_RECORD + 1],
        next_state: DecoderState::default(),
        ..base.clone()
    }
    .enforce_limits(&prior);
    assert_oversized_is_bounded(&oversized_events, &observation, &prior);

    let content_secret = "content-secret-sentinel";
    let content = ContentRef::bounded(
        "evidence-hash".to_owned(),
        1,
        content_secret,
        None,
        DisclosureClass::ObservedState,
        None,
    )
    .unwrap();
    let evidence = DecodedEvidence {
        evidence_id: EvidenceId::for_test("evidence_test"),
        owner_event_id: event.event_id().clone(),
        content,
        plaintext: zeroize::Zeroizing::new(b"plaintext-secret-sentinel".to_vec()),
    };
    let evidence_debug = format!("{evidence:?}");
    assert!(!evidence_debug.contains(content_secret));
    assert!(!evidence_debug.contains("plaintext-secret-sentinel"));
    assert!(evidence_debug.len() < 512);

    let oversized_evidence_count = agbox_adapters::DecodedRecord {
        evidence: vec![evidence.clone(); agbox_adapters::MAX_EVIDENCE_PER_RECORD + 1],
        next_state: DecoderState::default(),
        ..base.clone()
    }
    .enforce_limits(&prior);
    assert_oversized_is_bounded(&oversized_evidence_count, &observation, &prior);

    let oversized_plaintext = agbox_adapters::DecodedRecord {
        evidence: vec![DecodedEvidence {
            plaintext: zeroize::Zeroizing::new(vec![b'x'; MAX_CAPTURE_BYTES + 1]),
            ..evidence
        }],
        next_state: DecoderState::default(),
        ..base.clone()
    }
    .enforce_limits(&prior);
    assert_oversized_is_bounded(&oversized_plaintext, &observation, &prior);

    let oversized_semantics = agbox_adapters::DecodedRecord {
        semantic_bytes: MAX_RECORD_SEMANTIC_BYTES + 1,
        next_state: DecoderState::default(),
        ..base
    }
    .enforce_limits(&prior);
    assert_oversized_is_bounded(&oversized_semantics, &observation, &prior);
}

fn assert_oversized_is_bounded(
    decoded: &agbox_adapters::DecodedRecord,
    observation: &agbox_core::SourceObservation,
    prior: &DecoderState,
) {
    assert_eq!(&decoded.observation, observation);
    assert!(decoded.events.is_empty());
    assert!(decoded.evidence.is_empty());
    assert_eq!(&decoded.next_state, prior);
    assert!(matches!(
        decoded.disposition,
        DecodeDisposition::Oversized { .. }
    ));
}

struct FailingTerminalSource {
    bytes: Vec<u8>,
}

impl RecordSource for FailingTerminalSource {
    fn start(&self) -> u64 {
        0
    }
    fn end(&self) -> u64 {
        self.bytes.len() as u64
    }
    fn record_hash(&self) -> &'static str {
        "record-hash"
    }
    fn open(&self) -> io::Result<Box<dyn Read + Send>> {
        Ok(Box::new(FailingTerminalReader {
            inner: Cursor::new(self.bytes.clone()),
            failed: false,
        }))
    }
}

struct FailingTerminalReader {
    inner: Cursor<Vec<u8>>,
    failed: bool,
}

impl Read for FailingTerminalReader {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        let read = self.inner.read(output)?;
        if read == 0 && !self.failed {
            self.failed = true;
            return Err(io::Error::new(io::ErrorKind::InvalidData, "mutated"));
        }
        Ok(read)
    }
}

#[test]
fn terminal_source_integrity_failure_discards_partial_decode() {
    let source = FailingTerminalSource {
        bytes: br#"{"type":"future-record"}"#.to_vec(),
    };
    assert!(agbox_adapters::decode_fixture("claude", &source).is_err());
}

#[test]
fn record_window_mutation_and_truncation_cannot_yield_a_partial_success() {
    let original = br#"{"type":"future-record"}"#;
    let mut contents = original.to_vec();
    contents.push(b'\n');

    let mut mutated_file = tempfile::NamedTempFile::new().unwrap();
    mutated_file.write_all(&contents).unwrap();
    mutated_file.flush().unwrap();
    let mut scanner =
        RecordScanner::new(mutated_file.reopen().unwrap(), 0, contents.len() as u64).unwrap();
    let ScanOutcome::Complete(mutated_window) = scanner.next().unwrap() else {
        panic!("fixture record must scan");
    };
    mutated_file.as_file_mut().seek(SeekFrom::Start(0)).unwrap();
    mutated_file
        .as_file_mut()
        .write_all(br#"{"type":"mutate-record"}"#)
        .unwrap();
    mutated_file.flush().unwrap();
    assert!(agbox_adapters::decode_fixture("claude", &mutated_window).is_err());

    let mut truncated_file = tempfile::NamedTempFile::new().unwrap();
    truncated_file.write_all(&contents).unwrap();
    truncated_file.flush().unwrap();
    let mut scanner =
        RecordScanner::new(truncated_file.reopen().unwrap(), 0, contents.len() as u64).unwrap();
    let ScanOutcome::Complete(truncated_window) = scanner.next().unwrap() else {
        panic!("fixture record must scan");
    };
    truncated_file.as_file().set_len(8).unwrap();
    assert!(agbox_adapters::decode_fixture("claude", &truncated_window).is_err());
}

proptest! {
    #[test]
    fn capture_is_deterministic_across_escapes_utf8_and_boundaries(
        prefix in prop::collection::vec("[a-z🦀]{0,4}", 0..64),
        suffix in "[a-z🦀\\n]{0,64}",
    ) {
        let value = format!("{}{}", prefix.concat(), suffix);
        let json = format!("{{\"message\":{}}}", serde_json::to_string(&value).unwrap());
        let mut first = BoundedJsonReader::new(json.as_bytes());
        let mut second = BoundedJsonReader::new(json.as_bytes());
        let a = first.capture_string(&["message"]).unwrap().unwrap();
        let b = second.capture_string(&["message"]).unwrap().unwrap();
        prop_assert_eq!(&a, &b);
        prop_assert!(a.bytes.len() <= MAX_CAPTURE_BYTES);
        prop_assert!(std::str::from_utf8(&a.bytes).is_ok());
        prop_assert_eq!(a.total_bytes, value.len() as u64);
    }

    #[test]
    fn selection_is_bounded_across_field_order_and_capture_boundaries(
        extra in 0_usize..32,
        reverse in any::<bool>(),
    ) {
        let value = "x".repeat(MAX_CAPTURE_BYTES + extra);
        let json = if reverse {
            format!(r#"{{"other":true,"message":"{value}"}}"#)
        } else {
            format!(r#"{{"message":"{value}","other":true}}"#)
        };
        let mut reader = BoundedJsonReader::new(json.as_bytes());
        let captured = reader.capture_string(&["message"]).unwrap().unwrap();
        prop_assert!(captured.bytes.len() <= MAX_CAPTURE_BYTES);
        prop_assert_eq!(captured.total_bytes, value.len() as u64);
        prop_assert_eq!(captured.hash, blake3::hash(value.as_bytes()).to_hex().to_string());
    }
}
