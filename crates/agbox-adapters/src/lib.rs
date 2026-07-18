mod adapter;
mod claude;
mod codex;
mod json;

pub use adapter::{
    DecodeContext, DecodeDisposition, DecodeError, DecodedEvidence, DecodedRecord,
    DecodedRecordDraft, DecoderState, DiscoveredSource, MAX_DECODER_STATE_BYTES,
    MAX_EVENTS_PER_RECORD, MAX_EVIDENCE_PER_RECORD, MAX_RECORD_SEMANTIC_BYTES, NativeIdentifier,
    RecordSource, RootClass, RootSpec, SourceAdapter,
};
#[cfg(feature = "test-support")]
pub use adapter::{MemoryRecordSource, decode_fixture};
pub use claude::ClaudeAdapter;
pub use codex::{CodexAdapter, HistoryMode};
pub use json::{BoundedJsonReader, CapturedString, MAX_CAPTURE_BYTES};

const _: fn() -> &'static [&'static dyn SourceAdapter] = adapter::adapters;

#[must_use]
pub fn adapters() -> &'static [&'static dyn SourceAdapter] {
    static CLAUDE: ClaudeAdapter = ClaudeAdapter;
    static CODEX: CodexAdapter = CodexAdapter;
    static ADAPTERS: [&dyn SourceAdapter; 2] = [&CLAUDE, &CODEX];
    &ADAPTERS
}

#[cfg(feature = "test-support")]
pub mod test_support {
    use std::{
        io::{BufRead, BufReader},
        path::Path,
    };

    use agbox_core::ProjectId;
    use time::OffsetDateTime;

    use crate::{
        ClaudeAdapter, CodexAdapter, DecodeContext, DecodeError, DecodedRecord, DecoderState,
        MemoryRecordSource, SourceAdapter,
    };

    /// Decodes a sanitized JSONL fixture while preserving decoder state across
    /// records. This filesystem helper is intentionally test-only.
    ///
    /// # Errors
    ///
    /// Returns an error for unsupported providers, fixture I/O, or any record
    /// that violates the production decoder contract.
    pub fn decode_fixture_file(
        provider: &str,
        path: impl AsRef<Path>,
    ) -> Result<Vec<DecodedRecord>, DecodeError> {
        let adapter: &dyn SourceAdapter = match provider {
            "claude" => &ClaudeAdapter,
            "codex" => &CodexAdapter,
            _ => return Err(DecodeError::MissingIdentity("provider")),
        };
        let path = path.as_ref();
        let file = std::fs::File::open(path)?;
        let mut reader = BufReader::new(file);
        let mut line = Vec::new();
        let mut records = Vec::new();
        let mut state = DecoderState::default();
        let source_id = format!(
            "fixture_{}",
            &blake3::hash(path.to_string_lossy().as_bytes()).to_hex()[..24]
        );
        let context = DecodeContext {
            project_id: ProjectId::for_test("project_fixture"),
            project_root: Some("/fixture/project".into()),
            source_id,
            observed_at: OffsetDateTime::UNIX_EPOCH,
            source_generation: 0,
            format: match provider {
                "codex" => "codex-rollout-1".to_owned(),
                _ => "claude-transcript-2.1".to_owned(),
            },
        };
        loop {
            line.clear();
            let bytes = reader.read_until(b'\n', &mut line)?;
            if bytes == 0 {
                break;
            }
            if line.last() == Some(&b'\n') {
                let _ = line.pop();
            }
            if line.last() == Some(&b'\r') {
                let _ = line.pop();
            }
            if line.is_empty() {
                continue;
            }
            let source = MemoryRecordSource::new(std::mem::take(&mut line));
            let decoded = adapter.decode(&source, &context, &state)?;
            state = decoded.next_state().clone();
            records.push(decoded);
            while let Some(decoded) = adapter.decode_continuation(&context, &state)? {
                state = decoded.next_state().clone();
                records.push(decoded);
            }
        }
        Ok(records)
    }
}
