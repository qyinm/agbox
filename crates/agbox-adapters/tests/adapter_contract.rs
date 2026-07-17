#![allow(clippy::unwrap_used)]

use std::io::{self, Cursor, Read, Seek, SeekFrom, Write};

use agbox_adapters::{
    BoundedJsonReader, DecodeDisposition, DecodeError, DecodedEvidence, DecodedRecord,
    DecodedRecordDraft, DecoderState, MAX_CAPTURE_BYTES, MAX_DECODER_STATE_BYTES,
    MAX_EVENTS_PER_RECORD, MAX_RECORD_SEMANTIC_BYTES, MemoryRecordSource, RecordSource,
};
use agbox_core::{ActivityEventV1, ContentRef, DisclosureClass, EvidenceId};
use agbox_ingest::{RecordScanner, ScanOutcome};
use proptest::prelude::*;
use struson::reader::{JsonReader, JsonStreamReader};

#[test]
fn unknown_top_level_type_is_preserved_as_drift() {
    let source =
        MemoryRecordSource::new(br#"{"type":"future-record","nested":{"value":1}}"#.to_vec());
    let decoded = agbox_adapters::decode_fixture("claude", &source).unwrap();
    assert!(matches!(
        decoded.disposition(),
        DecodeDisposition::UnknownType { native_type }
            if native_type == "future-record"
    ));
    assert!(decoded.events().is_empty());
    assert!(!decoded.observation().schema_fingerprint().is_empty());
    assert!(!format!("{:?}", decoded.disposition()).contains("future-record"));
}

#[test]
fn native_type_allowlist_replaces_non_ascii_and_overlong_values_without_debug_leaks() {
    for native_type in ["sëcret-native-type".to_owned(), "s".repeat(129)] {
        let source = MemoryRecordSource::new(format!(r#"{{"type":"{native_type}"}}"#).into_bytes());
        let decoded = agbox_adapters::decode_fixture("claude", &source).unwrap();
        assert!(matches!(
            decoded.disposition(),
            DecodeDisposition::UnknownType { native_type }
                if native_type == "invalid-native-type"
        ));
        assert!(!format!("{decoded:?}").contains(&native_type));
    }

    let class_secret = "클래스-secret";
    let malformed = DecodeDisposition::malformed(class_secret);
    assert_eq!(malformed.class(), Some("invalid-malformed-class"));
    assert!(!format!("{malformed:?}").contains(class_secret));

    let long_class = "x".repeat(129);
    let oversized = DecodeDisposition::oversized(&long_class);
    assert_eq!(oversized.class(), Some("invalid-oversized-class"));
    assert!(!format!("{oversized:?}").contains(&long_class));
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
fn overlong_borrowed_path_selects_and_duplicate_rejects_by_streaming_hash() {
    let field = "selected-field-".repeat(32);
    let input = format!(r#"{{"{field}":"selected"}}"#);
    let mut reader = BoundedJsonReader::new(input.as_bytes());
    assert_eq!(
        reader
            .capture_string(&[field.as_str()])
            .unwrap()
            .unwrap()
            .bytes,
        b"selected"
    );

    let duplicate = format!(r#"{{"{field}":"first","{field}":"second"}}"#);
    let mut reader = BoundedJsonReader::new(duplicate.as_bytes());
    assert!(reader.capture_string(&[field.as_str()]).is_err());
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
    assert!(base.semantic_bytes() > 0);
    let observation = base.observation().clone();
    let mut prior = DecoderState::default();
    prior.replace(b"prior-state".to_vec()).unwrap();
    let event = ActivityEventV1::fixture_message();

    let oversized_events = DecodedRecord::new(
        DecodedRecordDraft {
            observation: base.observation().clone(),
            events: vec![event.clone(); MAX_EVENTS_PER_RECORD + 1],
            evidence: base.evidence().to_vec(),
            disposition: base.disposition().clone(),
            next_state: DecoderState::default(),
            semantic_bytes: 0,
        },
        &prior,
    );
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

    let oversized_evidence_count = DecodedRecord::new(
        DecodedRecordDraft {
            observation: base.observation().clone(),
            events: base.events().to_vec(),
            evidence: vec![evidence.clone(); agbox_adapters::MAX_EVIDENCE_PER_RECORD + 1],
            disposition: base.disposition().clone(),
            next_state: DecoderState::default(),
            semantic_bytes: 0,
        },
        &prior,
    );
    assert_oversized_is_bounded(&oversized_evidence_count, &observation, &prior);

    let maximum_evidence = DecodedEvidence {
        plaintext: zeroize::Zeroizing::new(vec![b'x'; MAX_CAPTURE_BYTES]),
        ..evidence.clone()
    };
    let underreported_semantics = DecodedRecord::new(
        DecodedRecordDraft {
            observation: base.observation().clone(),
            events: base.events().to_vec(),
            evidence: vec![maximum_evidence; agbox_adapters::MAX_EVIDENCE_PER_RECORD],
            disposition: base.disposition().clone(),
            next_state: DecoderState::default(),
            semantic_bytes: 0,
        },
        &prior,
    );
    assert_oversized_is_bounded(&underreported_semantics, &observation, &prior);

    let oversized_plaintext = DecodedRecord::new(
        DecodedRecordDraft {
            observation: base.observation().clone(),
            events: base.events().to_vec(),
            evidence: vec![DecodedEvidence {
                plaintext: zeroize::Zeroizing::new(vec![b'x'; MAX_CAPTURE_BYTES + 1]),
                ..evidence
            }],
            disposition: base.disposition().clone(),
            next_state: DecoderState::default(),
            semantic_bytes: 0,
        },
        &prior,
    );
    assert_oversized_is_bounded(&oversized_plaintext, &observation, &prior);

    let overreported_semantics = DecodedRecord::new(
        DecodedRecordDraft {
            observation: base.observation().clone(),
            events: base.events().to_vec(),
            evidence: base.evidence().to_vec(),
            disposition: base.disposition().clone(),
            next_state: DecoderState::default(),
            semantic_bytes: MAX_RECORD_SEMANTIC_BYTES + 1,
        },
        &prior,
    );
    assert_eq!(
        overreported_semantics.semantic_bytes(),
        base.semantic_bytes(),
        "caller-reported semantic bytes must be ignored"
    );
}

fn assert_oversized_is_bounded(
    decoded: &agbox_adapters::DecodedRecord,
    observation: &agbox_core::SourceObservation,
    prior: &DecoderState,
) {
    assert_eq!(decoded.observation(), observation);
    assert!(decoded.events().is_empty());
    assert!(decoded.evidence().is_empty());
    assert_eq!(decoded.next_state(), prior);
    assert!(matches!(
        decoded.disposition(),
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

    let mut reader = BoundedJsonReader::new(source.open().unwrap());
    assert!(matches!(
        reader.capture_scalar(&["type"], MAX_CAPTURE_BYTES + 1),
        Err(DecodeError::Io(error)) if error.kind() == io::ErrorKind::InvalidData
    ));
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

#[test]
fn early_parser_errors_still_drain_large_windows_and_surface_terminal_integrity_failure() {
    let malformed = format!(r#"{{"type":[}},"padding":"{}"}}"#, "x".repeat(16 * 1024));
    let (mut file, window) = scanned_window(format!("{malformed}\n").as_bytes());
    file.as_file_mut().seek(SeekFrom::End(-2)).unwrap();
    file.as_file_mut().write_all(b"y").unwrap();
    file.flush().unwrap();
    assert!(matches!(
        agbox_adapters::decode_fixture("claude", &window),
        Err(DecodeError::Io(error)) if error.kind() == io::ErrorKind::InvalidData
    ));

    let scalar = format!(
        r#"{{"number":123456,"padding":"{}"}}"#,
        "x".repeat(16 * 1024)
    );
    let (file, window) = scanned_window(format!("{scalar}\n").as_bytes());
    file.as_file().set_len(4 * 1024).unwrap();
    let mut reader = BoundedJsonReader::new(RecordSource::open(&window).unwrap());
    assert!(matches!(
        reader.capture_scalar(&["number"], 3),
        Err(DecodeError::Io(error)) if error.kind() == io::ErrorKind::UnexpectedEof
    ));
    assert!(reader.schema_fingerprint().is_none());
    assert_eq!(reader.retained_bytes(), 0);
}

fn scanned_window(bytes: &[u8]) -> (tempfile::NamedTempFile, agbox_ingest::RecordWindow) {
    let mut file = tempfile::NamedTempFile::new().unwrap();
    file.write_all(bytes).unwrap();
    file.flush().unwrap();
    let mut scanner = RecordScanner::new(file.reopen().unwrap(), 0, bytes.len() as u64).unwrap();
    let ScanOutcome::Complete(window) = scanner.next().unwrap() else {
        panic!("fixture record must scan");
    };
    (file, window)
}

proptest! {
    #[test]
    fn bounded_parser_acceptance_matches_struson_and_serde_json(
        input in prop::collection::vec(any::<u8>(), 0..512),
    ) {
        let mut bounded = BoundedJsonReader::new(input.as_slice());
        let bounded_accepts = bounded.capture_string(&["never-selected"]).is_ok();

        let mut struson = JsonStreamReader::new(input.as_slice());
        let struson_accepts = struson
            .skip_value()
            .and_then(|()| struson.consume_trailing_whitespace())
            .is_ok();
        let serde_accepts = serde_json::from_slice::<serde::de::IgnoredAny>(&input).is_ok();

        prop_assert_eq!(bounded_accepts, struson_accepts);
        prop_assert_eq!(bounded_accepts, serde_accepts);
    }

    #[test]
    fn selected_decoded_strings_match_struson_and_serde_json(value in any::<String>()) {
        let encoded = serde_json::to_string(&value).unwrap();
        let input = format!(r#"{{"message":{encoded},"other":1}}"#);

        let mut bounded = BoundedJsonReader::new(input.as_bytes());
        let captured = bounded.capture_string(&["message"]).unwrap().unwrap();

        let decoded: serde_json::Value = serde_json::from_str(&input).unwrap();
        let serde_value = decoded["message"].as_str().unwrap();

        let mut struson = JsonStreamReader::new(input.as_bytes());
        struson.begin_object().unwrap();
        prop_assert_eq!(struson.next_name().unwrap(), "message");
        let struson_value = struson.next_string().unwrap();
        prop_assert_eq!(struson.next_name().unwrap(), "other");
        struson.skip_value().unwrap();
        struson.end_object().unwrap();
        struson.consume_trailing_whitespace().unwrap();

        prop_assert_eq!(captured.bytes, value.as_bytes());
        prop_assert_eq!(serde_value, value.as_str());
        prop_assert_eq!(struson_value.as_str(), value.as_str());
    }

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
