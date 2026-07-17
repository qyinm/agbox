use std::fmt;

use serde::{Deserialize, Deserializer, Serialize, de};
use time::OffsetDateTime;

use crate::{ContentRef, Provider, limits::MAX_INLINE_BYTES};

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ByteRange {
    start: u64,
    end: u64,
}

#[derive(Debug, thiserror::Error)]
pub enum SourceError {
    #[error("byte range end precedes its start")]
    InvalidRange,
    #[error("{0} exceeds the inline-content bound")]
    TextTooLarge(&'static str),
    #[error("bounded source content is invalid")]
    InvalidContent(#[from] crate::ContentError),
}

impl ByteRange {
    /// Constructs an ordered byte range.
    ///
    /// # Errors
    ///
    /// Returns [`SourceError::InvalidRange`] when `end < start`.
    pub fn new(start: u64, end: u64) -> Result<Self, SourceError> {
        let range = Self { start, end };
        range.validate()?;
        Ok(range)
    }

    /// Revalidates the range before a store write.
    ///
    /// # Errors
    ///
    /// Returns [`SourceError::InvalidRange`] when `end < start`.
    pub fn validate(&self) -> Result<(), SourceError> {
        if self.end < self.start {
            return Err(SourceError::InvalidRange);
        }
        Ok(())
    }

    #[must_use]
    pub fn start(&self) -> u64 {
        self.start
    }

    #[must_use]
    pub fn end(&self) -> u64 {
        self.end
    }
}

#[derive(Deserialize)]
struct ByteRangeWire {
    start: u64,
    end: u64,
}

impl<'de> Deserialize<'de> for ByteRange {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = ByteRangeWire::deserialize(deserializer)?;
        Self::new(wire.start, wire.end).map_err(de::Error::custom)
    }
}

#[derive(Clone)]
pub struct SourceRefDraft {
    pub provider: Provider,
    pub format: String,
    pub native_session_id: String,
    pub native_record_type: String,
    pub native_record_id: Option<String>,
    pub source_generation: u64,
    pub byte_offset: u64,
    pub ordinal: Option<u64>,
    pub record_hash: String,
    pub decoder_version: String,
}

impl fmt::Debug for SourceRefDraft {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SourceRefDraft")
            .field("provider", &self.provider)
            .field("source_generation", &self.source_generation)
            .field("byte_offset", &self.byte_offset)
            .field("ordinal", &self.ordinal)
            .field("record_hash", &self.record_hash)
            .finish_non_exhaustive()
    }
}

#[derive(Clone, Eq, PartialEq, Serialize)]
pub struct SourceRef {
    provider: Provider,
    format: String,
    native_session_id: String,
    native_record_type: String,
    native_record_id: Option<String>,
    source_generation: u64,
    byte_offset: u64,
    ordinal: Option<u64>,
    record_hash: String,
    decoder_version: String,
}

impl fmt::Debug for SourceRef {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SourceRef")
            .field("provider", &self.provider)
            .field("source_generation", &self.source_generation)
            .field("byte_offset", &self.byte_offset)
            .field("ordinal", &self.ordinal)
            .field("record_hash", &self.record_hash)
            .finish_non_exhaustive()
    }
}

impl SourceRef {
    /// Constructs validated source metadata.
    ///
    /// # Errors
    ///
    /// Returns [`SourceError`] when any source-native string exceeds its bound.
    pub fn new(draft: SourceRefDraft) -> Result<Self, SourceError> {
        let source = Self {
            provider: draft.provider,
            format: draft.format,
            native_session_id: draft.native_session_id,
            native_record_type: draft.native_record_type,
            native_record_id: draft.native_record_id,
            source_generation: draft.source_generation,
            byte_offset: draft.byte_offset,
            ordinal: draft.ordinal,
            record_hash: draft.record_hash,
            decoder_version: draft.decoder_version,
        };
        source.validate()?;
        Ok(source)
    }

    /// Revalidates source metadata before a store write.
    ///
    /// # Errors
    ///
    /// Returns [`SourceError`] when any source-native string exceeds its bound.
    pub fn validate(&self) -> Result<(), SourceError> {
        validate_source_text("format", &self.format)?;
        validate_source_text("native_session_id", &self.native_session_id)?;
        validate_source_text("native_record_type", &self.native_record_type)?;
        if let Some(native_record_id) = &self.native_record_id {
            validate_source_text("native_record_id", native_record_id)?;
        }
        validate_source_text("record_hash", &self.record_hash)?;
        validate_source_text("decoder_version", &self.decoder_version)
    }

    #[must_use]
    pub fn provider(&self) -> Provider {
        self.provider
    }

    #[must_use]
    pub fn format(&self) -> &str {
        &self.format
    }

    #[must_use]
    pub fn native_session_id(&self) -> &str {
        &self.native_session_id
    }

    #[must_use]
    pub fn native_record_type(&self) -> &str {
        &self.native_record_type
    }

    #[must_use]
    pub fn native_record_id(&self) -> Option<&str> {
        self.native_record_id.as_deref()
    }

    #[must_use]
    pub fn source_generation(&self) -> u64 {
        self.source_generation
    }

    #[must_use]
    pub fn byte_offset(&self) -> u64 {
        self.byte_offset
    }

    #[must_use]
    pub fn ordinal(&self) -> Option<u64> {
        self.ordinal
    }

