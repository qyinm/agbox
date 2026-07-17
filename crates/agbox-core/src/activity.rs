use std::fmt;

use serde::{Deserialize, Deserializer, Serialize, de};
use time::OffsetDateTime;

use crate::{
    ContentError, ContentRef, DisclosureClass, EventId, PrivacyLabel, ProjectId, Provider,
    RedactionPolicy, SemanticKey, SessionId, SourceError, SourceIdentity, SourceRef,
    SourceRefDraft, limits::MAX_INLINE_BYTES,
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

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
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

#[derive(Deserialize)]
#[serde(tag = "kind")]
enum EventPayloadWire {
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

impl From<EventPayloadWire> for EventPayload {
    fn from(wire: EventPayloadWire) -> Self {
        match wire {
            EventPayloadWire::SessionStarted { context } => Self::SessionStarted { context },
            EventPayloadWire::SessionContextChanged {
                context,
                branch_hash,
            } => Self::SessionContextChanged {
                context,
                branch_hash,
            },
            EventPayloadWire::TurnStarted { prompt_id } => Self::TurnStarted { prompt_id },
            EventPayloadWire::TurnFinished { outcome } => Self::TurnFinished { outcome },
            EventPayloadWire::MessageCreated { content } => Self::MessageCreated { content },
            EventPayloadWire::ActionRequested {
                native_action_id,
                tool_name,
                input,
            } => Self::ActionRequested {
                native_action_id,
                tool_name,
                input,
            },
            EventPayloadWire::ActionFinished {
                native_action_id,
                outcome,
                output,
            } => Self::ActionFinished {
                native_action_id,
                outcome,
                output,
            },
            EventPayloadWire::ArtifactChanged {
                path,
                operation,
                content_hash,
            } => Self::ArtifactChanged {
                path,
                operation,
                content_hash,
            },
            EventPayloadWire::PlanObserved { plan } => Self::PlanObserved { plan },
            EventPayloadWire::AgentStarted { native_agent_id } => {
                Self::AgentStarted { native_agent_id }
            }
            EventPayloadWire::AgentFinished {
                native_agent_id,
                outcome,
            } => Self::AgentFinished {
                native_agent_id,
                outcome,
            },
            EventPayloadWire::ContextCompacted { summary_hash } => {
                Self::ContextCompacted { summary_hash }
            }
            EventPayloadWire::DiagnosticObserved { level, message } => {
                Self::DiagnosticObserved { level, message }
            }
        }
    }
}

impl<'de> Deserialize<'de> for EventPayload {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let payload = Self::from(EventPayloadWire::deserialize(deserializer)?);
        payload.validate().map_err(de::Error::custom)?;
        Ok(payload)
    }
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

    /// Revalidates standalone payload text and nested content references.
    ///
    /// # Errors
    ///
    /// Returns [`ActivityError`] when payload text or nested content violates
    /// an activity invariant.
    pub fn validate(&self) -> Result<(), ActivityError> {
        match self {
            Self::SessionStarted { context } => validate_optional_content(context.as_ref()),
            Self::SessionContextChanged {
                context,
                branch_hash,
            } => {
                context.validate()?;
                validate_optional_event_text("branch_hash", branch_hash.as_ref())
            }
            Self::TurnStarted { prompt_id } => {
                validate_optional_event_text("prompt_id", prompt_id.as_ref())
            }
            Self::TurnFinished { .. } => Ok(()),
            Self::MessageCreated { content } => content.validate().map_err(ActivityError::from),
            Self::ActionRequested {
                native_action_id,
                tool_name,
                input,
            } => {
                validate_event_text("native_action_id", native_action_id)?;
                validate_event_text("tool_name", tool_name)?;
                input.validate().map_err(ActivityError::from)
            }
            Self::ActionFinished {
                native_action_id,
                output,
                ..
            } => {
                validate_event_text("native_action_id", native_action_id)?;
                validate_optional_content(output.as_ref())
            }
            Self::ArtifactChanged {
                path,
                operation,
                content_hash,
            } => {
                path.validate()?;
                validate_event_text("operation", operation)?;
                validate_optional_event_text("content_hash", content_hash.as_ref())
            }
            Self::PlanObserved { plan } => plan.validate().map_err(ActivityError::from),
            Self::AgentStarted { native_agent_id }
            | Self::AgentFinished {
                native_agent_id, ..
            } => validate_event_text("native_agent_id", native_agent_id),
            Self::ContextCompacted { summary_hash } => {
                validate_optional_event_text("summary_hash", summary_hash.as_ref())
            }
            Self::DiagnosticObserved { level, message } => {
                validate_event_text("level", level)?;
                message.validate().map_err(ActivityError::from)
            }
        }
    }
}

fn validate_optional_content(content: Option<&ContentRef>) -> Result<(), ActivityError> {
    if let Some(content) = content {
        content.validate()?;
    }
    Ok(())
}

#[derive(Clone)]
pub struct ActivityEventDraft {
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

impl fmt::Debug for ActivityEventDraft {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        SanitizedEventDebug::from_draft(self).fmt(formatter)
    }
}

#[derive(Clone, Eq, PartialEq, Serialize)]
pub struct ActivityEventV1 {
    event_id: EventId,
    semantic_key: SemanticKey,
    schema_version: u16,
    occurred_at: OffsetDateTime,
    observed_at: OffsetDateTime,
    project_id: ProjectId,
    session_id: SessionId,
    turn_id: Option<String>,
    actor: Actor,
    correlation_id: Option<String>,
    causation_id: Option<String>,
    source: SourceRef,
    payload: EventPayload,
    privacy: PrivacyLabel,
}

impl fmt::Debug for ActivityEventV1 {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        SanitizedEventDebug::from_event(self).fmt(formatter)
    }
}

