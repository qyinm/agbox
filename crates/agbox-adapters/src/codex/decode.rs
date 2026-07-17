use std::{
    collections::HashSet,
    path::{Component, Path},
};

use agbox_core::{
    ActionOutcome, ActivityEventDraft, ActivityEventV1, Actor, ByteRange, ContentRef, DecodeStatus,
    DisclosureClass, EventId, EventPayload, EvidenceId, LocalLocator, PrivacyLabel, Provider,
    RedactionPolicy, SemanticKey, SessionId, SourceIdentity, SourceObservation,
    SourceObservationDraft, SourceRef, SourceRefDraft,
};
use time::{
    Date, Month, OffsetDateTime, PrimitiveDateTime, Time, format_description::well_known::Rfc3339,
};
use zeroize::{Zeroize, Zeroizing};

use crate::{
    BoundedJsonReader, DecodeContext, DecodeDisposition, DecodeError, DecodedEvidence,
    DecodedRecord, DecodedRecordDraft, DecoderState, MAX_CAPTURE_BYTES, MAX_EVENTS_PER_RECORD,
    MAX_EVIDENCE_PER_RECORD, RecordSource, RootClass, RootSpec, SourceAdapter,
    json::{CapturedMatch, CapturedValue, SecureCapturedString},
};

use super::state::{
    CallLink, CodexStateV1, HistoryMode, PendingResult, StagedArtifact, StagedContent,
    bounded_identifier,
};

const DECODER_VERSION: &str = "codex-rollout-1";
const MAX_ID_BYTES: usize = 128;
const MAX_TOOL_NAME_BYTES: usize = 64;
const MAX_PATH_BYTES: usize = 512;
const MAX_MATCHES: usize = MAX_EVENTS_PER_RECORD;

pub struct CodexAdapter;

impl std::fmt::Debug for CodexAdapter {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("CodexAdapter")
    }
}

impl SourceAdapter for CodexAdapter {
    fn provider(&self) -> Provider {
        Provider::Codex
    }

    fn decoder_version(&self) -> &'static str {
        DECODER_VERSION
    }

    fn roots(&self, home: &Path) -> Vec<RootSpec> {
        vec![
            RootSpec {
                path: home.join(".codex").join("sessions"),
                class: RootClass::Active,
                recursive: true,
            },
            RootSpec {
                path: home.join(".codex").join("archived_sessions"),
                class: RootClass::Archive,
                recursive: true,
            },
        ]
    }

    fn matches(&self, root: &RootSpec, relative: &Path) -> bool {
        if !root.recursive
            || relative.as_os_str().is_empty()
            || relative.is_absolute()
            || !relative
                .components()
                .all(|component| matches!(component, Component::Normal(_)))
            || relative
                .extension()
                .is_none_or(|extension| extension != "jsonl")
        {
            return false;
        }
        std::fs::symlink_metadata(root.path.join(relative))
            .is_ok_and(|metadata| metadata.file_type().is_file())
    }

    fn trusted_session_time(
        &self,
        root: &RootSpec,
        relative: &Path,
        _mtime: OffsetDateTime,
    ) -> Option<OffsetDateTime> {
        match root.class {
            RootClass::Active => active_hierarchy_date(relative),
            RootClass::Archive => archive_rollout_date(relative),
        }
    }

    #[allow(clippy::too_many_lines)]
    fn decode(
        &self,
        record: &dyn RecordSource,
        context: &DecodeContext,
        prior_state: &DecoderState,
    ) -> Result<DecodedRecord, DecodeError> {
        validate_context(context)?;
        let (top_type_capture, schema_fingerprint) =
            capture_single_string(record, &["type"], MAX_ID_BYTES, false)?;
        let top_type = top_type_capture
            .ok_or(DecodeError::MissingIdentity("type"))
            .and_then(|value| strict_identifier(value, MAX_ID_BYTES, "type"))?;
        let ordinal = match capture_optional_u64(record, &["ordinal"]) {
            Ok(value) => value,
            Err(error) => {
                return malformed_envelope(
                    record,
                    context,
                    prior_state,
                    &top_type,
                    &schema_fingerprint,
                    error,
                );
            }
        };
        let session_id = session_id(context);
        let source = make_source(record, context, &session_id, &top_type, ordinal)?;
        let mut progression_state = CodexStateV1::decode(prior_state)?;
        if let Err(error) = progress_envelope(&top_type, ordinal, &mut progression_state) {
            let observation = make_observation(
                record,
                context,
                source,
                &schema_fingerprint,
                DecodeStatus::Malformed,
            )?;
            return Ok(classified_empty(
                observation,
                DecodeDisposition::malformed(error_class(&error)),
                prior_state,
            ));
        }
        let progressed_decoder_state = progression_state.clone().encode_bounded()?;

        if !known_top_type(&top_type) {
            let observation = make_observation(
                record,
                context,
                source,
                &schema_fingerprint,
                DecodeStatus::UnknownType,
            )?;
            return Ok(record_with(
                observation,
                DecodeDisposition::unknown_type(&top_type),
                Vec::new(),
                Vec::new(),
                progressed_decoder_state,
                prior_state,
            ));
        }

        let nested_type = match top_type.as_str() {
            "response_item" | "event_msg" => {
                match capture_required_nested_type(record, &["payload", "type"]) {
                    Ok(value) => Some(value),
                    Err(error) => {
                        let observation = make_observation(
                            record,
                            context,
                            source,
                            &schema_fingerprint,
                            DecodeStatus::Malformed,
                        )?;
                        return Ok(empty_with_next_state(
                            observation,
                            DecodeDisposition::malformed(error_class(&error)),
                            progressed_decoder_state,
                            prior_state,
                        ));
                    }
                }
            }
            _ => None,
        };
        if let Some(nested) = nested_type.as_deref()
            && !known_nested_type(&top_type, nested)
        {
            let observation = make_observation(
                record,
                context,
                source,
                &schema_fingerprint,
                DecodeStatus::UnknownType,
            )?;
            return Ok(record_with(
                observation,
                DecodeDisposition::unknown_type(nested),
                Vec::new(),
                Vec::new(),
                progressed_decoder_state,
                prior_state,
            ));
        }

        let mut state = progression_state;
        let mut output = Output::default();
        let mut retained = RetainedBudget::default();
        let result = (|| {
            observe_known_history_mode(record, &mut state)?;
            let timestamp = capture_timestamp(record)?.unwrap_or(context.observed_at);
            let mut scope = Scope {
                context,
                source: &source,
                session_id: &session_id,
                occurred_at: timestamp,
            };
            match top_type.as_str() {
                "session_meta" => decode_session_meta(record, scope, &mut output, &mut retained),
                "turn_context" => decode_turn_context(record, scope, &mut output, &mut retained),
                "compacted" => {
                    reconcile_pending_results(scope, &mut state, &mut output, 1)?;
                    decode_compaction(record, scope, &mut output)
                }
                "response_item" => decode_response_item(
                    record,
                    &mut scope,
                    nested_type.as_deref().unwrap_or_default(),
                    &mut state,
                    &mut output,
                    &mut retained,
                ),
                "event_msg" => decode_event_message(
                    record,
                    &mut scope,
                    nested_type.as_deref().unwrap_or_default(),
                    &mut state,
                    &mut output,
                    &mut retained,
                ),
                _ => Ok(()),
            }
        })();

        match result {
            Ok(()) => {
                let observation = make_observation(
                    record,
                    context,
                    source,
                    &schema_fingerprint,
                    DecodeStatus::Known,
                )?;
                let next_state = state.encode_bounded()?;
                Ok(record_with(
                    observation,
                    DecodeDisposition::Known,
                    output.events,
                    output.evidence,
                    next_state,
                    prior_state,
                ))
            }
            Err(DecodeError::Io(error)) => Err(DecodeError::Io(error)),
            Err(DecodeError::OutputTooLarge | DecodeError::StateTooLarge) => {
                let observation = make_observation(
                    record,
                    context,
                    source,
                    &schema_fingerprint,
                    DecodeStatus::Oversized,
                )?;
                Ok(empty_with_next_state(
                    observation,
                    DecodeDisposition::oversized("codex-output"),
                    progressed_decoder_state,
                    prior_state,
                ))
            }
            Err(error) => {
                let observation = make_observation(
                    record,
                    context,
                    source,
                    &schema_fingerprint,
                    DecodeStatus::Malformed,
                )?;
                Ok(empty_with_next_state(
                    observation,
                    DecodeDisposition::malformed(error_class(&error)),
                    progressed_decoder_state,
                    prior_state,
                ))
            }
        }
    }
}