    #[must_use]
    pub fn record_hash(&self) -> &str {
        &self.record_hash
    }

    #[must_use]
    pub fn decoder_version(&self) -> &str {
        &self.decoder_version
    }
}

#[derive(Deserialize)]
struct SourceRefWire {
    provider: Provider,
    format: String,
    native_session_id: String,
    native_record_type: String,
    native_record_id: Option<String>,
    source_generation: u64,
    byte_offset: u64,
    ordinal: Option<u64>,
    record_hash: String,
    decoder_version: String,
}

impl<'de> Deserialize<'de> for SourceRef {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = SourceRefWire::deserialize(deserializer)?;
        Self::new(SourceRefDraft {
            provider: wire.provider,
            format: wire.format,
            native_session_id: wire.native_session_id,
            native_record_type: wire.native_record_type,
            native_record_id: wire.native_record_id,
            source_generation: wire.source_generation,
            byte_offset: wire.byte_offset,
            ordinal: wire.ordinal,
            record_hash: wire.record_hash,
            decoder_version: wire.decoder_version,
        })
        .map_err(de::Error::custom)
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DecodeStatus {
    Known,
    UnknownType,
    Malformed,
    Oversized,
}

#[derive(Clone)]
pub struct SourceObservationDraft {
    pub observation_id: String,
    pub source: SourceRef,
    pub range: ByteRange,
    pub observed_at: OffsetDateTime,
    pub status: DecodeStatus,
    pub bounded_record: Option<ContentRef>,
    pub schema_fingerprint: String,
}

impl fmt::Debug for SourceObservationDraft {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SourceObservationDraft")
            .field("observation_id", &self.observation_id)
            .field("record_hash", &self.source.record_hash)
            .field(
                "record_byte_length",
                &self
                    .bounded_record
                    .as_ref()
                    .map_or(0, ContentRef::byte_length),
            )
            .finish_non_exhaustive()
    }
}

#[derive(Clone, Eq, PartialEq, Serialize)]
pub struct SourceObservation {
    observation_id: String,
    source: SourceRef,
    range: ByteRange,
    observed_at: OffsetDateTime,
    status: DecodeStatus,
    bounded_record: Option<ContentRef>,
    schema_fingerprint: String,
}

impl fmt::Debug for SourceObservation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SourceObservation")
            .field("observation_id", &self.observation_id)
            .field("record_hash", &self.source.record_hash)
            .field(
                "record_byte_length",
                &self
                    .bounded_record
                    .as_ref()
                    .map_or(0, ContentRef::byte_length),
            )
            .finish_non_exhaustive()
    }
}

impl SourceObservation {
    /// Constructs a validated immutable source observation.
    ///
    /// # Errors
    ///
    /// Returns [`SourceError`] when source, range, content, or observation text
    /// violates its invariant.
    pub fn new(draft: SourceObservationDraft) -> Result<Self, SourceError> {
        let observation = Self {
            observation_id: draft.observation_id,
            source: draft.source,
            range: draft.range,
            observed_at: draft.observed_at,
            status: draft.status,
            bounded_record: draft.bounded_record,
            schema_fingerprint: draft.schema_fingerprint,
        };
        observation.validate()?;
        Ok(observation)
    }

    /// Revalidates the observation before a store write.
    ///
    /// # Errors
    ///
    /// Returns [`SourceError`] when an observation invariant is violated.
    pub fn validate(&self) -> Result<(), SourceError> {
        validate_source_text("observation_id", &self.observation_id)?;
        validate_source_text("schema_fingerprint", &self.schema_fingerprint)?;
        self.source.validate()?;
        self.range.validate()?;
        if let Some(content) = &self.bounded_record {
            content.validate()?;
        }
        Ok(())
    }

    #[must_use]
    pub fn observation_id(&self) -> &str {
        &self.observation_id
    }

    #[must_use]
    pub fn source(&self) -> &SourceRef {
        &self.source
    }

    #[must_use]
    pub fn range(&self) -> &ByteRange {
        &self.range
    }

    #[must_use]
    pub fn observed_at(&self) -> OffsetDateTime {
        self.observed_at
    }

    #[must_use]
    pub fn status(&self) -> DecodeStatus {
        self.status
    }

    #[must_use]
    pub fn bounded_record(&self) -> Option<&ContentRef> {
        self.bounded_record.as_ref()
    }

    #[must_use]
    pub fn schema_fingerprint(&self) -> &str {
        &self.schema_fingerprint
    }
}

#[derive(Deserialize)]
struct SourceObservationWire {
    observation_id: String,
    source: SourceRef,
    range: ByteRange,
    observed_at: OffsetDateTime,
    status: DecodeStatus,
    bounded_record: Option<ContentRef>,
    schema_fingerprint: String,
}

impl<'de> Deserialize<'de> for SourceObservation {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = SourceObservationWire::deserialize(deserializer)?;
        Self::new(SourceObservationDraft {
            observation_id: wire.observation_id,
            source: wire.source,
            range: wire.range,
            observed_at: wire.observed_at,
            status: wire.status,
            bounded_record: wire.bounded_record,
            schema_fingerprint: wire.schema_fingerprint,
        })
        .map_err(de::Error::custom)
    }
}

fn validate_source_text(field: &'static str, value: &str) -> Result<(), SourceError> {
    if value.len() > MAX_INLINE_BYTES {
        return Err(SourceError::TextTooLarge(field));
    }
    Ok(())
}
