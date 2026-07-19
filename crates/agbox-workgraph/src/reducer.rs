use std::{collections::BTreeMap, fmt};

use agbox_core::{
    ActionOutcome, ActivityEventV1, Actor, DisclosureClass, EventId, EventPayload, ProjectId,
    Provider, SessionId,
};
use time::OffsetDateTime;

const MAX_EVENTS: usize = agbox_core::limits::MAX_BATCH_RECORDS;
const MAX_SEMANTIC_BYTES: usize = agbox_core::limits::MAX_BATCH_SEMANTIC_BYTES;

#[derive(Clone, Debug)]
pub struct CommittedEvent {
    pub event_seq: u64,
    pub event: ActivityEventV1,
}

#[derive(Clone, Eq, PartialEq)]
pub enum ReducedFact {
    AgentRunStarted {
        project_id: ProjectId,
        session_id: SessionId,
        provider: Provider,
        native_agent_id: String,
        observed_at: OffsetDateTime,
        evidence: EventId,
    },
    AgentRunFinished {
        project_id: ProjectId,
        session_id: SessionId,
        provider: Provider,
        native_agent_id: String,
        succeeded: bool,
        observed_at: OffsetDateTime,
        evidence: EventId,
    },
    SessionContext {
        project_id: ProjectId,
        session_id: SessionId,
        provider: Provider,
        branch_hash: Option<String>,
        observed_at: OffsetDateTime,
        evidence: EventId,
    },
    Artifact {
        project_id: ProjectId,
        session_id: SessionId,
        path_hash: String,
        project_relative_path: Option<String>,
        operation: String,
        content_hash: Option<String>,
        observed_at: OffsetDateTime,
        evidence: EventId,
    },
    ActionRequested {
        project_id: ProjectId,
        session_id: SessionId,
        native_action_id: String,
        tool_name: String,
        input_hash: String,
        redacted_input: Option<String>,
        observed_at: OffsetDateTime,
        evidence: EventId,
    },
    ActionFinishedObserved {
        project_id: ProjectId,
        session_id: SessionId,
        native_action_id: String,
        succeeded: bool,
        observed_at: OffsetDateTime,
        evidence: EventId,
    },
    EligibleVerificationObserved {
        project_id: ProjectId,
        session_id: SessionId,
        native_action_id: String,
        succeeded: bool,
        basis: &'static str,
        observed_at: OffsetDateTime,
        evidence: EventId,
    },
    Verification {
        project_id: ProjectId,
        session_id: SessionId,
        native_action_id: String,
        command: Option<String>,
        succeeded: bool,
        basis: &'static str,
        observed_at: OffsetDateTime,
        evidence: EventId,
    },
    HumanObjective {
        project_id: ProjectId,
        content_hash: String,
        redacted_text: Option<String>,
        observed_at: OffsetDateTime,
        evidence: EventId,
    },
    HumanConstraint {
        project_id: ProjectId,
        content_hash: String,
        redacted_text: Option<String>,
        observed_at: OffsetDateTime,
        evidence: EventId,
    },
    AgentStatement {
        project_id: ProjectId,
        content_hash: String,
        redacted_text: Option<String>,
        observed_at: OffsetDateTime,
        evidence: EventId,
    },
}

impl fmt::Debug for ReducedFact {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ReducedFact")
            .field("kind", &self.kind())
            .field("evidence", self.evidence_id())
            .finish_non_exhaustive()
    }
}

