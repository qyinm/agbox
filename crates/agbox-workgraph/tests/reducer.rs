#![allow(clippy::unwrap_used)]

use agbox_core::{
    ActionOutcome, ActivityEventDraft, ActivityEventV1, Actor, ContentRef, DisclosureClass,
    EventId, EventPayload, RedactionPolicy, SemanticKey, SourceIdentity,
};
use agbox_workgraph::{CommittedEvent, DeterministicReducer, ReduceError, ReducedFact};

fn content(value: &str, class: DisclosureClass, excerpt: bool) -> ContentRef {
    let redacted = excerpt.then(|| {
        RedactionPolicy::new()
            .unwrap()
            .redact(value, None, class)
            .unwrap()
    });
    ContentRef::bounded(
        format!("b3:{value}"),
        u64::try_from(value.len()).unwrap(),
        "text/plain",
        None,
        class,
        redacted,
    )
    .unwrap()
}

fn event(index: u32, actor: Actor, payload: EventPayload) -> ActivityEventV1 {
    let mut draft: ActivityEventDraft = ActivityEventV1::fixture_message_draft();
    let source = SourceIdentity {
        provider: draft.source.provider(),
        source_id: "src_fixture".into(),
        generation: 1,
        byte_offset: u64::from(index),
        record_hash: format!("b3:record-{index}"),
    };
    draft.event_id = EventId::from_source(&source, index);
    draft.semantic_key = SemanticKey::from_native(
        draft.source.provider(),
        draft.source.native_session_id(),
        "task-16",
        &index.to_string(),
    );
    draft.actor = actor;
    draft.payload = payload;
    ActivityEventV1::new(draft).unwrap()
}

fn committed(events: Vec<ActivityEventV1>) -> Vec<CommittedEvent> {
    events
        .into_iter()
        .enumerate()
        .map(|(index, event)| CommittedEvent {
            event_seq: u64::try_from(index).unwrap() + 1,
            event,
        })
        .collect()
}

#[test]
fn reducer_observes_artifacts_and_structured_verification_without_agent_claims() {
    let events = committed(vec![
        event(
            1,
            Actor::Agent,
            EventPayload::AgentStarted {
                native_agent_id: "claude-run".into(),
            },
        ),
        event(
            2,
            Actor::Tool,
            EventPayload::ArtifactChanged {
                path: content("src/lib.rs", DisclosureClass::ObservedState, true),
                operation: "update".into(),
                content_hash: Some("b3:file".into()),
            },
        ),
        event(
            3,
            Actor::Agent,
            EventPayload::ActionRequested {
                native_action_id: "cargo-test".into(),
                tool_name: "shell".into(),
                input: content("cargo test", DisclosureClass::ObservedState, true),
            },
        ),
        event(
            4,
            Actor::Tool,
            EventPayload::ActionFinished {
                native_action_id: "cargo-test".into(),
                outcome: ActionOutcome::Succeeded,
                output: Some(content("ok", DisclosureClass::ToolResult, true)),
            },
        ),
        event(
            5,
            Actor::Agent,
            EventPayload::MessageCreated {
                content: content("all tests pass", DisclosureClass::AgentStatement, true),
            },
        ),
    ]);

    let mutation = DeterministicReducer.reduce(&events).unwrap();
    assert!(mutation.facts.iter().any(|fact| matches!(
        fact,
        ReducedFact::Artifact { operation, .. } if operation == "update"
    )));
    assert!(mutation.facts.iter().any(|fact| matches!(
        fact,
        ReducedFact::Verification {
            succeeded: true,
            basis: "structured_tool_result",
            ..
        }
    )));
    assert!(
        mutation
            .facts
            .iter()
            .any(|fact| matches!(fact, ReducedFact::AgentStatement { .. }))
    );
    assert_eq!(
        mutation
            .facts
            .iter()
            .filter(|fact| matches!(fact, ReducedFact::Verification { .. }))
            .count(),
        1
    );
}