struct SanitizedEventDebug<'a> {
    name: &'static str,
    event_id: &'a EventId,
    semantic_key: &'a SemanticKey,
    project_id: &'a ProjectId,
    session_id: &'a SessionId,
    payload: &'a EventPayload,
    privacy: PrivacyLabel,
}

impl<'a> SanitizedEventDebug<'a> {
    fn from_draft(draft: &'a ActivityEventDraft) -> Self {
        Self {
            name: "ActivityEventDraft",
            event_id: &draft.event_id,
            semantic_key: &draft.semantic_key,
            project_id: &draft.project_id,
            session_id: &draft.session_id,
            payload: &draft.payload,
            privacy: draft.privacy,
        }
    }

    fn from_event(event: &'a ActivityEventV1) -> Self {
        Self {
            name: "ActivityEventV1",
            event_id: &event.event_id,
            semantic_key: &event.semantic_key,
            project_id: &event.project_id,
            session_id: &event.session_id,
            payload: &event.payload,
            privacy: event.privacy,
        }
    }
}

impl fmt::Debug for SanitizedEventDebug<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct(self.name)
            .field("event_id", self.event_id)
            .field("semantic_key", self.semantic_key)
            .field("project_id", self.project_id)
            .field("session_id", self.session_id)
            .field("event_kind", &self.payload.kind())
            .field("privacy", &self.privacy)
            .finish_non_exhaustive()
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ActivityError {
    #[error("activity schema version must be exactly 1")]
    InvalidSchemaVersion,
    #[error("{0} exceeds the inline-content bound")]
    TextTooLarge(&'static str),
    #[error("event source is invalid")]
    InvalidSource(#[from] SourceError),
    #[error("event content reference is invalid")]
    InvalidContent(#[from] ContentError),
}

impl ActivityEventV1 {
    /// Constructs an immutable, validated activity event.
    ///
    /// # Errors
    ///
    /// Returns [`ActivityError`] when schema, native strings, source metadata,
    /// or nested content violates an event invariant.
    pub fn new(draft: ActivityEventDraft) -> Result<Self, ActivityError> {
        let event = Self {
            event_id: draft.event_id,
            semantic_key: draft.semantic_key,
            schema_version: draft.schema_version,
            occurred_at: draft.occurred_at,
            observed_at: draft.observed_at,
            project_id: draft.project_id,
            session_id: draft.session_id,
            turn_id: draft.turn_id,
            actor: draft.actor,
            correlation_id: draft.correlation_id,
            causation_id: draft.causation_id,
            source: draft.source,
            payload: draft.payload,
            privacy: draft.privacy,
        };
        event.validate()?;
        Ok(event)
    }

    /// Revalidates the activity event before a store write.
    ///
    /// # Errors
    ///
    /// Returns [`ActivityError`] when an event invariant is violated.
    pub fn validate(&self) -> Result<(), ActivityError> {
        if self.schema_version != 1 {
            return Err(ActivityError::InvalidSchemaVersion);
        }
        validate_optional_event_text("turn_id", self.turn_id.as_ref())?;
        validate_optional_event_text("correlation_id", self.correlation_id.as_ref())?;
        validate_optional_event_text("causation_id", self.causation_id.as_ref())?;
        self.source.validate()?;
        self.payload.validate()
    }

    #[must_use]
    pub fn event_id(&self) -> &EventId {
        &self.event_id
    }

    #[must_use]
    pub fn semantic_key(&self) -> &SemanticKey {
        &self.semantic_key
    }

    #[must_use]
    pub fn schema_version(&self) -> u16 {
        self.schema_version
    }

    #[must_use]
    pub fn occurred_at(&self) -> OffsetDateTime {
        self.occurred_at
    }

    #[must_use]
    pub fn observed_at(&self) -> OffsetDateTime {
        self.observed_at
    }

    #[must_use]
    pub fn project_id(&self) -> &ProjectId {
        &self.project_id
    }

    #[must_use]
    pub fn session_id(&self) -> &SessionId {
        &self.session_id
    }

    #[must_use]
    pub fn turn_id(&self) -> Option<&str> {
        self.turn_id.as_deref()
    }

    #[must_use]
    pub fn actor(&self) -> Actor {
        self.actor
    }

    #[must_use]
    pub fn correlation_id(&self) -> Option<&str> {
        self.correlation_id.as_deref()
    }

    #[must_use]
    pub fn causation_id(&self) -> Option<&str> {
        self.causation_id.as_deref()
    }

    #[must_use]
    pub fn source(&self) -> &SourceRef {
        &self.source
    }

    #[must_use]
    pub fn payload(&self) -> &EventPayload {
        &self.payload
    }

    #[must_use]
    pub fn privacy(&self) -> PrivacyLabel {
        self.privacy
    }
}

fn validate_event_text(field: &'static str, value: &str) -> Result<(), ActivityError> {
    if value.len() > MAX_INLINE_BYTES {
        return Err(ActivityError::TextTooLarge(field));
    }
    Ok(())
}

fn validate_optional_event_text(
    field: &'static str,
    value: Option<&String>,
) -> Result<(), ActivityError> {
    if let Some(value) = value {
        validate_event_text(field, value)?;
    }
    Ok(())
}

#[derive(Deserialize)]
struct ActivityEventWire {
    event_id: EventId,
    semantic_key: SemanticKey,
    schema_version: u16,
    occurred_at: OffsetDateTime,
    observed_at: OffsetDateTime,
    project_id: ProjectId,
    session_id: SessionId,
    turn_id: Option<String>,
    actor: Actor,
    correlation_id: Option<String>,
    causation_id: Option<String>,
    source: SourceRef,
    payload: EventPayload,
    privacy: PrivacyLabel,
}

impl<'de> Deserialize<'de> for ActivityEventV1 {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = ActivityEventWire::deserialize(deserializer)?;
        Self::new(ActivityEventDraft {
            event_id: wire.event_id,
            semantic_key: wire.semantic_key,
            schema_version: wire.schema_version,
            occurred_at: wire.occurred_at,
            observed_at: wire.observed_at,
            project_id: wire.project_id,
            session_id: wire.session_id,
            turn_id: wire.turn_id,
            actor: wire.actor,
            correlation_id: wire.correlation_id,
            causation_id: wire.causation_id,
            source: wire.source,
            payload: wire.payload,
            privacy: wire.privacy,
        })
        .map_err(de::Error::custom)
    }
}

#[cfg(any(test, feature = "test-support"))]
impl ActivityEventV1 {
    #[must_use]
    pub fn fixture_message_draft() -> ActivityEventDraft {
        let source_identity = SourceIdentity {
            provider: Provider::Codex,
            source_id: "src_fixture".into(),
            generation: 1,
            byte_offset: 64,
            record_hash: "b3:fixture-record".into(),
        };
        let Ok(source) = SourceRef::new(SourceRefDraft {
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
        }) else {
            unreachable!("fixed fixture source is valid");
        };
        let Ok(redaction_policy) = RedactionPolicy::new() else {
            unreachable!("fixed redaction patterns are valid");
        };
        let Ok(excerpt) =
            redaction_policy.redact("fixture message", None, DisclosureClass::AgentStatement)
        else {
            unreachable!("fixed fixture excerpt is valid");
        };
        let Ok(content) = ContentRef::bounded(
            "b3:fixture-content".into(),
            15,
            "text/plain",
            None,
            DisclosureClass::AgentStatement,
            Some(excerpt),
        ) else {
            unreachable!("fixed fixture content is valid");
        };
        let Some(session_id) = SessionId::parse_wire("session_fixture") else {
            unreachable!("fixed fixture session ID is valid");
        };

        ActivityEventDraft {
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

    #[must_use]
    pub fn fixture_message() -> Self {
        let Ok(event) = Self::new(Self::fixture_message_draft()) else {
            unreachable!("fixed fixture event is valid");
        };
        event
    }
}
