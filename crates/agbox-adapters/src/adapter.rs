use std::{
    fmt,
    io::{self, Read},
    path::{Path, PathBuf},
};

use agbox_core::{
    ActivityEventV1, ByteRange, ContentRef, DecodeStatus, EventId, EvidenceId, ProjectId, Provider,
    SourceObservation, SourceObservationDraft, SourceRef, SourceRefDraft,
};
use time::OffsetDateTime;
use zeroize::Zeroizing;

pub use agbox_core::limits::{
    MAX_DECODER_STATE_BYTES, MAX_EVENTS_PER_RECORD, MAX_EVIDENCE_PER_RECORD,
    MAX_RECORD_SEMANTIC_BYTES,
};

use crate::BoundedJsonReader;

const MAX_NATIVE_IDENTIFIER_BYTES: usize = 128;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RootClass {
    Active,
    Archive,
}

#[derive(Clone)]
pub struct RootSpec {
    pub path: PathBuf,
    pub class: RootClass,
    pub recursive: bool,
}

impl fmt::Debug for RootSpec {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RootSpec")
            .field("class", &self.class)
            .field("recursive", &self.recursive)
            .finish_non_exhaustive()
    }
}

#[derive(Clone)]
pub struct DiscoveredSource {
    pub source_id: String,
    pub provider: Provider,
    pub root: PathBuf,
    pub path: PathBuf,
    pub class: RootClass,
    pub file_identity: String,
    pub generation: u64,
    pub size: u64,
    pub mtime: OffsetDateTime,
    pub session_time: Option<OffsetDateTime>,
}

impl fmt::Debug for DiscoveredSource {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DiscoveredSource")
            .field("provider", &self.provider)
            .field("class", &self.class)
            .field("generation", &self.generation)
            .field("size", &self.size)
            .field("has_session_time", &self.session_time.is_some())
            .finish_non_exhaustive()
    }
}

pub trait RecordSource: Send + Sync {
    fn start(&self) -> u64;
    fn end(&self) -> u64;
    fn record_hash(&self) -> &str;

    /// Opens a fresh reader over exactly this record's bytes.
    ///
    /// # Errors
    ///
    /// Returns an I/O error when the bounded source cannot be opened.
    fn open(&self) -> io::Result<Box<dyn Read + Send>>;
}

impl RecordSource for agbox_ingest::RecordWindow {
    fn start(&self) -> u64 {
        self.start()
    }

    fn end(&self) -> u64 {
        self.content_end()
    }

    fn record_hash(&self) -> &str {
        self.record_hash()
    }

    fn open(&self) -> io::Result<Box<dyn Read + Send>> {
        Ok(Box::new(agbox_ingest::RecordWindow::open(self)?))
    }
}

#[derive(Clone, Default, Eq, PartialEq)]
pub struct DecoderState {
    bytes: Vec<u8>,
}

impl fmt::Debug for DecoderState {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DecoderState")
            .field("byte_length", &self.bytes.len())
            .finish()
    }
}

impl DecoderState {
    /// Replaces the state only when the replacement is within its hard bound.
    ///
    /// # Errors
    ///
    /// Returns [`DecodeError::StateTooLarge`] and leaves the prior bytes
    /// untouched when `bytes` exceeds 32 KiB.
    pub fn replace(&mut self, bytes: Vec<u8>) -> Result<(), DecodeError> {
        if bytes.len() > MAX_DECODER_STATE_BYTES {
            return Err(DecodeError::StateTooLarge);
        }
        self.bytes = bytes;
        Ok(())
    }

    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes
    }
}

#[derive(Clone)]
pub struct DecodeContext {
    pub project_id: ProjectId,
    pub observed_at: OffsetDateTime,
    pub source_generation: u64,
    pub format: String,
}

impl fmt::Debug for DecodeContext {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DecodeContext")
            .field("project_id", &self.project_id)
            .field("observed_at", &self.observed_at)
            .field("source_generation", &self.source_generation)
            .finish_non_exhaustive()
    }
}

#[derive(Clone, Eq, PartialEq)]
pub enum DecodeDisposition {
    Known,
    UnknownType { native_type: String },
    Malformed { class: String },
    Oversized { class: String },
}