#[derive(Clone, Copy)]
struct Scope<'a> {
    context: &'a DecodeContext,
    source: &'a SourceRef,
    session_id: &'a str,
    occurred_at: OffsetDateTime,
}

#[derive(Default)]
struct Output {
    events: Vec<ActivityEventV1>,
    evidence: Vec<DecodedEvidence>,
    next_event_ordinal: u32,
    next_evidence_ordinal: u32,
}

impl Output {
    fn push_event(&mut self, event: ActivityEventV1) -> Result<(), DecodeError> {
        if self.events.len() >= MAX_EVENTS_PER_RECORD {
            return Err(DecodeError::OutputTooLarge);
        }
        self.events.push(event);
        Ok(())
    }

    fn push_evidence(&mut self, evidence: DecodedEvidence) -> Result<(), DecodeError> {
        if self.evidence.len() >= MAX_EVIDENCE_PER_RECORD {
            return Err(DecodeError::OutputTooLarge);
        }
        self.evidence.push(evidence);
        Ok(())
    }

    fn event_id(&mut self, scope: Scope<'_>) -> Result<EventId, DecodeError> {
        let ordinal = self.next_event_ordinal;
        self.next_event_ordinal = self
            .next_event_ordinal
            .checked_add(1)
            .ok_or(DecodeError::OutputTooLarge)?;
        Ok(EventId::from_source(&source_identity(scope), ordinal))
    }

    fn evidence_ordinal(&mut self) -> Result<u32, DecodeError> {
        let ordinal = self.next_evidence_ordinal;
        self.next_evidence_ordinal = self
            .next_evidence_ordinal
            .checked_add(1)
            .ok_or(DecodeError::OutputTooLarge)?;
        Ok(ordinal)
    }
}

struct RetainedBudget {
    remaining: usize,
}

impl Default for RetainedBudget {
    fn default() -> Self {
        Self {
            remaining: MAX_CAPTURE_BYTES,
        }
    }
}

impl RetainedBudget {
    const fn remaining(&self) -> usize {
        self.remaining
    }

    fn charge(&mut self, capture: &SecureCapturedString) -> Result<(), DecodeError> {
        self.remaining = self
            .remaining
            .checked_sub(capture.bytes.len())
            .ok_or(DecodeError::OutputTooLarge)?;
        Ok(())
    }
}

fn decode_session_meta(
    record: &dyn RecordSource,
    scope: Scope<'_>,
    output: &mut Output,
    retained: &mut RetainedBudget,
) -> Result<(), DecodeError> {
    let context = capture_context_path(
        record,
        &["payload", "cwd"],
        scope.context.project_root.as_deref(),
    )?;
    let event_id = output.event_id(scope)?;
    let context_ref = context
        .map(|value| {
            let capture = capture_derived(value.as_bytes())?;
            retained.charge(&capture)?;
            make_content_with_event(
                scope,
                output,
                &event_id,
                capture,
                DisclosureClass::ObservedState,
                "text/uri-list",
                scope.context.project_root.as_deref(),
                true,
            )
        })
        .transpose()?;
    let event = make_event(
        scope,
        event_id,
        Actor::System,
        None,
        None,
        SemanticKey::from_native(
            Provider::Codex,
            scope.session_id,
            "codex.session",
            "started",
        ),
        EventPayload::SessionStarted {
            context: context_ref,
        },
        PrivacyLabel::SyncEligible,
    )?;
    output.push_event(event)
}

fn decode_turn_context(
    record: &dyn RecordSource,
    scope: Scope<'_>,
    output: &mut Output,
    retained: &mut RetainedBudget,
) -> Result<(), DecodeError> {
    let context = capture_context_path(
        record,
        &["payload", "cwd"],
        scope.context.project_root.as_deref(),
    )?;
    let branch_hash = capture_optional_string(record, &["payload", "branch"], MAX_ID_BYTES, false)?
        .or(capture_optional_string(
            record,
            &["payload", "git", "branch"],
            MAX_ID_BYTES,
            false,
        )?)
        .map(|mut branch| {
            let hash = branch.hash.clone();
            branch.bytes.zeroize();
            hash
        });
    let Some(context) = context else {
        return Ok(());
    };
    let event_id = output.event_id(scope)?;
    let capture = capture_derived(context.as_bytes())?;
    retained.charge(&capture)?;
    let context_ref = make_content_with_event(
        scope,
        output,
        &event_id,
        capture,
        DisclosureClass::ObservedState,
        "text/uri-list",
        scope.context.project_root.as_deref(),
        true,
    )?;
    let event = make_event(
        scope,
        event_id,
        Actor::System,
        None,
        None,
        SemanticKey::from_native(
            Provider::Codex,
            scope.session_id,
            "codex.context",
            scope.source.record_hash(),
        ),
        EventPayload::SessionContextChanged {
            context: context_ref,
            branch_hash,
        },
        PrivacyLabel::SyncEligible,
    )?;
    output.push_event(event)
}

fn decode_compaction(
    record: &dyn RecordSource,
    scope: Scope<'_>,
    output: &mut Output,
) -> Result<(), DecodeError> {
    let summary_hash = capture_raw_optional_hash(record, &["payload", "replacement_history"])?;
    emit_event(
        scope,
        output,
        Actor::System,
        None,
        None,
        SemanticKey::from_native(
            Provider::Codex,
            scope.session_id,
            "codex.compaction",
            scope.source.record_hash(),
        ),
        EventPayload::ContextCompacted { summary_hash },
        PrivacyLabel::RestrictedLocal,
    )
}

fn decode_response_item(
    record: &dyn RecordSource,
    scope: &mut Scope<'_>,
    item_type: &str,
    state: &mut CodexStateV1,
    output: &mut Output,
    retained: &mut RetainedBudget,
) -> Result<(), DecodeError> {
    match item_type {
        "message" | "agent_message" => {
            let role = if item_type == "agent_message" {
                Some("assistant".to_owned())
            } else {
                capture_optional_plain(record, &["payload", "role"], MAX_ID_BYTES)?
            };
            let actor = match role.as_deref() {
                Some("user") => Actor::Human,
                Some("assistant" | "agent") => Actor::Agent,
                _ => return Ok(()),
            };
            if let Some(content) = capture_message_content(record, retained)? {
                emit_message(*scope, output, actor, content)?;
            }
            Ok(())
        }
        "function_call" | "custom_tool_call" | "local_shell_call" | "tool_search_call" => {
            decode_action_request(record, *scope, item_type, state, output, retained)
        }
        "web_search_call" | "image_generation_call" => {
            let call_id = capture_call_id(record, &["payload", "call_id"])?
                .or(capture_call_id(record, &["payload", "id"])?);
            let status = capture_optional_plain(record, &["payload", "status"], MAX_ID_BYTES)?;
            if call_id.is_none() || status.is_none() {
                return Ok(());
            }
            decode_action_request(record, *scope, item_type, state, output, retained)?;
            if terminal_status(status.as_deref()) {
                decode_ranked_result(
                    record,
                    *scope,
                    state,
                    output,
                    retained,
                    call_id,
                    ResultSource::ResponseOutput,
                    &["payload", "output"],
                    None,
                )?;
            }
            Ok(())
        }
        "function_call_output" | "custom_tool_call_output" | "tool_search_output" => {
            let call_id = capture_call_id(record, &["payload", "call_id"])?
                .or(capture_call_id(record, &["payload", "id"])?);
            decode_ranked_result(
                record,
                *scope,
                state,
                output,
                retained,
                call_id,
                ResultSource::ResponseOutput,
                &["payload", "output"],
                None,
            )
        }
        "compaction" => {
            reconcile_pending_results(*scope, state, output, 1)?;
            decode_compaction(record, *scope, output)
        }
        _ => Ok(()),
    }
}

