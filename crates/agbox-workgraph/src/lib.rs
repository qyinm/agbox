mod contract;
mod correlate;
mod reducer;

pub use contract::{
    ContractBuildError, ContractField, ProvisionalContract, ProvisionalContractBuilder,
};
pub use correlate::{
    CONTINUE_THRESHOLD, CorrelationDecision, CorrelationInput, CorrelationOutcome,
    CorrelationScore, CorrelationSignals, CorrelationTruncation, Correlator, MAX_ARTIFACT_HASHES,
    MAX_CANDIDATES, MAX_COMMAND_HASHES, MIN_NON_SEMANTIC_SCORE, WorkAssociation, WorkCandidate,
    score,
};
pub use reducer::{CommittedEvent, DeterministicReducer, GraphMutation, ReduceError, ReducedFact};

#[cfg(feature = "test-support")]
pub mod test_support {
    use agbox_core::{EventId, ProjectId, Provider, SessionId};
    use time::macros::datetime;

    use crate::ReducedFact;

    #[must_use]
    pub fn facts_for_active_parser_work() -> Vec<ReducedFact> {
        let project_id = ProjectId::for_test("project-a");
        let session_id = SessionId::parse_wire("session-a")
            .unwrap_or_else(|| unreachable!("the static fixture session ID is valid"));
        vec![
            ReducedFact::HumanObjective {
                project_id: project_id.clone(),
                content_hash: "b3:objective".into(),
                redacted_text: Some("Implement the parser".into()),
                evidence: event_id("evt-objective"),
            },
            ReducedFact::AgentRunStarted {
                project_id: project_id.clone(),
                session_id: session_id.clone(),
                provider: Provider::Codex,
                native_agent_id: "codex-run".into(),
                observed_at: datetime!(2026-07-17 11:58 UTC),
                evidence: event_id("evt-started"),
            },
            ReducedFact::Artifact {
                project_id: project_id.clone(),
                session_id: session_id.clone(),
                path_hash: "b3:parser".into(),
                project_relative_path: Some("src/parser.rs".into()),
                operation: "update".into(),
                content_hash: Some("b3:content".into()),
                observed_at: datetime!(2026-07-17 12:00 UTC),
                evidence: event_id("evt-artifact"),
            },
            ReducedFact::ActionRequested {
                project_id: project_id.clone(),
                session_id,
                native_action_id: "next-test".into(),
                tool_name: "shell".into(),
                input_hash: "b3:test".into(),
                redacted_input: Some("cargo test parser".into()),
                evidence: event_id("evt-next-action"),
            },
            ReducedFact::AgentStatement {
                project_id,
                content_hash: "b3:summary".into(),
                redacted_text: Some("Parser implementation is in progress".into()),
                evidence: event_id("evt-summary"),
            },
        ]
    }

    fn event_id(value: &str) -> EventId {
        EventId::parse_wire(value)
            .unwrap_or_else(|| unreachable!("the static fixture event ID is valid"))
    }
}
