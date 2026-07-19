use std::{
    fmt,
    io::{self, Read, Write},
    path::{Path, PathBuf},
};

use agbox_core::{
    ActivityEventV1, ContentRef, EventId, EvidenceId, ProjectId, Provider, SourceObservation,
};
#[cfg(feature = "test-support")]
use agbox_core::{ByteRange, DecodeStatus, SourceObservationDraft, SourceRef, SourceRefDraft};
use time::OffsetDateTime;
use zeroize::Zeroizing;

pub use agbox_core::limits::{
    MAX_DECODER_STATE_BYTES, MAX_EVENTS_PER_RECORD, MAX_EVIDENCE_PER_RECORD,
    MAX_RECORD_SEMANTIC_BYTES,
};

#[cfg(feature = "test-support")]
use crate::BoundedJsonReader;

const MAX_NATIVE_IDENTIFIER_BYTES: usize = 128;
const MAX_PROJECT_HINT_BYTES: u64 = 4_096;
// A reduced diagnostic is mechanically bounded. If checked serialization ever
// fails despite that invariant, preserve fail-closed semantics rather than
// publishing an apparently small semantic count.
const SEMANTIC_MEASUREMENT_FAILURE_SENTINEL: usize = usize::MAX;

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
    pub ctime: OffsetDateTime,
    pub session_time: Option<OffsetDateTime>,
}

/// Ephemeral, bounded workspace hint extracted before source enrollment.
///
/// It is not an authorization decision: callers must independently canonicalize
/// and bind the directory with `ProjectResolver` before assigning a project.
pub struct ProjectHint {
    value: Zeroizing<String>,
}

impl ProjectHint {
    #[must_use]
    pub fn as_path(&self) -> &Path {
        Path::new(&self.value)
    }
}

impl fmt::Debug for ProjectHint {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ProjectHint")
            .field("byte_length", &self.value.len())
            .finish()
    }
}

/// Extracts a bounded provider workspace hint from one JSON record.
///
/// # Errors
///
/// Returns a decoding error when the record is malformed or the selected path
/// is oversized; callers must then leave the source unassigned.
pub fn project_hint(provider: Provider, record: &[u8]) -> Result<Option<ProjectHint>, DecodeError> {
    let path = match provider {
        Provider::Claude => &["cwd"][..],
        Provider::Codex => &["payload", "cwd"][..],
    };
    let mut reader = crate::BoundedJsonReader::new(record);
    let Some(captured) = reader.capture_string(path)? else {
        return Ok(None);
    };
    if captured.total_bytes > MAX_PROJECT_HINT_BYTES {
        return Err(DecodeError::OutputTooLarge);
    }
    let value = String::from_utf8(captured.bytes)
        .map_err(|_| DecodeError::Malformed("project-hint-utf8".into()))?;
    if !Path::new(&value).is_absolute() {
        return Ok(None);
    }
    Ok(Some(ProjectHint {
        value: Zeroizing::new(value),
    }))
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
    pub project_root: Option<PathBuf>,
    pub source_id: String,
    pub observed_at: OffsetDateTime,
    pub source_generation: u64,
    pub format: String,
}

impl fmt::Debug for DecodeContext {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DecodeContext")
            .field("project_id", &self.project_id)
            .field("source_id_bytes", &self.source_id.len())
            .field("observed_at", &self.observed_at)
            .field("source_generation", &self.source_generation)
            .finish_non_exhaustive()
    }
}

#[derive(Clone, Eq, PartialEq)]
pub struct NativeIdentifier(String);

impl NativeIdentifier {
    fn from_raw_or(raw: &str, fallback: &'static str) -> Self {
        if allowlisted_native_identifier(raw.as_bytes()) {
            Self(raw.to_owned())
        } else {
            Self(fallback.to_owned())
        }
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for NativeIdentifier {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("NativeIdentifier")
            .field("byte_length", &self.0.len())
            .finish_non_exhaustive()
    }
}

impl PartialEq<str> for NativeIdentifier {
    fn eq(&self, other: &str) -> bool {
        self.0 == other
    }
}

impl PartialEq<&str> for NativeIdentifier {
    fn eq(&self, other: &&str) -> bool {
        self.0 == *other
    }
}

#[derive(Clone, Eq, PartialEq)]
pub enum DecodeDisposition {
    Known,
    UnknownType { native_type: NativeIdentifier },
    Malformed { class: NativeIdentifier },
    Oversized { class: NativeIdentifier },
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

impl DecodeDisposition {
    #[must_use]
    pub fn unknown_type(raw: &str) -> Self {
        Self::UnknownType {
            native_type: NativeIdentifier::from_raw_or(raw, "invalid-native-type"),
        }
    }

    #[must_use]
    pub fn malformed(class: &str) -> Self {
        Self::Malformed {
            class: NativeIdentifier::from_raw_or(class, "invalid-malformed-class"),
        }
    }