fn decode_action_request(
    record: &dyn RecordSource,
    scope: Scope<'_>,
    item_type: &str,
    state: &mut CodexStateV1,
    output: &mut Output,
    retained: &mut RetainedBudget,
) -> Result<(), DecodeError> {
    let Some(call_id) = capture_call_id(record, &["payload", "call_id"])?
        .or(capture_call_id(record, &["payload", "id"])?)
    else {
        return Ok(());
    };
    let result_key =
        SemanticKey::from_native(Provider::Codex, scope.session_id, "codex.call", &call_id);
    if state.completed_rank(result_key.as_str()).is_some()
        || state.pending_result(&call_id).is_some()
    {
        return Ok(());
    }
    let tool_name = match item_type {
        "function_call" | "custom_tool_call" => {
            let Some(name) = capture_optional_digest_identifier(
                record,
                &["payload", "name"],
                "tool",
                MAX_TOOL_NAME_BYTES,
            )?
            else {
                return Ok(());
            };
            name
        }
        "local_shell_call" => "local_shell".to_owned(),
        "tool_search_call" => "tool_search".to_owned(),
        "web_search_call" => "web_search".to_owned(),
        "image_generation_call" => "image_generation".to_owned(),
        _ => return Ok(()),
    };
    let input_paths: &[&[&str]] = match item_type {
        "function_call" => &[&["payload", "arguments"]],
        "custom_tool_call" => &[&["payload", "input"]],
        "local_shell_call" => &[&["payload", "action"], &["payload", "command"]],
        "tool_search_call" => &[&["payload", "query"], &["payload", "input"]],
        "web_search_call" => &[&["payload", "query"]],
        "image_generation_call" => &[&["payload", "prompt"]],
        _ => &[],
    };
    let input = capture_first_value(record, input_paths, retained.remaining())?
        .unwrap_or(capture_derived(b"{}")?);
    retained.charge(&input)?;
    let input_hash = input.hash.clone();
    let event_id = output.event_id(scope)?;
    let content = make_content_with_event(
        scope,
        output,
        &event_id,
        input,
        DisclosureClass::AgentStatement,
        "application/json",
        scope.context.project_root.as_deref(),
        true,
    )?;
    let event = make_event(
        scope,
        event_id.clone(),
        Actor::Agent,
        Some(call_id.clone()),
        None,
        SemanticKey::from_native(
            Provider::Codex,
            scope.session_id,
            "codex.call.request",
            &call_id,
        ),
        EventPayload::ActionRequested {
            native_action_id: call_id.clone(),
            tool_name: tool_name.clone(),
            input: content,
        },
        PrivacyLabel::SyncEligible,
    )?;
    output.push_event(event)?;
    let project_relative_path = capture_context_path(
        record,
        &["payload", "path"],
        scope.context.project_root.as_deref(),
    )?;
    state.insert_call(CallLink {
        call_id,
        request_event_id: event_id.as_str().to_owned(),
        tool_name,
        input_hash,
        project_relative_path,
    })
}

fn decode_event_message(
    record: &dyn RecordSource,
    scope: &mut Scope<'_>,
    event_type: &str,
    state: &mut CodexStateV1,
    output: &mut Output,
    retained: &mut RetainedBudget,
) -> Result<(), DecodeError> {
    match event_type {
        "task_started" => {
            reconcile_pending_results(*scope, state, output, 1)?;
            emit_event(
                *scope,
                output,
                Actor::System,
                None,
                None,
                SemanticKey::from_native(
                    Provider::Codex,
                    scope.session_id,
                    "codex.turn",
                    scope.source.record_hash(),
                ),
                EventPayload::TurnStarted { prompt_id: None },
                PrivacyLabel::SyncEligible,
            )
        }
        "task_complete" => {
            reconcile_pending_results(*scope, state, output, 1)?;
            emit_turn_finished(*scope, output, ActionOutcome::Succeeded)
        }
        "turn_aborted" => {
            reconcile_pending_results(*scope, state, output, 1)?;
            emit_turn_finished(*scope, output, ActionOutcome::Cancelled)
        }
        "user_message" | "agent_message" => {
            if let Some(content) = capture_event_message_content(record, retained)? {
                emit_message(
                    *scope,
                    output,
                    if event_type == "user_message" {
                        Actor::Human
                    } else {
                        Actor::Agent
                    },
                    content,
                )?;
            }
            Ok(())
        }
        "context_compacted" => {
            reconcile_pending_results(*scope, state, output, 1)?;
            decode_compaction(record, *scope, output)
        }
        "exec_command_end" | "mcp_tool_call_end" | "patch_apply_end" => {
            let call_id = capture_call_id(record, &["payload", "call_id"])?
                .or(capture_call_id(record, &["payload", "id"])?);
            let artifact = if event_type == "patch_apply_end" {
                capture_context_path(
                    record,
                    &["payload", "path"],
                    scope.context.project_root.as_deref(),
                )?
            } else {
                None
            };
            let source = if state.history_mode() == HistoryMode::Legacy {
                ResultSource::LegacyTerminalEvent
            } else {
                ResultSource::EventFallback
            };
            decode_ranked_result(
                record,
                *scope,
                state,
                output,
                retained,
                call_id,
                source,
                &["payload", "stdout"],
                artifact.map(|path| vec![(path, "update".to_owned())]),
            )
        }
        "item_completed" => {
            let call_id = capture_call_id(record, &["payload", "item", "call_id"])?
                .or(capture_call_id(record, &["payload", "item", "id"])?);
            let item_kind =
                capture_optional_plain(record, &["payload", "item", "type"], MAX_ID_BYTES)?;
            let changes = if item_kind.as_deref() == Some("file_change") {
                capture_file_changes(record, scope.context.project_root.as_deref(), retained)?
            } else {
                Vec::new()
            };
            decode_ranked_result(
                record,
                *scope,
                state,
                output,
                retained,
                call_id,
                ResultSource::ItemCompleted,
                &["payload", "item", "output"],
                Some(changes),
            )
        }
        "sub_agent_activity" => decode_sub_agent(record, *scope, output),
        _ => Ok(()),
    }
}

#[allow(clippy::too_many_arguments)]
fn decode_ranked_result(
    record: &dyn RecordSource,
    scope: Scope<'_>,
    state: &mut CodexStateV1,
    output: &mut Output,
    retained: &mut RetainedBudget,
    call_id: Option<String>,
    source_kind: ResultSource,
    output_path: &[&str],
    artifacts: Option<Vec<(String, String)>>,
) -> Result<(), DecodeError> {
    let Some(call_id) = call_id else {
        return Ok(());
    };
    let rank = result_rank(state.history_mode(), source_kind);
    if rank == 0 {
        return Ok(());
    }
    let semantic_key =
        SemanticKey::from_native(Provider::Codex, scope.session_id, "codex.call", &call_id);
    if state.completed_rank(semantic_key.as_str()).is_some() {
        return Ok(());
    }
    let link = state.call(&call_id).cloned();
    let pending = state.pending_result(&call_id).cloned();
    if link.is_none() && pending.is_none() {
        return Ok(());
    }
    let outcome = capture_outcome(record)?;
    let result_capture = capture_first_value(
        record,
        &[
            output_path,
            &["payload", "output"],
            &["payload", "result"],
            &["payload", "stderr"],
        ],
        retained.remaining(),
    )?;
    if let Some(capture) = &result_capture {
        retained.charge(capture)?;
    }
    let request_event_id = pending
        .as_ref()
        .map(|candidate| candidate.request_event_id.clone())
        .or_else(|| {
            link.as_ref()
                .map(|candidate| candidate.request_event_id.clone())
        });
    let Some(request_event_id) = request_event_id else {
        return Ok(());
    };
    let artifact_values = result_artifacts(outcome, artifacts, link.as_ref());

    if rank < 3 {
        let _ = state.take_call(&call_id);
        return stage_ranked_result(
            state,
            call_id,
            request_event_id,
            rank,
            outcome,
            result_capture.as_ref(),
            &artifact_values,
        );
    }

    let _ = state.take_pending_result(&call_id);
    let _ = state.take_call(&call_id);
    let should_emit = state.observe_result(semantic_key.as_str().to_owned(), rank)?;
    if !should_emit {
        return Ok(());
    }
    let event_id = output.event_id(scope)?;
    let output_ref = result_capture
        .map(|capture| {
            make_content_with_event(
                scope,
                output,
                &event_id,
                capture,
                DisclosureClass::ToolResult,
                "text/plain",
                None,
                true,
            )
        })
        .transpose()?;
    let event = make_event(
        scope,
        event_id.clone(),
        Actor::Tool,
        Some(call_id.clone()),
        Some(request_event_id),
        semantic_key,
        EventPayload::ActionFinished {
            native_action_id: call_id.clone(),
            outcome,
            output: output_ref,
        },
        PrivacyLabel::SyncEligible,
    )?;
    output.push_event(event)?;
    for (path, operation) in artifact_values {
        emit_artifact(
            scope, output, retained, &call_id, &event_id, &path, &operation,
        )?;
    }
    Ok(())
}

