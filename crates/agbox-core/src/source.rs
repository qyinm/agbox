use std::fmt;

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use crate::{ContentRef, Provider};

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ByteRange {
    pub start: u64,
    pub end: u64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SourceRef {
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

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DecodeStatus {
    Known,
    UnknownType,
    Malformed,
    Oversized,
}

#[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
pub struct SourceObservation {
    pub observation_id: String,
    pub source: SourceRef,
    pub range: ByteRange,
    pub observed_at: OffsetDateTime,
    pub status: DecodeStatus,
    pub bounded_record: Option<ContentRef>,
    pub schema_fingerprint: String,
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