impl fmt::Debug for DecodeDisposition {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Known => formatter.write_str("Known"),
            Self::UnknownType { .. } => {
                formatter.write_str("UnknownType { native_type: <redacted> }")
            }
            Self::Malformed { .. } => formatter.write_str("Malformed { class: <redacted> }"),
            Self::Oversized { .. } => formatter.write_str("Oversized { class: <redacted> }"),
        }
    }
}

#[derive(Clone)]
pub struct DecodedEvidence {
    pub evidence_id: EvidenceId,
    pub owner_event_id: EventId,
    pub content: ContentRef,
    pub plaintext: Zeroizing<Vec<u8>>,
}

impl fmt::Debug for DecodedEvidence {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DecodedEvidence")
            .field("evidence_id", &self.evidence_id)
            .field("owner_event_id", &self.owner_event_id)
            .field("content", &self.content)
            .field("plaintext_bytes", &self.plaintext.len())
            .finish()
    }
}

#[derive(Clone)]
pub struct DecodedRecord {
    pub observation: SourceObservation,
    pub events: Vec<ActivityEventV1>,
    pub evidence: Vec<DecodedEvidence>,
    pub disposition: DecodeDisposition,
    pub next_state: DecoderState,
    pub semantic_bytes: usize,
}

impl fmt::Debug for DecodedRecord {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DecodedRecord")
            .field("observation", &self.observation)
            .field("event_count", &self.events.len())
            .field("evidence_count", &self.evidence.len())
            .field("disposition", &self.disposition)
            .field("next_state_bytes", &self.next_state.as_bytes().len())
            .field("semantic_bytes", &self.semantic_bytes)
            .finish()
    }
}

impl DecodedRecord {
    /// Replaces any over-limit normalized output with a bounded diagnostic.
    #[must_use]
    pub fn enforce_limits(mut self, prior_state: &DecoderState) -> Self {
        let evidence_too_large = self
            .evidence
            .iter()
            .any(|item| item.plaintext.len() > agbox_core::limits::MAX_INLINE_BYTES);
        let too_large = self.events.len() > MAX_EVENTS_PER_RECORD
            || self.evidence.len() > MAX_EVIDENCE_PER_RECORD
            || evidence_too_large
            || self.next_state.as_bytes().len() > MAX_DECODER_STATE_BYTES
            || self.semantic_bytes > MAX_RECORD_SEMANTIC_BYTES;
        if too_large {
            self.events.clear();
            self.evidence.clear();
            self.disposition = DecodeDisposition::Oversized {
                class: "normalized-output".to_owned(),
            };
            self.next_state = prior_state.clone();
            self.semantic_bytes = prior_state.as_bytes().len();
        }
        self
    }
}

#[derive(thiserror::Error)]
pub enum DecodeError {
    #[error("I/O failure: {0}")]
    Io(#[from] io::Error),
    #[error("malformed JSON")]
    Malformed(String),
    #[error("decoder state exceeds 32 KiB")]
    StateTooLarge,
    #[error("required identity field is absent: {0}")]
    MissingIdentity(&'static str),
    #[error("record exceeds bounded normalized output")]
    OutputTooLarge,
}

impl fmt::Debug for DecodeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => formatter.debug_tuple("Io").field(&error.kind()).finish(),
            Self::Malformed(_) => formatter.write_str("Malformed(<redacted>)"),
            Self::StateTooLarge => formatter.write_str("StateTooLarge"),
            Self::MissingIdentity(field) => formatter
                .debug_tuple("MissingIdentity")
                .field(field)
                .finish(),
            Self::OutputTooLarge => formatter.write_str("OutputTooLarge"),
        }
    }
}

pub trait SourceAdapter: Send + Sync {
    fn provider(&self) -> Provider;
    fn decoder_version(&self) -> &'static str;
    fn roots(&self, home: &Path) -> Vec<RootSpec>;
    fn matches(&self, root: &RootSpec, relative: &Path) -> bool;
    fn trusted_session_time(
        &self,
        root: &RootSpec,
        relative: &Path,
        mtime: OffsetDateTime,
    ) -> Option<OffsetDateTime>;

    /// Decodes one bounded record without retaining native JSON.
    ///
    /// # Errors
    ///
    /// Returns [`DecodeError`] when source integrity, JSON validity, identity,
    /// or an output bound prevents a complete normalized result.
    fn decode(
        &self,
        record: &dyn RecordSource,
        context: &DecodeContext,
        state: &DecoderState,
    ) -> Result<DecodedRecord, DecodeError>;
}