fn result_artifacts(
    outcome: ActionOutcome,
    artifacts: Option<Vec<(String, String)>>,
    link: Option<&CallLink>,
) -> Vec<(String, String)> {
    if outcome != ActionOutcome::Succeeded {
        return Vec::new();
    }
    let mut artifacts = artifacts.unwrap_or_default();
    if artifacts.is_empty()
        && link.is_some_and(|value| is_patch_tool(&value.tool_name))
        && let Some(path) = link.and_then(|value| value.project_relative_path.clone())
    {
        artifacts.push((path, "update".to_owned()));
    }
    artifacts
}

#[allow(clippy::too_many_arguments)]
fn stage_ranked_result(
    state: &mut CodexStateV1,
    call_id: String,
    request_event_id: String,
    rank: u8,
    outcome: ActionOutcome,
    result: Option<&SecureCapturedString>,
    artifacts: &[(String, String)],
) -> Result<(), DecodeError> {
    let artifact = artifacts.first().map(|(path, operation)| {
        StagedArtifact(
            blake3::hash(path.as_bytes()).to_hex().to_string(),
            u64::try_from(path.len()).unwrap_or(u64::MAX),
            canonical_operation(operation),
        )
    });
    state.stage_result(PendingResult {
        call_id,
        request_event_id,
        rank,
        outcome: outcome.into(),
        output: result.map(|capture| StagedContent(capture.hash.clone(), capture.total_bytes)),
        artifact,
    })
}

fn reconcile_pending_results(
    scope: Scope<'_>,
    state: &mut CodexStateV1,
    output: &mut Output,
    reserved_events: usize,
) -> Result<(), DecodeError> {
    let available = MAX_EVENTS_PER_RECORD
        .saturating_sub(output.events.len())
        .saturating_sub(reserved_events);
    let to_flush = available.min(state.pending_result_count());
    for _ in 0..to_flush {
        if output.events.len() + reserved_events >= MAX_EVENTS_PER_RECORD {
            break;
        }
        let Some(pending) = state.pop_pending_result() else {
            break;
        };
        let semantic_key = SemanticKey::from_native(
            Provider::Codex,
            scope.session_id,
            "codex.call",
            &pending.call_id,
        );
        if !state.observe_result(semantic_key.as_str().to_owned(), pending.rank)? {
            continue;
        }
        let event_id = output.event_id(scope)?;
        let output_ref = staged_content(
            pending.output.as_ref(),
            DisclosureClass::ToolResult,
            "text/plain",
        )?;
        let event = make_event(
            scope,
            event_id.clone(),
            Actor::Tool,
            Some(pending.call_id.clone()),
            Some(pending.request_event_id),
            semantic_key,
            EventPayload::ActionFinished {
                native_action_id: pending.call_id.clone(),
                outcome: pending.outcome.into(),
                output: output_ref,
            },
            PrivacyLabel::SyncEligible,
        )?;
        output.push_event(event)?;
        if let Some(StagedArtifact(hash, bytes, operation)) = pending.artifact {
            if output.events.len() + reserved_events >= MAX_EVENTS_PER_RECORD {
                continue;
            }
            emit_staged_artifact(
                scope,
                output,
                &pending.call_id,
                &event_id,
                hash,
                bytes,
                operation,
            )?;
        }
    }
    Ok(())
}

fn staged_content(
    staged: Option<&StagedContent>,
    disclosure: DisclosureClass,
    media_type: &'static str,
) -> Result<Option<ContentRef>, DecodeError> {
    match staged {
        Some(StagedContent(hash, bytes)) => {
            ContentRef::bounded(hash.clone(), *bytes, media_type, None, disclosure, None)
                .map(Some)
                .map_err(|_| DecodeError::Malformed("invalid-codex-staged-content".to_owned()))
        }
        None => Ok(None),
    }
}

#[allow(clippy::too_many_arguments)]
fn emit_staged_artifact(
    scope: Scope<'_>,
    output: &mut Output,
    call_id: &str,
    parent_id: &EventId,
    path_hash: String,
    path_bytes: u64,
    operation: String,
) -> Result<(), DecodeError> {
    let event_id = output.event_id(scope)?;
    let path = ContentRef::bounded(
        path_hash,
        path_bytes,
        "text/uri-list",
        None,
        DisclosureClass::ObservedState,
        None,
    )
    .map_err(|_| DecodeError::Malformed("invalid-codex-staged-artifact".to_owned()))?;
    let semantic_id = path.hash().to_owned();
    let event = make_event(
        scope,
        event_id,
        Actor::Tool,
        Some(call_id.to_owned()),
        Some(parent_id.as_str().to_owned()),
        SemanticKey::from_native(
            Provider::Codex,
            scope.session_id,
            "codex.artifact",
            &format!("{call_id}:{semantic_id}"),
        ),
        EventPayload::ArtifactChanged {
            path,
            operation,
            content_hash: None,
        },
        PrivacyLabel::SyncEligible,
    )?;
    output.push_event(event)
}

fn emit_message(
    scope: Scope<'_>,
    output: &mut Output,
    actor: Actor,
    content: SecureCapturedString,
) -> Result<(), DecodeError> {
    let event_id = output.event_id(scope)?;
    let disclosure = if actor == Actor::Human {
        DisclosureClass::HumanIntent
    } else {
        DisclosureClass::AgentStatement
    };
    let content_ref = make_content_with_event(
        scope,
        output,
        &event_id,
        content,
        disclosure,
        "text/plain",
        scope.context.project_root.as_deref(),
        true,
    )?;
    let semantic_id = content_ref.hash().to_owned();
    let event = make_event(
        scope,
        event_id,
        actor,
        None,
        None,
        SemanticKey::from_native(
            Provider::Codex,
            scope.session_id,
            "codex.message",
            &semantic_id,
        ),
        EventPayload::MessageCreated {
            content: content_ref,
        },
        PrivacyLabel::SyncEligible,
    )?;
    output.push_event(event)
}

fn emit_turn_finished(
    scope: Scope<'_>,
    output: &mut Output,
    outcome: ActionOutcome,
) -> Result<(), DecodeError> {
    emit_event(
        scope,
        output,
        Actor::System,
        None,
        None,
        SemanticKey::from_native(
            Provider::Codex,
            scope.session_id,
            "codex.turn.finished",
            scope.source.record_hash(),
        ),
        EventPayload::TurnFinished { outcome },
        PrivacyLabel::SyncEligible,
    )
}

fn decode_sub_agent(
    record: &dyn RecordSource,
    scope: Scope<'_>,
    output: &mut Output,
) -> Result<(), DecodeError> {
    let Some(agent_id) = capture_optional_digest_identifier(
        record,
        &["payload", "agent_id"],
        "agent",
        MAX_ID_BYTES,
    )?
    else {
        return Ok(());
    };
    let status = capture_optional_plain(record, &["payload", "status"], MAX_ID_BYTES)?;
    let payload = match status.as_deref() {
        Some("started" | "running") => EventPayload::AgentStarted {
            native_agent_id: agent_id.clone(),
        },
        Some("completed" | "succeeded") => EventPayload::AgentFinished {
            native_agent_id: agent_id.clone(),
            outcome: ActionOutcome::Succeeded,
        },
        Some("failed") => EventPayload::AgentFinished {
            native_agent_id: agent_id.clone(),
            outcome: ActionOutcome::Failed,
        },
        Some("cancelled" | "aborted") => EventPayload::AgentFinished {
            native_agent_id: agent_id.clone(),
            outcome: ActionOutcome::Cancelled,
        },
        _ => return Ok(()),
    };
    emit_event(
        scope,
        output,
        Actor::System,
        Some(agent_id.clone()),
        None,
        SemanticKey::from_native(Provider::Codex, scope.session_id, "codex.agent", &agent_id),
        payload,
        PrivacyLabel::SyncEligible,
    )
}

