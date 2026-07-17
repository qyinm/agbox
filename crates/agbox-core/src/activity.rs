use std::fmt;

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use crate::{
    ContentRef, EventId, PrivacyLabel, ProjectId, Provider, RedactionPolicy, SemanticKey,
    SessionId, SourceIdentity, SourceRef,
};

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Actor {
    Human,
    Agent,
    Tool,
    System,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ActionOutcome {
    Succeeded,
    Failed,
    Cancelled,
    Unknown,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "kind")]
pub enum EventPayload {
    #[serde(rename = "session.started")]
    SessionStarted { context: Option<ContentRef> },
    #[serde(rename = "session.context_changed")]
    SessionContextChanged {
        context: ContentRef,
        branch_hash: Option<String>,
    },
    #[serde(rename = "turn.started")]
    TurnStarted { prompt_id: Option<String> },
    #[serde(rename = "turn.finished")]
    TurnFinished { outcome: ActionOutcome },
    #[serde(rename = "message.created")]
    MessageCreated { content: ContentRef },
    #[serde(rename = "action.requested")]
    ActionRequested {
        native_action_id: String,
        tool_name: String,
        input: ContentRef,
    },
    #[serde(rename = "action.finished")]
    ActionFinished {
        native_action_id: String,
        outcome: ActionOutcome,
        output: Option<ContentRef>,
    },
    #[serde(rename = "artifact.changed")]
    ArtifactChanged {
        path: ContentRef,
        operation: String,
        content_hash: Option<String>,
    },
    #[serde(rename = "plan.observed")]
    PlanObserved { plan: ContentRef },
    #[serde(rename = "agent.started")]
    AgentStarted { native_agent_id: String },
    #[serde(rename = "agent.finished")]
    AgentFinished {
        native_agent_id: String,
        outcome: ActionOutcome,
    },
    #[serde(rename = "context.compacted")]
    ContextCompacted { summary_hash: Option<String> },
    #[serde(rename = "diagnostic.observed")]
    DiagnosticObserved { level: String, message: ContentRef },
}

impl EventPayload {
    fn kind(&self) -> &'static str {
        match self {
            Self::SessionStarted { .. } => "session.started",
            Self::SessionContextChanged { .. } => "session.context_changed",
            Self::TurnStarted { .. } => "turn.started",
            Self::TurnFinished { .. } => "turn.finished",
            Self::MessageCreated { .. } => "message.created",
            Self::ActionRequested { .. } => "action.requested",
            Self::ActionFinished { .. } => "action.finished",
            Self::ArtifactChanged { .. } => "artifact.changed",
            Self::PlanObserved { .. } => "plan.observed",
            Self::AgentStarted { .. } => "agent.started",
            Self::AgentFinished { .. } => "agent.finished",
            Self::ContextCompacted { .. } => "context.compacted",
            Self::DiagnosticObserved { .. } => "diagnostic.observed",
        }
    }
}

#[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
pub struct ActivityEventV1 {
    pub event_id: EventId,
    pub semantic_key: SemanticKey,
    pub schema_version: u16,
    pub occurred_at: OffsetDateTime,
    pub observed_at: OffsetDateTime,
    pub project_id: ProjectId,
    pub session_id: SessionId,
    pub turn_id: Option<String>,
    pub actor: Actor,
    pub correlation_id: Option<String>,
    pub causation_id: Option<String>,
    pub source: SourceRef,
    pub payload: EventPayload,
    pub privacy: PrivacyLabel,
}

impl fmt::Debug for ActivityEventV1 {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ActivityEventV1")
            .field("event_id", &self.event_id)
            .field("semantic_key", &self.semantic_key)
            .field("project_id", &self.project_id)
            .field("session_id", &self.session_id)
            .field("event_kind", &self.payload.kind())
            .field("privacy", &self.privacy)
            .finish_non_exhaustive()
    }
}

#[cfg(any(test, feature = "test-support"))]
impl ActivityEventV1 {
    #[must_use]
    pub fn fixture_message() -> Self {
        let source_identity = SourceIdentity {
            provider: Provider::Codex,
            source_id: "src_fixture".into(),
            generation: 1,
            byte_offset: 64,
            record_hash: "b3:fixture-record".into(),
        };
        let source = SourceRef {
            provider: Provider::Codex,
            format: "jsonl".into(),
            native_session_id: "native-session-fixture".into(),
            native_record_type: "message".into(),
            native_record_id: Some("message-fixture".into()),
            source_generation: 1,
            byte_offset: 64,
            ordinal: Some(1),
            record_hash: "b3:fixture-record".into(),
            decoder_version: "fixture-v1".into(),
        };
        let Ok(redaction_policy) = RedactionPolicy::new() else {
            unreachable!("fixed redaction patterns are valid");
        };
        let Ok(excerpt) = redaction_policy.redact("fixture message", None) else {
            unreachable!("fixed fixture excerpt is valid");
        };
        let Ok(content) = ContentRef::bounded(
            "b3:fixture-content".into(),
            15,
            "text/plain",
            None,
            Some(excerpt),
        ) else {
            unreachable!("fixed fixture content is valid");
        };
        let Some(session_id) = SessionId::parse_wire("session_fixture") else {
            unreachable!("fixed fixture session ID is valid");
        };

        Self {
            event_id: EventId::from_source(&source_identity, 0),
            semantic_key: SemanticKey::from_native(
                Provider::Codex,
                "native-session-fixture",
                "message",
                "message-fixture",
            ),
            schema_version: 1,
            occurred_at: OffsetDateTime::UNIX_EPOCH,
            observed_at: OffsetDateTime::UNIX_EPOCH,
            project_id: ProjectId::for_test("project_fixture"),
            session_id,
            turn_id: Some("turn-fixture".into()),
            actor: Actor::Agent,
            correlation_id: None,
            causation_id: None,
            source,
            payload: EventPayload::MessageCreated { content },
            privacy: PrivacyLabel::DerivedLocal,
        }
    }
}
