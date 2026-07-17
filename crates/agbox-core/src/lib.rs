mod id;

use serde::{Deserialize, Serialize};

pub use id::{
    AgentRunId, ContractId, EventId, EvidenceId, ProjectId, SemanticKey, SessionId, WorkId,
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