#[test]
fn unmatched_finish_is_observed_but_not_promoted_to_verification() {
    let events = committed(vec![event(
        1,
        Actor::Tool,
        EventPayload::ActionFinished {
            native_action_id: "cross-slice".into(),
            outcome: ActionOutcome::Succeeded,
            output: None,
        },
    )]);

    let mutation = DeterministicReducer.reduce(&events).unwrap();
    assert!(matches!(
        mutation.facts.as_slice(),
        [ReducedFact::ActionFinishedObserved { .. }]
    ));
}

#[test]
fn hash_only_content_never_invents_human_or_agent_prose() {
    let events = committed(vec![
        event(
            1,
            Actor::Human,
            EventPayload::MessageCreated {
                content: content("private objective", DisclosureClass::HumanIntent, false),
            },
        ),
        event(
            2,
            Actor::Agent,
            EventPayload::MessageCreated {
                content: content("private claim", DisclosureClass::AgentStatement, false),
            },
        ),
    ]);

    let mutation = DeterministicReducer.reduce(&events).unwrap();
    assert!(mutation.facts.iter().all(|fact| match fact {
        ReducedFact::HumanObjective { redacted_text, .. }
        | ReducedFact::HumanConstraint { redacted_text, .. }
        | ReducedFact::AgentStatement { redacted_text, .. } => redacted_text.is_none(),
        _ => true,
    }));
    let debug = format!("{mutation:?}");
    assert!(!debug.contains("private objective"));
    assert!(!debug.contains("private claim"));
}

#[test]
fn reducer_rejects_unordered_and_oversized_slices() {
    let one = event(
        1,
        Actor::Agent,
        EventPayload::AgentStarted {
            native_agent_id: "run".into(),
        },
    );
    let unordered = vec![
        CommittedEvent {
            event_seq: 2,
            event: one.clone(),
        },
        CommittedEvent {
            event_seq: 2,
            event: one.clone(),
        },
    ];
    assert_eq!(
        DeterministicReducer.reduce(&unordered).unwrap_err(),
        ReduceError::InvalidSequence
    );
    let oversized = (0..=agbox_core::limits::MAX_BATCH_RECORDS)
        .map(|index| CommittedEvent {
            event_seq: u64::try_from(index).unwrap() + 1,
            event: one.clone(),
        })
        .collect::<Vec<_>>();
    assert_eq!(
        DeterministicReducer.reduce(&oversized).unwrap_err(),
        ReduceError::TooManyEvents
    );
}

#[test]
fn human_actor_requires_human_intent_disclosure_for_instruction_facts() {
    let events = committed(vec![event(
        1,
        Actor::Human,
        EventPayload::MessageCreated {
            content: content(
                "agent-classified text",
                DisclosureClass::AgentStatement,
                true,
            ),
        },
    )]);

    let mutation = DeterministicReducer.reduce(&events).unwrap();
    assert!(!mutation.facts.iter().any(|fact| matches!(
        fact,
        ReducedFact::HumanObjective { .. } | ReducedFact::HumanConstraint { .. }
    )));
}

#[test]
fn action_finish_requires_tool_actor_and_tool_result_disclosure_for_verification() {
    let request = event(
        1,
        Actor::Agent,
        EventPayload::ActionRequested {
            native_action_id: "authority".into(),
            tool_name: "shell".into(),
            input: content("cargo test", DisclosureClass::ObservedState, true),
        },
    );
    let agent_finish = committed(vec![
        request.clone(),
        event(
            2,
            Actor::Agent,
            EventPayload::ActionFinished {
                native_action_id: "authority".into(),
                outcome: ActionOutcome::Succeeded,
                output: None,
            },
        ),
    ]);
    let wrong_disclosure = committed(vec![
        request,
        event(
            3,
            Actor::Tool,
            EventPayload::ActionFinished {
                native_action_id: "authority".into(),
                outcome: ActionOutcome::Succeeded,
                output: Some(content("claim", DisclosureClass::AgentStatement, true)),
            },
        ),
    ]);

    for events in [&agent_finish, &wrong_disclosure] {
        let mutation = DeterministicReducer.reduce(events).unwrap();
        assert!(
            !mutation
                .facts
                .iter()
                .any(|fact| matches!(fact, ReducedFact::Verification { .. }))
        );
        assert!(
            mutation
                .facts
                .iter()
                .any(|fact| matches!(fact, ReducedFact::ActionFinishedObserved { .. }))
        );
    }
}
