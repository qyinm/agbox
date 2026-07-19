use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{Provider, SourceIdentity};

macro_rules! string_id {
    ($name:ident) => {
        #[derive(Clone, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
        #[serde(transparent)]
        pub struct $name(String);

        impl $name {
            pub fn as_str(&self) -> &str {
                &self.0
            }

            pub fn parse_wire(value: &str) -> Option<Self> {
                (!value.is_empty()
                    && value.len() <= 128
                    && value.bytes().all(|byte| byte.is_ascii_graphic()))
                .then(|| Self(value.to_owned()))
            }
        }
    };
}

string_id!(EventId);
string_id!(SemanticKey);
string_id!(WorkId);
string_id!(EvidenceId);
string_id!(ContractId);
string_id!(ProjectId);
string_id!(SessionId);
string_id!(AgentRunId);

fn stable(prefix: &str, parts: &[&[u8]]) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(prefix.as_bytes());
    for part in parts {
        hasher.update(&(part.len() as u64).to_le_bytes());
        hasher.update(part);
    }
    format!("{prefix}_{}", &hasher.finalize().to_hex()[..24])
}

impl EventId {
    #[must_use]
    pub fn from_source(source: &SourceIdentity, local_ordinal: u32) -> Self {
        Self(stable(
            "evt",
            &[
                source.provider.as_str().as_bytes(),
                source.source_id.as_bytes(),
                &source.generation.to_le_bytes(),
                &source.byte_offset.to_le_bytes(),
                source.record_hash.as_bytes(),
                &local_ordinal.to_le_bytes(),
            ],
        ))
    }
}

impl EvidenceId {
    #[must_use]
    pub fn from_source(source: &SourceIdentity, local_ordinal: u32) -> Self {
        Self(stable(
            "ev",
            &[
                source.provider.as_str().as_bytes(),
                source.source_id.as_bytes(),
                &source.generation.to_le_bytes(),
                &source.byte_offset.to_le_bytes(),
                source.record_hash.as_bytes(),
                &local_ordinal.to_le_bytes(),
            ],
        ))
    }
}

impl SemanticKey {
    #[must_use]
    pub fn from_native(
        provider: Provider,
        native_session_id: &str,
        namespace: &str,
        native_id: &str,
    ) -> Self {
        Self(stable(
            "sem",
            &[
                provider.as_str().as_bytes(),
                native_session_id.as_bytes(),
                namespace.as_bytes(),
                native_id.as_bytes(),
            ],
        ))
    }
}

impl WorkId {
    #[must_use]
    pub fn new() -> Self {
        Self(format!("work_{}", Uuid::new_v4().simple()))
    }
}

impl Default for WorkId {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(feature = "test-support")]
macro_rules! test_id_constructor {
    ($name:ident) => {
        impl $name {
            pub fn for_test(value: &str) -> Self {
                Self(value.to_owned())
            }
        }
    };
}

#[cfg(feature = "test-support")]
test_id_constructor!(EvidenceId);
#[cfg(feature = "test-support")]
test_id_constructor!(ProjectId);
#[cfg(feature = "test-support")]
test_id_constructor!(WorkId);

impl Provider {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Codex => "codex",
        }
    }
}