fn emit_artifact(
    scope: Scope<'_>,
    output: &mut Output,
    retained: &mut RetainedBudget,
    call_id: &str,
    parent_id: &EventId,
    path: &str,
    operation: &str,
) -> Result<(), DecodeError> {
    let capture = capture_derived_bounded(path.as_bytes(), retained.remaining())?;
    retained.charge(&capture)?;
    let event_id = output.event_id(scope)?;
    let path_ref = make_content_with_event(
        scope,
        output,
        &event_id,
        capture,
        DisclosureClass::ObservedState,
        "text/uri-list",
        None,
        true,
    )?;
    let event = make_event(
        scope,
        event_id,
        Actor::Tool,
        Some(call_id.to_owned()),
        Some(parent_id.as_str().to_owned()),
        SemanticKey::from_native(
            Provider::Codex,
            scope.session_id,
            "codex.artifact",
            &format!("{call_id}:{}", path_ref.hash()),
        ),
        EventPayload::ArtifactChanged {
            path: path_ref,
            operation: canonical_operation(operation),
            content_hash: None,
        },
        PrivacyLabel::SyncEligible,
    )?;
    output.push_event(event)
}

#[derive(Clone, Copy)]
enum ResultSource {
    ItemCompleted,
    LegacyTerminalEvent,
    ResponseOutput,
    EventFallback,
}

const fn result_rank(mode: HistoryMode, source: ResultSource) -> u8 {
    match (mode, source) {
        (HistoryMode::Paginated, ResultSource::ItemCompleted)
        | (HistoryMode::Legacy, ResultSource::LegacyTerminalEvent) => 3,
        (_, ResultSource::ResponseOutput) => 2,
        (_, ResultSource::EventFallback) => 1,
        _ => 0,
    }
}

#[allow(clippy::too_many_arguments)]
fn emit_event(
    scope: Scope<'_>,
    output: &mut Output,
    actor: Actor,
    correlation_id: Option<String>,
    causation_id: Option<String>,
    semantic_key: SemanticKey,
    payload: EventPayload,
    privacy: PrivacyLabel,
) -> Result<(), DecodeError> {
    let event_id = output.event_id(scope)?;
    let event = make_event(
        scope,
        event_id,
        actor,
        correlation_id,
        causation_id,
        semantic_key,
        payload,
        privacy,
    )?;
    output.push_event(event)
}

#[allow(clippy::too_many_arguments)]
fn make_event(
    scope: Scope<'_>,
    event_id: EventId,
    actor: Actor,
    correlation_id: Option<String>,
    causation_id: Option<String>,
    semantic_key: SemanticKey,
    payload: EventPayload,
    privacy: PrivacyLabel,
) -> Result<ActivityEventV1, DecodeError> {
    let session_id = SessionId::parse_wire(scope.session_id)
        .ok_or_else(|| DecodeError::Malformed("invalid-codex-session".to_owned()))?;
    ActivityEventV1::new(ActivityEventDraft {
        event_id,
        semantic_key,
        schema_version: 1,
        occurred_at: scope.occurred_at,
        observed_at: scope.context.observed_at,
        project_id: scope.context.project_id.clone(),
        session_id,
        turn_id: None,
        actor,
        correlation_id,
        causation_id,
        source: scope.source.clone(),
        payload,
        privacy,
    })
    .map_err(|_| DecodeError::Malformed("invalid-codex-event".to_owned()))
}

#[allow(clippy::too_many_arguments)]
fn make_content_with_event(
    scope: Scope<'_>,
    output: &mut Output,
    event_id: &EventId,
    mut capture: SecureCapturedString,
    disclosure: DisclosureClass,
    media_type: &'static str,
    project_root: Option<&Path>,
    allow_evidence: bool,
) -> Result<ContentRef, DecodeError> {
    let plaintext = std::mem::take(&mut capture.bytes);
    let text = std::str::from_utf8(&plaintext)
        .map_err(|_| DecodeError::Malformed("invalid-codex-content-utf8".to_owned()))?;
    let redacted = if looks_like_base64(text) {
        None
    } else {
        Some(
            RedactionPolicy::new()
                .and_then(|policy| policy.redact(text, project_root, disclosure))
                .map_err(|_| DecodeError::Malformed("codex-redaction-failed".to_owned()))?,
        )
    };
    let evidence_ordinal = output.evidence_ordinal()?;
    let evidence_id = EvidenceId::from_source(&source_identity(scope), evidence_ordinal);
    let keep_evidence = allow_evidence && !capture.truncated;
    let locator = keep_evidence.then(|| LocalLocator::Evidence {
        evidence_id: evidence_id.clone(),
    });
    let content = ContentRef::bounded(
        capture.take_hash(),
        capture.total_bytes,
        media_type,
        locator,
        disclosure,
        redacted,
    )
    .map_err(|_| DecodeError::Malformed("invalid-codex-content".to_owned()))?;
    if keep_evidence {
        output.push_evidence(DecodedEvidence {
            evidence_id,
            owner_event_id: event_id.clone(),
            content: content.clone(),
            plaintext,
        })?;
    }
    Ok(content)
}

fn progress_envelope(
    top_type: &str,
    ordinal: Option<u64>,
    state: &mut CodexStateV1,
) -> Result<(), DecodeError> {
    let observed = if ordinal.is_some() {
        HistoryMode::Paginated
    } else if top_type == "session_meta" {
        HistoryMode::Legacy
    } else {
        HistoryMode::Unknown
    };
    state.observe_history_mode(observed);
    state.observe_ordinal(ordinal)
}

fn observe_known_history_mode(
    record: &dyn RecordSource,
    state: &mut CodexStateV1,
) -> Result<(), DecodeError> {
    let explicit = capture_optional_plain(record, &["payload", "history_mode"], MAX_ID_BYTES)?;
    let observed = match explicit.as_deref() {
        Some("paginated") => HistoryMode::Paginated,
        Some("legacy") => HistoryMode::Legacy,
        Some(_) | None => HistoryMode::Unknown,
    };
    state.observe_history_mode(observed);
    Ok(())
}

fn capture_timestamp(record: &dyn RecordSource) -> Result<Option<OffsetDateTime>, DecodeError> {
    let Some(value) = capture_optional_plain(record, &["timestamp"], MAX_ID_BYTES)? else {
        return Ok(None);
    };
    OffsetDateTime::parse(&value, &Rfc3339)
        .map(Some)
        .map_err(|_| DecodeError::Malformed("invalid-codex-timestamp".to_owned()))
}

fn capture_message_content(
    record: &dyn RecordSource,
    retained: &mut RetainedBudget,
) -> Result<Option<SecureCapturedString>, DecodeError> {
    if let Some(value) =
        capture_optional_string(record, &["payload", "content"], retained.remaining(), true)?
    {
        retained.charge(&value)?;
        return Ok(Some(value));
    }
    let value = capture_joined_text(
        record,
        &["payload", "content", "text"],
        retained.remaining(),
    )?;
    if let Some(value) = &value {
        retained.charge(value)?;
    }
    Ok(value)
}

fn capture_event_message_content(
    record: &dyn RecordSource,
    retained: &mut RetainedBudget,
) -> Result<Option<SecureCapturedString>, DecodeError> {
    for path in [
        &["payload", "message"][..],
        &["payload", "text"][..],
        &["payload", "content"][..],
    ] {
        if let Some(value) = capture_optional_string(record, path, retained.remaining(), true)? {
            retained.charge(&value)?;
            return Ok(Some(value));
        }
    }
    Ok(None)
}

fn capture_joined_text(
    record: &dyn RecordSource,
    path: &[&str],
    retained_limit: usize,
) -> Result<Option<SecureCapturedString>, DecodeError> {
    let matches = capture_matches(record, path, 0, 0)?;
    let mut selected = Vec::new();
    let mut seen = HashSet::new();
    for captured in matches {
        if !seen.insert(captured.array_indices.clone()) {
            return Err(DecodeError::Malformed(
                "duplicate-codex-content-field".to_owned(),
            ));
        }
        if !matches!(captured.value, CapturedValue::String(_)) {
            return Err(DecodeError::Malformed(
                "non-string-codex-content".to_owned(),
            ));
        }
        selected.push(captured.array_indices);
    }
    if selected.is_empty() {
        return Ok(None);
    }
    let mut reader = BoundedJsonReader::new(record.open()?);
    reader.capture_joined_matches(path, &selected, retained_limit, MAX_MATCHES)
}

fn capture_first_value(
    record: &dyn RecordSource,
    paths: &[&[&str]],
    retained_limit: usize,
) -> Result<Option<SecureCapturedString>, DecodeError> {
    for path in paths {
        if let Some(value) = capture_optional_string(record, path, retained_limit, true)? {
            return Ok(Some(value));
        }
        if let Some(value) = capture_raw_value(record, path, retained_limit)? {
            return Ok(Some(value));
        }
    }
    Ok(None)
}