impl ReducedFact {
    fn kind(&self) -> &'static str {
        match self {
            Self::AgentRunStarted { .. } => "agent_run_started",
            Self::AgentRunFinished { .. } => "agent_run_finished",
            Self::SessionContext { .. } => "session_context",
            Self::Artifact { .. } => "artifact",
            Self::ActionRequested { .. } => "action_requested",
            Self::ActionFinishedObserved { .. } => "action_finished_observed",
            Self::EligibleVerificationObserved { .. } => "eligible_verification_observed",
            Self::Verification { .. } => "verification",
            Self::HumanObjective { .. } => "human_objective",
            Self::HumanConstraint { .. } => "human_constraint",
            Self::AgentStatement { .. } => "agent_statement",
        }
    }

    #[must_use]
    pub fn evidence_id(&self) -> &EventId {
        match self {
            Self::AgentRunStarted { evidence, .. }
            | Self::AgentRunFinished { evidence, .. }
            | Self::SessionContext { evidence, .. }
            | Self::Artifact { evidence, .. }
            | Self::ActionRequested { evidence, .. }
            | Self::ActionFinishedObserved { evidence, .. }
            | Self::EligibleVerificationObserved { evidence, .. }
            | Self::Verification { evidence, .. }
            | Self::HumanObjective { evidence, .. }
            | Self::HumanConstraint { evidence, .. }
            | Self::AgentStatement { evidence, .. } => evidence,
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct GraphMutation {
    pub facts: Vec<ReducedFact>,
    pub expected_event_seq: u64,
    pub through_event_seq: Option<u64>,
    pub through_event_id: Option<EventId>,
}

#[derive(Debug, Default)]
pub struct DeterministicReducer;

impl DeterministicReducer {
    /// Reduces one ordered, bounded slice of committed immutable events.
    ///
    /// # Errors
    ///
    /// Returns a bounded error when the slice is too large, is not contiguous,
    /// or contains an event that no longer satisfies the core event contract.
    #[allow(clippy::too_many_lines)]
    pub fn reduce(&self, events: &[CommittedEvent]) -> Result<GraphMutation, ReduceError> {
        validate_slice(events)?;
        let mut mutation = GraphMutation {
            facts: Vec::with_capacity(events.len()),
            expected_event_seq: events
                .first()
                .map_or(0, |committed| committed.event_seq - 1),
            through_event_seq: None,
            through_event_id: None,
        };
        let mut requests = BTreeMap::<(String, String, String), Option<String>>::new();

        for committed in events {
            let event = &committed.event;
            let evidence = event.event_id().clone();
            let project_id = event.project_id().clone();
            let session_id = event.session_id().clone();
            let observed_at = event.observed_at();
            match event.payload() {
                EventPayload::AgentStarted { native_agent_id } => {
                    mutation.facts.push(ReducedFact::AgentRunStarted {
                        project_id,
                        session_id,
                        provider: event.source().provider(),
                        native_agent_id: native_agent_id.clone(),
                        observed_at,
                        evidence,
                    });
                }
                EventPayload::AgentFinished {
                    native_agent_id,
                    outcome,
                } => {
                    mutation.facts.push(ReducedFact::AgentRunFinished {
                        project_id,
                        session_id,
                        provider: event.source().provider(),
                        native_agent_id: native_agent_id.clone(),
                        succeeded: succeeded(*outcome),
                        observed_at,
                        evidence,
                    });
                }
                EventPayload::SessionContextChanged { branch_hash, .. } => {
                    mutation.facts.push(ReducedFact::SessionContext {
                        project_id,
                        session_id,
                        provider: event.source().provider(),
                        branch_hash: branch_hash.clone(),
                        observed_at,
                        evidence,
                    });
                }
                EventPayload::ArtifactChanged {
                    path,
                    operation,
                    content_hash,
                } => {
                    mutation.facts.push(ReducedFact::Artifact {
                        project_id,
                        session_id,
                        path_hash: path.hash().to_owned(),
                        project_relative_path: nonempty(path.redacted_excerpt()),
                        operation: operation.clone(),
                        content_hash: content_hash.clone(),
                        observed_at,
                        evidence,
                    });
                }
                EventPayload::ActionRequested {
                    native_action_id,
                    tool_name,
                    input,
                } => {
                    requests.insert(
                        action_key(event, native_action_id),
                        nonempty(input.redacted_excerpt()),
                    );
                    mutation.facts.push(ReducedFact::ActionRequested {
                        project_id,
                        session_id,
                        native_action_id: native_action_id.clone(),
                        tool_name: tool_name.clone(),
                        input_hash: input.hash().to_owned(),
                        redacted_input: nonempty(input.redacted_excerpt()),
                        observed_at,
                        evidence,
                    });
                }
                EventPayload::ActionFinished {
                    native_action_id,
                    outcome,
                    output,
                } => {
                    let key = action_key(event, native_action_id);
                    let authorized_result = event.actor() == Actor::Tool
                        && output.as_ref().is_none_or(|content| {
                            content.disclosure_class() == DisclosureClass::ToolResult
                        });
                    if authorized_result {
                        if let Some(command) = requests.get(&key) {
                            mutation.facts.push(ReducedFact::Verification {
                                project_id,
                                session_id,
                                native_action_id: native_action_id.clone(),
                                command: command.clone(),
                                succeeded: succeeded(*outcome),
                                basis: "structured_tool_result",
                                observed_at,
                                evidence,
                            });
                        } else {
                            mutation
                                .facts
                                .push(ReducedFact::EligibleVerificationObserved {
                                    project_id,
                                    session_id,
                                    native_action_id: native_action_id.clone(),
                                    succeeded: succeeded(*outcome),
                                    basis: "structured_tool_result",
                                    observed_at,
                                    evidence,
                                });
                        }
                    } else {
                        mutation.facts.push(ReducedFact::ActionFinishedObserved {
                            project_id,
                            session_id,
                            native_action_id: native_action_id.clone(),
                            succeeded: succeeded(*outcome),
                            observed_at,
                            evidence,
                        });
                    }
                }
                EventPayload::MessageCreated { content }
                    if event.actor() == Actor::Human
                        && content.disclosure_class() == DisclosureClass::HumanIntent =>
                {
                    let redacted_text = nonempty(content.redacted_excerpt());
                    let fact = if redacted_text.as_deref().is_some_and(is_constraint) {
                        ReducedFact::HumanConstraint {
                            project_id,
                            content_hash: content.hash().to_owned(),
                            redacted_text,
                            observed_at,
                            evidence,
                        }
                    } else {
                        ReducedFact::HumanObjective {
                            project_id,
                            content_hash: content.hash().to_owned(),
                            redacted_text,
                            observed_at,
                            evidence,
                        }
                    };
                    mutation.facts.push(fact);
                }
                EventPayload::MessageCreated { content } if event.actor() == Actor::Agent => {
                    mutation.facts.push(ReducedFact::AgentStatement {
                        project_id,
                        content_hash: content.hash().to_owned(),
                        redacted_text: nonempty(content.redacted_excerpt()),
                        observed_at,
                        evidence,
                    });
                }
                _ => {}
            }
            mutation.through_event_seq = Some(committed.event_seq);
            mutation.through_event_id = Some(event.event_id().clone());
        }
        Ok(mutation)
    }
}

fn validate_slice(events: &[CommittedEvent]) -> Result<(), ReduceError> {
    if events.len() > MAX_EVENTS {
        return Err(ReduceError::TooManyEvents);
    }
    let mut semantic_bytes = 0_usize;
    let mut previous = None;
    for committed in events {
        if committed.event_seq == 0
            || previous.is_some_and(|value: u64| value.checked_add(1) != Some(committed.event_seq))
        {
            return Err(ReduceError::InvalidSequence);
        }
        committed
            .event
            .validate()
            .map_err(|_| ReduceError::InvalidEvent)?;
        semantic_bytes = semantic_bytes
            .checked_add(size_of::<u64>())
            .and_then(|total| {
                serde_json::to_vec(&committed.event)
                    .ok()
                    .and_then(|event| total.checked_add(event.len()))
            })
            .ok_or(ReduceError::TooManyBytes)?;
        if semantic_bytes > MAX_SEMANTIC_BYTES {
            return Err(ReduceError::TooManyBytes);
        }
        previous = Some(committed.event_seq);
    }
    Ok(())
}

fn action_key(event: &ActivityEventV1, native_action_id: &str) -> (String, String, String) {
    (
        event.project_id().as_str().to_owned(),
        event.session_id().as_str().to_owned(),
        native_action_id.to_owned(),
    )
}

fn nonempty(value: Option<&str>) -> Option<String> {
    value
        .filter(|value| !value.trim().is_empty())
        .map(str::to_owned)
}

fn succeeded(outcome: ActionOutcome) -> bool {
    outcome == ActionOutcome::Succeeded
}

fn is_constraint(value: &str) -> bool {
    let value = value.trim_start().to_ascii_lowercase();
    ["do not ", "don't ", "must not ", "never ", "constraint:"]
        .iter()
        .any(|prefix| value.starts_with(prefix))
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum ReduceError {
    #[error("committed event sequence is invalid")]
    InvalidSequence,
    #[error("committed event slice exceeds the event bound")]
    TooManyEvents,
    #[error("committed event slice exceeds the semantic byte bound")]
    TooManyBytes,
    #[error("committed event violates the activity contract")]
    InvalidEvent,
}
