mod adapter;
mod json;

pub use adapter::{
    DecodeContext, DecodeDisposition, DecodeError, DecodedEvidence, DecodedRecord,
    DecodedRecordDraft, DecoderState, DiscoveredSource, MAX_DECODER_STATE_BYTES,
    MAX_EVENTS_PER_RECORD, MAX_EVIDENCE_PER_RECORD, MAX_RECORD_SEMANTIC_BYTES, NativeIdentifier,
    RecordSource, RootClass, RootSpec, SourceAdapter, adapters,
};
#[cfg(feature = "test-support")]
pub use adapter::{MemoryRecordSource, decode_fixture};
pub use json::{BoundedJsonReader, CapturedString, MAX_CAPTURE_BYTES};