fn capture_optional_string(
    record: &dyn RecordSource,
    path: &[&str],
    limit: usize,
    allow_container: bool,
) -> Result<Option<SecureCapturedString>, DecodeError> {
    let matches = capture_matches(record, path, limit, limit.saturating_mul(2))?;
    if matches.len() > 1 {
        return Err(DecodeError::Malformed(
            "duplicate-codex-selected-field".to_owned(),
        ));
    }
    match matches.into_iter().next().map(|value| value.value) {
        None => Ok(None),
        Some(CapturedValue::Scalar(value)) if value == "null" => Ok(None),
        Some(CapturedValue::String(value)) => Ok(Some(value)),
        Some(CapturedValue::Container) if allow_container => Ok(None),
        Some(CapturedValue::Container | CapturedValue::Scalar(_)) => Err(DecodeError::Malformed(
            "invalid-codex-selected-field".to_owned(),
        )),
    }
}

fn capture_raw_value(
    record: &dyn RecordSource,
    path: &[&str],
    retained_limit: usize,
) -> Result<Option<SecureCapturedString>, DecodeError> {
    let mut reader = BoundedJsonReader::new(record.open()?);
    let matches =
        reader.capture_raw_matches(path, retained_limit, 2, retained_limit.saturating_mul(2))?;
    if matches.len() > 1 {
        return Err(DecodeError::Malformed(
            "duplicate-codex-selected-field".to_owned(),
        ));
    }
    match matches.into_iter().next().map(|value| value.value) {
        None => Ok(None),
        Some(CapturedValue::Scalar(value)) if value == "null" => Ok(None),
        Some(CapturedValue::String(value)) => Ok(Some(value)),
        Some(CapturedValue::Container | CapturedValue::Scalar(_)) => {
            Err(DecodeError::Malformed("invalid-codex-raw-field".to_owned()))
        }
    }
}

fn capture_raw_optional_hash(
    record: &dyn RecordSource,
    path: &[&str],
) -> Result<Option<String>, DecodeError> {
    Ok(capture_raw_value(record, path, 0)?.map(|value| value.hash))
}

fn capture_optional_plain(
    record: &dyn RecordSource,
    path: &[&str],
    limit: usize,
) -> Result<Option<String>, DecodeError> {
    let Some(mut value) = capture_optional_string(record, path, limit, false)? else {
        return Ok(None);
    };
    if value.truncated || value.total_bytes > limit as u64 {
        return Err(DecodeError::Malformed(
            "oversized-codex-identifier".to_owned(),
        ));
    }
    String::from_utf8(value.take_bytes())
        .map(Some)
        .map_err(|_| DecodeError::Malformed("invalid-codex-identifier".to_owned()))
}

fn capture_optional_digest_identifier(
    record: &dyn RecordSource,
    path: &[&str],
    domain: &str,
    limit: usize,
) -> Result<Option<String>, DecodeError> {
    let Some(mut value) = capture_optional_string(record, path, limit, false)? else {
        return Ok(None);
    };
    if value.total_bytes == 0 {
        return Ok(None);
    }
    let raw = (!value.truncated && value.total_bytes <= limit as u64)
        .then(|| String::from_utf8(value.take_bytes()).ok())
        .flatten();
    Ok(Some(
        raw.filter(|text| safe_native_id(text, domain))
            .unwrap_or_else(|| {
                if domain == "call" {
                    opaque_call_id(value.total_bytes, &value.hash)
                } else {
                    opaque_id(domain, value.total_bytes, &value.hash)
                }
            }),
    ))
}

fn capture_call_id(
    record: &dyn RecordSource,
    path: &[&str],
) -> Result<Option<String>, DecodeError> {
    capture_optional_digest_identifier(record, path, "call", MAX_ID_BYTES)
}

fn capture_required_nested_type(
    record: &dyn RecordSource,
    path: &[&str],
) -> Result<String, DecodeError> {
    capture_optional_plain(record, path, MAX_ID_BYTES)?
        .ok_or(DecodeError::MissingIdentity("payload.type"))
        .and_then(|value| {
            bounded_identifier(&value, MAX_ID_BYTES)
                .then_some(value)
                .ok_or_else(|| DecodeError::Malformed("invalid-codex-nested-type".to_owned()))
        })
}

fn capture_optional_u64(
    record: &dyn RecordSource,
    path: &[&str],
) -> Result<Option<u64>, DecodeError> {
    let matches = capture_matches(record, path, 32, 64)?;
    if matches.len() > 1 {
        return Err(DecodeError::Malformed("duplicate-codex-ordinal".to_owned()));
    }
    match matches.into_iter().next().map(|value| value.value) {
        None => Ok(None),
        Some(CapturedValue::Scalar(value)) if value == "null" => Ok(None),
        Some(CapturedValue::Scalar(value)) => value
            .parse::<u64>()
            .map(Some)
            .map_err(|_| DecodeError::Malformed("invalid-codex-ordinal".to_owned())),
        Some(CapturedValue::String(_) | CapturedValue::Container) => {
            Err(DecodeError::Malformed("invalid-codex-ordinal".to_owned()))
        }
    }
}

fn capture_outcome(record: &dyn RecordSource) -> Result<ActionOutcome, DecodeError> {
    for path in [
        &["payload", "status"][..],
        &["payload", "item", "status"][..],
    ] {
        if let Some(status) = capture_optional_plain(record, path, MAX_ID_BYTES)? {
            return Ok(match status.as_str() {
                "completed" | "succeeded" | "success" => ActionOutcome::Succeeded,
                "failed" | "error" => ActionOutcome::Failed,
                "cancelled" | "aborted" => ActionOutcome::Cancelled,
                _ => ActionOutcome::Unknown,
            });
        }
    }
    let exit_code = capture_optional_i64(record, &["payload", "exit_code"])?;
    Ok(match exit_code {
        Some(0) => ActionOutcome::Succeeded,
        Some(_) => ActionOutcome::Failed,
        None => ActionOutcome::Unknown,
    })
}

fn capture_optional_i64(
    record: &dyn RecordSource,
    path: &[&str],
) -> Result<Option<i64>, DecodeError> {
    let matches = capture_matches(record, path, 32, 64)?;
    if matches.len() > 1 {
        return Err(DecodeError::Malformed(
            "duplicate-codex-numeric-field".to_owned(),
        ));
    }
    match matches.into_iter().next().map(|value| value.value) {
        None => Ok(None),
        Some(CapturedValue::Scalar(value)) if value == "null" => Ok(None),
        Some(CapturedValue::Scalar(value)) => value
            .parse::<i64>()
            .map(Some)
            .map_err(|_| DecodeError::Malformed("invalid-codex-numeric-field".to_owned())),
        Some(CapturedValue::String(_) | CapturedValue::Container) => Err(DecodeError::Malformed(
            "invalid-codex-numeric-field".to_owned(),
        )),
    }
}

fn capture_file_changes(
    record: &dyn RecordSource,
    root: Option<&Path>,
    retained: &mut RetainedBudget,
) -> Result<Vec<(String, String)>, DecodeError> {
    let paths = capture_matches(
        record,
        &["payload", "item", "changes", "path"],
        MAX_PATH_BYTES,
        retained.remaining(),
    )?;
    for captured in &paths {
        if let CapturedValue::String(value) = &captured.value {
            retained.charge(value)?;
        }
    }
    let kinds = capture_matches(
        record,
        &["payload", "item", "changes", "kind"],
        MAX_ID_BYTES,
        retained.remaining(),
    )?;
    for captured in &kinds {
        if let CapturedValue::String(value) = &captured.value {
            retained.charge(value)?;
        }
    }
    let mut kind_by_index = std::collections::BTreeMap::new();
    let mut seen_kinds = HashSet::new();
    for captured in kinds {
        let [index] = captured.array_indices.as_slice() else {
            return Err(DecodeError::Malformed(
                "invalid-codex-change-location".to_owned(),
            ));
        };
        if !seen_kinds.insert(*index) {
            return Err(DecodeError::Malformed(
                "duplicate-codex-change-kind".to_owned(),
            ));
        }
        let CapturedValue::String(value) = captured.value else {
            return Err(DecodeError::Malformed(
                "invalid-codex-change-kind".to_owned(),
            ));
        };
        kind_by_index.insert(*index, canonical_change_kind(value)?);
    }
    let mut output = Vec::new();
    let mut seen = HashSet::new();
    for captured in paths {
        let [index] = captured.array_indices.as_slice() else {
            return Err(DecodeError::Malformed(
                "invalid-codex-change-location".to_owned(),
            ));
        };
        if !seen.insert(*index) {
            return Err(DecodeError::Malformed(
                "duplicate-codex-change-path".to_owned(),
            ));
        }
        let CapturedValue::String(mut value) = captured.value else {
            return Err(DecodeError::Malformed(
                "invalid-codex-change-path".to_owned(),
            ));
        };
        if value.truncated || value.total_bytes > MAX_PATH_BYTES as u64 {
            continue;
        }
        let raw = Zeroizing::new(value.take_bytes());
        let Ok(raw) = std::str::from_utf8(&raw) else {
            continue;
        };
        if let Some(path) = normalize_project_path(raw, root)
            && output.len() < MAX_EVENTS_PER_RECORD.saturating_sub(1)
        {
            output.push((
                path,
                kind_by_index
                    .remove(index)
                    .unwrap_or_else(|| "update".to_owned()),
            ));
        }
    }
    Ok(output)
}