#[must_use]
pub fn adapters() -> &'static [&'static dyn SourceAdapter] {
    &[]
}

fn allowlisted_native_identifier(value: &[u8]) -> Option<String> {
    if value.is_empty()
        || value.len() > MAX_NATIVE_IDENTIFIER_BYTES
        || !value.iter().all(u8::is_ascii_graphic)
    {
        return None;
    }
    std::str::from_utf8(value).ok().map(str::to_owned)
}

#[cfg(feature = "test-support")]
pub struct MemoryRecordSource {
    bytes: Vec<u8>,
    hash: String,
}

#[cfg(feature = "test-support")]
impl fmt::Debug for MemoryRecordSource {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("MemoryRecordSource")
            .field("byte_length", &self.bytes.len())
            .field("record_hash", &self.hash)
            .finish()
    }
}

#[cfg(feature = "test-support")]
impl MemoryRecordSource {
    #[must_use]
    pub fn new(bytes: Vec<u8>) -> Self {
        let hash = blake3::hash(&bytes).to_hex().to_string();
        Self { bytes, hash }
    }
}

#[cfg(feature = "test-support")]
impl RecordSource for MemoryRecordSource {
    fn start(&self) -> u64 {
        0
    }

    fn end(&self) -> u64 {
        u64::try_from(self.bytes.len()).unwrap_or(u64::MAX)
    }

    fn record_hash(&self) -> &str {
        &self.hash
    }

    fn open(&self) -> io::Result<Box<dyn Read + Send>> {
        Ok(Box::new(io::Cursor::new(self.bytes.clone())))
    }
}

#[cfg(feature = "test-support")]
/// Runs the explicit fixture decoder; this is never part of the normal registry.
///
/// # Errors
///
/// Returns [`DecodeError`] when the provider is unsupported, the type is
/// missing, the input is malformed, or terminal source verification fails.
pub fn decode_fixture(
    provider: &str,
    record: &dyn RecordSource,
) -> Result<DecodedRecord, DecodeError> {
    let provider = match provider {
        "claude" => Provider::Claude,
        "codex" => Provider::Codex,
        _ => return Err(DecodeError::MissingIdentity("provider")),
    };
    let mut reader = BoundedJsonReader::new(record.open()?);
    let native_type = reader
        .capture_string(&["type"])?
        .ok_or(DecodeError::MissingIdentity("type"))?;
    let schema_fingerprint = reader
        .schema_fingerprint()
        .ok_or_else(|| DecodeError::Malformed("missing-schema".to_owned()))?
        .to_owned();
    let native_type = allowlisted_native_identifier(&native_type.bytes)
        .unwrap_or_else(|| "invalid-native-type".to_owned());
    let source = SourceRef::new(SourceRefDraft {
        provider,
        format: "fixture-jsonl".to_owned(),
        native_session_id: "fixture-session".to_owned(),
        native_record_type: native_type.clone(),
        native_record_id: None,
        source_generation: 0,
        byte_offset: record.start(),
        ordinal: None,
        record_hash: record.record_hash().to_owned(),
        decoder_version: "fixture-v1".to_owned(),
    })
    .map_err(|_| DecodeError::Malformed("invalid-source".to_owned()))?;
    let range = ByteRange::new(record.start(), record.end())
        .map_err(|_| DecodeError::Malformed("invalid-range".to_owned()))?;
    let observation_id = format!(
        "obs_{}",
        &blake3::hash(record.record_hash().as_bytes()).to_hex()[..24]
    );
    let observation = SourceObservation::new(SourceObservationDraft {
        observation_id,
        source,
        range,
        observed_at: OffsetDateTime::UNIX_EPOCH,
        status: DecodeStatus::UnknownType,
        bounded_record: None,
        schema_fingerprint,
    })
    .map_err(|_| DecodeError::Malformed("invalid-observation".to_owned()))?;
    Ok(DecodedRecord {
        observation,
        events: Vec::new(),
        evidence: Vec::new(),
        disposition: DecodeDisposition::UnknownType { native_type },
        next_state: DecoderState::default(),
        semantic_bytes: 0,
    })
}
