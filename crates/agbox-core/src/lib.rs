pub mod activity;
pub mod content;
mod id;
pub mod limits;
pub mod privacy;
pub mod source;
pub mod work;

use serde::{Deserialize, Serialize};

pub use activity::{
    ActionOutcome, ActivityError, ActivityEventDraft, ActivityEventV1, Actor, EventPayload,
};
pub use content::{ContentError, ContentRef, LocalLocator};
pub use id::{
    AgentRunId, ContractId, EventId, EvidenceId, ProjectId, SemanticKey, SessionId, WorkId,
};
pub use privacy::{
    Authority, DisclosureClass, PrivacyLabel, RedactedText, RedactionError, RedactionPolicy,
};
pub use source::{
    ByteRange, DecodeStatus, SourceError, SourceObservation, SourceObservationDraft, SourceRef,
    SourceRefDraft,
};
pub use work::{
    AssertionError, ContractError, WorkAssertion, WorkContractRevision, WorkContractRevisionDraft,
    WorkEdge, WorkEdgeError, WorkEdgeKind, WorkStatus,
};

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Provider {
    Claude,
    Codex,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SourceIdentity {
    pub provider: Provider,
    pub source_id: String,
    pub generation: u64,
    pub byte_offset: u64,
    pub record_hash: String,
}