fn canonical_change_kind(mut value: SecureCapturedString) -> Result<String, DecodeError> {
    if value.truncated || value.total_bytes == 0 || value.total_bytes > MAX_ID_BYTES as u64 {
        return Err(DecodeError::Malformed(
            "invalid-codex-change-kind".to_owned(),
        ));
    }
    let raw = std::str::from_utf8(&value.bytes)
        .map_err(|_| DecodeError::Malformed("invalid-codex-change-kind".to_owned()))?;
    if !bounded_identifier(raw, MAX_ID_BYTES) {
        return Err(DecodeError::Malformed(
            "invalid-codex-change-kind".to_owned(),
        ));
    }
    let canonical = canonical_operation(raw);
    value.bytes.zeroize();
    Ok(canonical)
}

fn capture_context_path(
    record: &dyn RecordSource,
    path: &[&str],
    root: Option<&Path>,
) -> Result<Option<String>, DecodeError> {
    let Some(value) = capture_optional_plain(record, path, MAX_PATH_BYTES)? else {
        return Ok(None);
    };
    Ok(normalize_project_path(&value, root))
}

fn capture_matches(
    record: &dyn RecordSource,
    path: &[&str],
    limit: usize,
    aggregate: usize,
) -> Result<Vec<CapturedMatch>, DecodeError> {
    let mut reader = BoundedJsonReader::new(record.open()?);
    reader.capture_matches(path, limit, MAX_MATCHES, aggregate)
}

fn capture_single_string(
    record: &dyn RecordSource,
    path: &[&str],
    limit: usize,
    allow_container: bool,
) -> Result<(Option<SecureCapturedString>, String), DecodeError> {
    let mut reader = BoundedJsonReader::new(record.open()?);
    let matches = reader.capture_matches(path, limit, 2, limit.saturating_mul(2))?;
    let schema = reader
        .schema_fingerprint()
        .ok_or_else(|| DecodeError::Malformed("missing-codex-schema".to_owned()))?
        .to_owned();
    if matches.len() > 1 {
        return Err(DecodeError::Malformed(
            "duplicate-codex-selected-field".to_owned(),
        ));
    }
    let value = match matches.into_iter().next().map(|value| value.value) {
        None => None,
        Some(CapturedValue::String(value)) => Some(value),
        Some(CapturedValue::Container) if allow_container => None,
        Some(CapturedValue::Container | CapturedValue::Scalar(_)) => {
            return Err(DecodeError::Malformed(
                "invalid-codex-selected-field".to_owned(),
            ));
        }
    };
    Ok((value, schema))
}

fn strict_identifier(
    mut value: SecureCapturedString,
    limit: usize,
    class: &str,
) -> Result<String, DecodeError> {
    if value.truncated || value.total_bytes == 0 || value.total_bytes > limit as u64 {
        return Err(DecodeError::Malformed(format!("invalid-codex-{class}")));
    }
    let value = String::from_utf8(value.take_bytes())
        .map_err(|_| DecodeError::Malformed(format!("invalid-codex-{class}")))?;
    if bounded_identifier(&value, limit) {
        Ok(value)
    } else {
        Err(DecodeError::Malformed(format!("invalid-codex-{class}")))
    }
}

fn normalize_project_path(value: &str, root: Option<&Path>) -> Option<String> {
    if value.is_empty() || value.len() > MAX_PATH_BYTES || value.chars().any(char::is_control) {
        return None;
    }
    let root = root?;
    if !valid_root(root) {
        return None;
    }
    let path = Path::new(value);
    let relative = if path.is_absolute() {
        path.strip_prefix(root).ok()?
    } else {
        path
    };
    if relative.as_os_str().is_empty() {
        return Some("$PROJECT".to_owned());
    }
    if !relative
        .components()
        .all(|component| matches!(component, Component::Normal(_)))
    {
        return None;
    }
    let mut candidate = root.to_path_buf();
    for component in relative.components() {
        let Component::Normal(component) = component else {
            return None;
        };
        candidate.push(component);
        match std::fs::symlink_metadata(&candidate) {
            Ok(metadata) if metadata.file_type().is_symlink() => return None,
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => break,
            Err(_) => return None,
        }
    }
    let relative = relative.to_string_lossy();
    let normalized = format!("$PROJECT/{relative}");
    (normalized.len() <= MAX_PATH_BYTES).then_some(normalized)
}

fn valid_root(root: &Path) -> bool {
    root.is_absolute()
        && root
            .components()
            .all(|component| matches!(component, Component::RootDir | Component::Normal(_)))
}

fn active_hierarchy_date(relative: &Path) -> Option<OffsetDateTime> {
    let components = relative
        .components()
        .map(|component| match component {
            Component::Normal(value) => value.to_str(),
            _ => None,
        })
        .collect::<Option<Vec<_>>>()?;
    if components.len() != 4 {
        return None;
    }
    parse_utc_date(components[0], components[1], components[2])
}

fn archive_rollout_date(relative: &Path) -> Option<OffsetDateTime> {
    let mut components = relative.components();
    let Component::Normal(filename) = components.next()? else {
        return None;
    };
    if components.next().is_some() {
        return None;
    }
    let filename = filename.to_str()?;
    let date = filename.strip_prefix("rollout-")?.get(..10)?;
    let mut pieces = date.split('-');
    parse_utc_date(pieces.next()?, pieces.next()?, pieces.next()?)
}

fn parse_utc_date(year: &str, month: &str, day: &str) -> Option<OffsetDateTime> {
    let year = year.parse::<i32>().ok()?;
    let month = Month::try_from(month.parse::<u8>().ok()?).ok()?;
    let day = day.parse::<u8>().ok()?;
    let date = Date::from_calendar_date(year, month, day).ok()?;
    Some(PrimitiveDateTime::new(date, Time::MIDNIGHT).assume_utc())
}

fn validate_context(context: &DecodeContext) -> Result<(), DecodeError> {
    if context.source_id.is_empty() || context.source_id.len() > MAX_ID_BYTES {
        return Err(DecodeError::Malformed("invalid-codex-source-id".to_owned()));
    }
    if context
        .project_root
        .as_ref()
        .is_some_and(|root| !valid_root(root))
    {
        return Err(DecodeError::Malformed(
            "invalid-codex-project-root".to_owned(),
        ));
    }
    Ok(())
}

fn source_id(context: &DecodeContext) -> String {
    if bounded_identifier(&context.source_id, MAX_ID_BYTES)
        && ["source_", "src_", "fixture_"]
            .iter()
            .any(|prefix| context.source_id.starts_with(prefix))
    {
        context.source_id.clone()
    } else {
        opaque_id(
            "source",
            u64::try_from(context.source_id.len()).unwrap_or(u64::MAX),
            blake3::hash(context.source_id.as_bytes()).to_hex().as_str(),
        )
    }
}

fn session_id(context: &DecodeContext) -> String {
    let source = source_id(context);
    opaque_id(
        "session",
        u64::try_from(source.len()).unwrap_or(u64::MAX),
        blake3::hash(source.as_bytes()).to_hex().as_str(),
    )
}

fn safe_native_id(value: &str, domain: &str) -> bool {
    let valid_chars = value
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'));
    valid_chars
        && match domain {
            "call" => {
                value.len() <= 48 && (value.starts_with("call-") || value.starts_with("call_"))
            }
            "tool" => value.len() <= MAX_TOOL_NAME_BYTES,
            "agent" => value.starts_with("agent-") || value.starts_with("agent_"),
            _ => false,
        }
}