    #[must_use]
    pub fn oversized(class: &str) -> Self {
        Self::Oversized {
            class: NativeIdentifier::from_raw_or(class, "invalid-oversized-class"),
        }
    }

    #[must_use]
    pub fn native_type(&self) -> Option<&str> {
        match self {
            Self::UnknownType { native_type } => Some(native_type.as_str()),
            _ => None,
        }
    }

    #[must_use]
    pub fn class(&self) -> Option<&str> {
        match self {
            Self::Malformed { class } | Self::Oversized { class } => Some(class.as_str()),
            _ => None,
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
pub struct DecodedRecordDraft {
    pub observation: SourceObservation,
    pub events: Vec<ActivityEventV1>,
    pub evidence: Vec<DecodedEvidence>,
    pub disposition: DecodeDisposition,
    pub next_state: DecoderState,
    pub semantic_bytes: usize,
}

impl fmt::Debug for DecodedRecordDraft {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DecodedRecordDraft")
            .field("observation", &self.observation)
            .field("event_count", &self.events.len())
            .field("evidence_count", &self.evidence.len())
            .field("disposition", &self.disposition)
            .field("next_state_bytes", &self.next_state.as_bytes().len())
            .field("reported_semantic_bytes", &self.semantic_bytes)
            .finish()
    }
}

#[derive(Clone)]
pub struct DecodedRecord {
    observation: SourceObservation,
    events: Vec<ActivityEventV1>,
    evidence: Vec<DecodedEvidence>,
    disposition: DecodeDisposition,
    next_state: DecoderState,
    semantic_bytes: usize,
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

/// Move-only normalized output from one verified record.
///
/// This type deliberately has no public constructor: callers can consume a
/// [`DecodedRecord`] without copying evidence plaintext, while construction
/// and limit enforcement remain inside the adapter boundary.
///
/// ```compile_fail
/// use agbox_adapters::{
///     DecodeDisposition, DecodedEvidence, DecodedRecordParts, DecoderState,
/// };
/// use agbox_core::{ActivityEventV1, SourceObservation};
///
/// fn bypass(
///     observation: SourceObservation,
///     events: Vec<ActivityEventV1>,
///     evidence: Vec<DecodedEvidence>,
/// ) -> DecodedRecordParts {
///     DecodedRecordParts {
///         observation,
///         events,
///         evidence,
///         disposition: DecodeDisposition::Known,
///         next_state: DecoderState::default(),
///         semantic_bytes: 0,
///     }
/// }
/// ```
pub struct DecodedRecordParts {
    observation: SourceObservation,
    events: Vec<ActivityEventV1>,
    evidence: Vec<DecodedEvidence>,
    disposition: DecodeDisposition,
    next_state: DecoderState,
    semantic_bytes: usize,
}

impl fmt::Debug for DecodedRecordParts {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DecodedRecordParts")
            .field("observation", &self.observation)
            .field("event_count", &self.events.len())
            .field("evidence_count", &self.evidence.len())
            .field("disposition", &self.disposition)
            .field("next_state_bytes", &self.next_state.as_bytes().len())
            .field("semantic_bytes", &self.semantic_bytes)
            .finish()
    }
}

impl DecodedRecordParts {
    /// Consumes the validated parts into their move-only components.
    #[must_use]
    #[allow(clippy::type_complexity)]
    pub fn decompose(
        self,
    ) -> (
        SourceObservation,
        Vec<ActivityEventV1>,
        Vec<DecodedEvidence>,
        DecodeDisposition,
        DecoderState,
        usize,
    ) {
        (
            self.observation,
            self.events,
            self.evidence,
            self.disposition,
            self.next_state,
            self.semantic_bytes,
        )
    }
}

impl DecodedRecord {
    /// Constructs a record and replaces over-limit normalized output with a
    /// bounded diagnostic. The draft's reported semantic byte count is ignored
    /// and replaced by an internal streaming measurement.
    #[must_use]
    pub fn new(draft: DecodedRecordDraft, prior_state: &DecoderState) -> Self {
        let record = Self {
            observation: draft.observation,
            events: draft.events,
            evidence: draft.evidence,
            disposition: draft.disposition,
            next_state: draft.next_state,
            semantic_bytes: 0,
        };
        record.enforce_limits(prior_state)
    }

    #[must_use]
    pub fn observation(&self) -> &SourceObservation {
        &self.observation
    }

    #[must_use]
    pub fn events(&self) -> &[ActivityEventV1] {
        &self.events
    }

    #[must_use]
    pub fn evidence(&self) -> &[DecodedEvidence] {
        &self.evidence
    }

    #[must_use]
    pub fn disposition(&self) -> &DecodeDisposition {
        &self.disposition
    }

    #[must_use]
    pub fn next_state(&self) -> &DecoderState {
        &self.next_state
    }

    #[must_use]
    pub const fn semantic_bytes(&self) -> usize {
        self.semantic_bytes
    }

    /// Consumes the validated record so evidence plaintext can move directly
    /// into encrypted-store write ownership.
    #[must_use]
    pub fn into_parts(self) -> DecodedRecordParts {
        DecodedRecordParts {
            observation: self.observation,
            events: self.events,
            evidence: self.evidence,
            disposition: self.disposition,
            next_state: self.next_state,
            semantic_bytes: self.semantic_bytes,
        }
    }

    #[cfg(feature = "test-support")]
    #[doc(hidden)]
    #[must_use]
    pub fn retained_capacities_for_test(&self) -> (usize, usize) {
        (self.events.capacity(), self.evidence.capacity())
    }

    /// Revalidates a previously constructed record against all output bounds.
    #[must_use]
    pub fn enforce_limits(mut self, prior_state: &DecoderState) -> Self {
        let too_large = match self.measure_semantic_bytes() {
            Some(measured) => {
                self.semantic_bytes = measured;
                measured > MAX_RECORD_SEMANTIC_BYTES
            }
            None => true,
        };
        if too_large {
            self.events = Vec::new();
            self.evidence = Vec::new();
            self.disposition = DecodeDisposition::oversized("normalized-output");
            self.next_state = prior_state.clone();
            self.semantic_bytes = match self.measure_semantic_bytes() {
                Some(measured) => measured,
                None => SEMANTIC_MEASUREMENT_FAILURE_SENTINEL,
            };
        }
        self
    }

    fn measure_semantic_bytes(&self) -> Option<usize> {
        if self.events.len() > MAX_EVENTS_PER_RECORD
            || self.evidence.len() > MAX_EVIDENCE_PER_RECORD
            || self.next_state.as_bytes().len() > MAX_DECODER_STATE_BYTES
        {
            return None;
        }
        if self
            .evidence
            .iter()
            .any(|item| item.plaintext.len() > agbox_core::limits::MAX_INLINE_BYTES)
        {
            return None;
        }

        let mut counter = SemanticCounter::default();
        counter.serialized(&self.observation).ok()?;
        counter.add(self.next_state.as_bytes().len())?;
        counter.add(
            self.disposition
                .native_type()
                .or_else(|| self.disposition.class())
                .map_or(0, str::len),
        )?;
        for event in &self.events {
            counter.serialized(event).ok()?;
        }
        for evidence in &self.evidence {
            counter.add(evidence.evidence_id.as_str().len())?;
            counter.add(evidence.owner_event_id.as_str().len())?;
            counter.serialized(&evidence.content).ok()?;
            counter.add(evidence.plaintext.len())?;
        }
        Some(counter.bytes)
    }
}

#[derive(Default)]
struct SemanticCounter {
    bytes: usize,
}

impl SemanticCounter {
    fn add(&mut self, bytes: usize) -> Option<()> {
        self.bytes = self.bytes.checked_add(bytes)?;
        Some(())
    }

    fn serialized<T: serde::Serialize>(&mut self, value: &T) -> Result<(), serde_json::Error> {
        serde_json::to_writer(self, value)
    }
}

impl Write for SemanticCounter {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        self.bytes = self
            .bytes
            .checked_add(buffer.len())
            .ok_or_else(|| io::Error::other("semantic byte count overflow"))?;
        Ok(buffer.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
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

    /// Emits a bounded continuation previously staged by [`Self::decode`].
    ///
    /// Callers must poll this method with each returned state until it returns
    /// `None` before decoding another source record. Implementations must derive
    /// continuations only from verified bounded state and must not reopen
    /// mutable source bytes.
    ///
    /// # Errors
    ///
    /// Returns [`DecodeError`] when persisted continuation state is invalid or
    /// cannot produce a complete bounded normalized result.
    fn decode_continuation(
        &self,
        _context: &DecodeContext,
        _state: &DecoderState,
    ) -> Result<Option<DecodedRecord>, DecodeError> {
        Ok(None)
    }
}

#[must_use]
pub fn adapters() -> &'static [&'static dyn SourceAdapter] {
    static CLAUDE: crate::ClaudeAdapter = crate::ClaudeAdapter;
    static ADAPTERS: [&dyn SourceAdapter; 1] = [&CLAUDE];
    &ADAPTERS
}

fn allowlisted_native_identifier(value: &[u8]) -> bool {
    if value.is_empty()
        || value.len() > MAX_NATIVE_IDENTIFIER_BYTES
        || !value.iter().all(u8::is_ascii_graphic)
    {
        return false;
    }
    std::str::from_utf8(value).is_ok()
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
    let raw_native_type = std::str::from_utf8(&native_type.bytes).unwrap_or("");
    let disposition = DecodeDisposition::unknown_type(raw_native_type);
    let native_type = disposition
        .native_type()
        .ok_or_else(|| DecodeError::Malformed("missing-native-type".to_owned()))?
        .to_owned();
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
    Ok(DecodedRecord::new(
        DecodedRecordDraft {
            observation,
            events: Vec::new(),
            evidence: Vec::new(),
            disposition,
            next_state: DecoderState::default(),
            semantic_bytes: 0,
        },
        &DecoderState::default(),
    ))
}