fn opaque_id(domain: &str, total_bytes: u64, hash: &str) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"agbox-codex-opaque-id-v1");
    hasher.update(&(domain.len() as u64).to_le_bytes());
    hasher.update(domain.as_bytes());
    hasher.update(&total_bytes.to_le_bytes());
    hasher.update(hash.as_bytes());
    format!("codex_{domain}_{}", &hasher.finalize().to_hex()[..48])
}

fn opaque_call_id(total_bytes: u64, hash: &str) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"agbox-codex-call-identity-v1");
    hasher.update(&total_bytes.to_le_bytes());
    hasher.update(hash.as_bytes());
    format!("codex_call_{}", &hasher.finalize().to_hex()[..32])
}

fn source_identity(scope: Scope<'_>) -> SourceIdentity {
    SourceIdentity {
        provider: Provider::Codex,
        source_id: source_id(scope.context),
        generation: scope.source.source_generation(),
        byte_offset: scope.source.byte_offset(),
        record_hash: scope.source.record_hash().to_owned(),
    }
}

fn make_source(
    record: &dyn RecordSource,
    context: &DecodeContext,
    session_id: &str,
    native_type: &str,
    ordinal: Option<u64>,
) -> Result<SourceRef, DecodeError> {
    SourceRef::new(SourceRefDraft {
        provider: Provider::Codex,
        format: DECODER_VERSION.to_owned(),
        native_session_id: session_id.to_owned(),
        native_record_type: native_type.to_owned(),
        native_record_id: ordinal.map(|value| format!("codex_record_{value}")),
        source_generation: context.source_generation,
        byte_offset: record.start(),
        ordinal,
        record_hash: record.record_hash().to_owned(),
        decoder_version: DECODER_VERSION.to_owned(),
    })
    .map_err(|_| DecodeError::Malformed("invalid-codex-source".to_owned()))
}

fn make_observation(
    record: &dyn RecordSource,
    context: &DecodeContext,
    source: SourceRef,
    schema_fingerprint: &str,
    status: DecodeStatus,
) -> Result<SourceObservation, DecodeError> {
    let range = ByteRange::new(record.start(), record.end())
        .map_err(|_| DecodeError::Malformed("invalid-codex-range".to_owned()))?;
    let bounded_record = ContentRef::bounded(
        record.record_hash().to_owned(),
        record
            .end()
            .checked_sub(record.start())
            .ok_or_else(|| DecodeError::Malformed("invalid-codex-range".to_owned()))?,
        "application/x-ndjson",
        Some(LocalLocator::SourceRange {
            source_id: source_id(context),
            generation: context.source_generation,
            byte_start: record.start(),
            byte_end: record.end(),
        }),
        DisclosureClass::ObservedState,
        None,
    )
    .map_err(|_| DecodeError::Malformed("invalid-codex-observation-content".to_owned()))?;
    SourceObservation::new(SourceObservationDraft {
        observation_id: format!(
            "obs_{}",
            &blake3::hash(
                format!(
                    "{}:{}:{}:{}",
                    source_id(context),
                    context.source_generation,
                    record.start(),
                    record.record_hash()
                )
                .as_bytes()
            )
            .to_hex()[..24]
        ),
        source,
        range,
        observed_at: context.observed_at,
        status,
        bounded_record: Some(bounded_record),
        schema_fingerprint: schema_fingerprint.to_owned(),
    })
    .map_err(|_| DecodeError::Malformed("invalid-codex-observation".to_owned()))
}

fn malformed_envelope(
    record: &dyn RecordSource,
    context: &DecodeContext,
    prior_state: &DecoderState,
    top_type: &str,
    schema: &str,
    error: DecodeError,
) -> Result<DecodedRecord, DecodeError> {
    if let DecodeError::Io(error) = error {
        return Err(DecodeError::Io(error));
    }
    let source = make_source(record, context, &session_id(context), top_type, None)?;
    let observation = make_observation(record, context, source, schema, DecodeStatus::Malformed)?;
    Ok(classified_empty(
        observation,
        DecodeDisposition::malformed(error_class(&error)),
        prior_state,
    ))
}

fn classified_empty(
    observation: SourceObservation,
    disposition: DecodeDisposition,
    prior_state: &DecoderState,
) -> DecodedRecord {
    empty_with_next_state(observation, disposition, prior_state.clone(), prior_state)
}

fn empty_with_next_state(
    observation: SourceObservation,
    disposition: DecodeDisposition,
    next_state: DecoderState,
    prior_state: &DecoderState,
) -> DecodedRecord {
    record_with(
        observation,
        disposition,
        Vec::new(),
        Vec::new(),
        next_state,
        prior_state,
    )
}

fn record_with(
    observation: SourceObservation,
    disposition: DecodeDisposition,
    events: Vec<ActivityEventV1>,
    evidence: Vec<DecodedEvidence>,
    next_state: DecoderState,
    prior_state: &DecoderState,
) -> DecodedRecord {
    DecodedRecord::new(
        DecodedRecordDraft {
            observation,
            events,
            evidence,
            disposition,
            next_state,
            semantic_bytes: 0,
        },
        prior_state,
    )
}

fn capture_derived(value: &[u8]) -> Result<SecureCapturedString, DecodeError> {
    Ok(SecureCapturedString {
        bytes: Zeroizing::new(value.to_vec()),
        total_bytes: u64::try_from(value.len()).map_err(|_| DecodeError::OutputTooLarge)?,
        hash: blake3::hash(value).to_hex().to_string(),
        truncated: false,
    })
}

fn capture_derived_bounded(
    value: &[u8],
    retained_limit: usize,
) -> Result<SecureCapturedString, DecodeError> {
    let retained = value.len().min(retained_limit);
    let mut end = retained;
    while std::str::from_utf8(&value[..end]).is_err() {
        end = end
            .checked_sub(1)
            .ok_or_else(|| DecodeError::Malformed("invalid-codex-derived-content".to_owned()))?;
    }
    Ok(SecureCapturedString {
        bytes: Zeroizing::new(value[..end].to_vec()),
        total_bytes: u64::try_from(value.len()).map_err(|_| DecodeError::OutputTooLarge)?,
        hash: blake3::hash(value).to_hex().to_string(),
        truncated: end < value.len(),
    })
}

fn error_class(error: &DecodeError) -> &str {
    match error {
        DecodeError::Malformed(_) => "malformed-codex-record",
        DecodeError::MissingIdentity(_) => "missing-codex-identity",
        DecodeError::OutputTooLarge => "oversized-codex-record",
        DecodeError::StateTooLarge => "oversized-codex-state",
        DecodeError::Io(_) => "codex-io",
    }
}

fn known_top_type(value: &str) -> bool {
    matches!(
        value,
        "session_meta"
            | "response_item"
            | "event_msg"
            | "turn_context"
            | "compacted"
            | "world_state"
    )
}

fn known_nested_type(top_type: &str, value: &str) -> bool {
    match top_type {
        "response_item" => matches!(
            value,
            "message"
                | "agent_message"
                | "function_call"
                | "custom_tool_call"
                | "local_shell_call"
                | "tool_search_call"
                | "tool_search_output"
                | "web_search_call"
                | "image_generation_call"
                | "function_call_output"
                | "custom_tool_call_output"
                | "reasoning"
                | "compaction"
                | "world_state"
        ),
        "event_msg" => matches!(
            value,
            "task_started"
                | "task_complete"
                | "turn_aborted"
                | "user_message"
                | "agent_message"
                | "item_completed"
                | "mcp_tool_call_end"
                | "patch_apply_end"
                | "exec_command_end"
                | "sub_agent_activity"
                | "context_compacted"
        ),
        _ => false,
    }
}

fn terminal_status(status: Option<&str>) -> bool {
    matches!(
        status,
        Some("completed" | "succeeded" | "failed" | "cancelled" | "aborted")
    )
}

fn canonical_operation(value: &str) -> String {
    match value {
        "create" | "add" => "create",
        "delete" | "remove" => "delete",
        _ => "update",
    }
    .to_owned()
}

fn is_patch_tool(value: &str) -> bool {
    matches!(
        value.to_ascii_lowercase().as_str(),
        "apply_patch" | "patch" | "write" | "edit"
    )
}

fn looks_like_base64(value: &str) -> bool {
    value
        .split(|character: char| {
            !(character.is_ascii_alphanumeric() || matches!(character, '+' | '/' | '='))
        })
        .any(|token| {
            token.len() >= 128
                && token.len() % 4 == 0
                && token
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'+' | b'/' | b'='))
        })
}
