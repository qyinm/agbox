use std::{
    collections::{BTreeMap, HashSet},
    path::{Component, Path},
};

use agbox_core::{
    ActionOutcome, ActivityEventDraft, ActivityEventV1, Actor, ByteRange, ContentRef, DecodeStatus,
    DisclosureClass, EventId, EventPayload, EvidenceId, LocalLocator, PrivacyLabel, Provider,
    RedactionPolicy, SemanticKey, SessionId, SourceIdentity, SourceObservation,
    SourceObservationDraft, SourceRef, SourceRefDraft,
};
use time::{OffsetDateTime, format_description::well_known::Rfc3339};
use zeroize::Zeroizing;

use crate::{
    BoundedJsonReader, DecodeContext, DecodeDisposition, DecodeError, DecodedEvidence,
    DecodedRecord, DecodedRecordDraft, DecoderState, MAX_CAPTURE_BYTES, MAX_EVENTS_PER_RECORD,
    MAX_EVIDENCE_PER_RECORD, MAX_RECORD_SEMANTIC_BYTES, RecordSource, RootClass, RootSpec,
    SourceAdapter,
    json::{CapturedMatch, CapturedValue, SecureCapturedString},
};

use super::state::{
    ClaudeStateV1, ToolLink, bounded_identifier, canonical_context_mode, canonical_permission_mode,
};

const DECODER_VERSION: &str = "claude-transcript-2.1";
const MAX_MATCHES: usize = MAX_EVENTS_PER_RECORD;
const MAX_ID_BYTES: usize = 128;
const MAX_TOOL_NAME_BYTES: usize = 64;
const MAX_PATH_BYTES: usize = 512;
const SEMANTIC_HEADROOM: usize = 64 * 1024;

pub struct ClaudeAdapter;

impl std::fmt::Debug for ClaudeAdapter {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("ClaudeAdapter")
    }
}

impl SourceAdapter for ClaudeAdapter {
    fn provider(&self) -> Provider {
        Provider::Claude
    }

    fn decoder_version(&self) -> &'static str {
        DECODER_VERSION
    }

    fn roots(&self, home: &Path) -> Vec<RootSpec> {
        vec![RootSpec {
            path: home.join(".claude").join("projects"),
            class: RootClass::Active,
            recursive: true,
        }]
    }

    fn matches(&self, root: &RootSpec, relative: &Path) -> bool {
        root.class == RootClass::Active
            && root.recursive
            && !relative.as_os_str().is_empty()
            && !relative.is_absolute()
            && relative
                .components()
                .all(|component| matches!(component, Component::Normal(_)))
            && relative
                .extension()
                .is_some_and(|extension| extension == "jsonl")
    }

    fn trusted_session_time(
        &self,
        _root: &RootSpec,
        _relative: &Path,
        _mtime: OffsetDateTime,
    ) -> Option<OffsetDateTime> {
        None
    }

    fn decode(
        &self,
        record: &dyn RecordSource,
        context: &DecodeContext,
        prior_state: &DecoderState,
    ) -> Result<DecodedRecord, DecodeError> {
        if !bounded_identifier(&context.source_id, MAX_ID_BYTES) {
            return Err(DecodeError::Malformed("invalid-source-id".to_owned()));
        }
        if context.project_root.as_ref().is_some_and(|root| {
            !root.is_absolute()
                || !root
                    .components()
                    .all(|component| matches!(component, Component::RootDir | Component::Normal(_)))
        }) {
            return Err(DecodeError::Malformed(
                "invalid-trusted-project-root".to_owned(),
            ));
        }
        let (native_type_capture, schema_fingerprint) =
            capture_single_string(record, &["type"], MAX_ID_BYTES, true)?;
        let native_type_capture =
            native_type_capture.ok_or(DecodeError::MissingIdentity("type"))?;
        let native_type = safe_native_type(native_type_capture);

        if !is_known_type(&native_type) {
            let source = make_source(record, context, "metadata-session", &native_type, None)?;
            let observation = make_observation(
                record,
                context,
                source,
                &schema_fingerprint,
                DecodeStatus::UnknownType,
            )?;
            return Ok(empty_record(
                observation,
                DecodeDisposition::unknown_type(&native_type),
                prior_state,
            ));
        }

        let semantic = matches!(
            native_type.as_str(),
            "user" | "assistant" | "system" | "result"
        );
        if semantic {
            decode_semantic_record(
                record,
                context,
                prior_state,
                &native_type,
                &schema_fingerprint,
            )
        } else {
            decode_metadata_record(
                record,
                context,
                prior_state,
                native_type,
                &schema_fingerprint,
            )
        }
    }
}

fn decode_metadata_record(
    record: &dyn RecordSource,
    context: &DecodeContext,
    prior_state: &DecoderState,
    native_type: String,
    schema_fingerprint: &str,
) -> Result<DecodedRecord, DecodeError> {
    let session_id = optional_identifier(record, &["sessionId"], MAX_ID_BYTES)?;
    let native_record_id = optional_identifier(record, &["uuid"], MAX_ID_BYTES)?;
    let timestamp = optional_identifier(record, &["timestamp"], MAX_ID_BYTES)?;
    let source = make_source(
        record,
        context,
        session_id.as_deref().unwrap_or("metadata-session"),
        &native_type,
        native_record_id.clone(),
    )?;
    let observation = make_observation(
        record,
        context,
        source.clone(),
        schema_fingerprint,
        DecodeStatus::Known,
    )?;
    decode_envelope(
        record,
        context,
        prior_state,
        RecordEnvelope {
            native_type,
            session_id,
            native_record_id,
            timestamp,
            source,
            observation,
        },
    )
}

fn decode_semantic_record(
    record: &dyn RecordSource,
    context: &DecodeContext,
    prior_state: &DecoderState,
    native_type: &str,
    schema_fingerprint: &str,
) -> Result<DecodedRecord, DecodeError> {
    let session_id = optional_identifier(record, &["sessionId"], MAX_ID_BYTES)?
        .ok_or(DecodeError::MissingIdentity("sessionId"))?;
    let record_identity = capture_identity_digest(record, &["uuid"], MAX_ID_BYTES)?;
    let timestamp = capture_nullable_text(record, &["timestamp"], MAX_ID_BYTES)?;
    let (IdentityCapture::Valid(record_identity), TextCapture::Valid(timestamp)) =
        (&record_identity, &timestamp)
    else {
        let native_record_id = match record_identity {
            IdentityCapture::Valid(identity) => Some(opaque_graph_id(
                "record",
                &session_id,
                identity.total_bytes,
                &identity.hash,
            )),
            IdentityCapture::Missing | IdentityCapture::Invalid => None,
        };
        let source = make_source(record, context, &session_id, native_type, native_record_id)?;
        let observation = make_observation(
            record,
            context,
            source,
            schema_fingerprint,
            DecodeStatus::Malformed,
        )?;
        return Ok(empty_record(
            observation,
            DecodeDisposition::malformed("missing_required_identity"),
            prior_state,
        ));
    };
    let occurred_at = OffsetDateTime::parse(timestamp.as_str(), &Rfc3339)
        .map_err(|_| DecodeError::Malformed("invalid-timestamp".to_owned()))?;
    let native_record_id = opaque_graph_id(
        "record",
        &session_id,
        record_identity.total_bytes,
        &record_identity.hash,
    );
    let assistant_record_id = opaque_graph_id(
        "assistant-record",
        &session_id,
        record_identity.total_bytes,
        &record_identity.hash,
    );
    let GraphCapture::Valid(graph) = capture_graph_fields(record, &session_id)? else {
        let source = make_source(
            record,
            context,
            &session_id,
            native_type,
            Some(native_record_id),
        )?;
        let observation = make_observation(
            record,
            context,
            source,
            schema_fingerprint,
            DecodeStatus::Malformed,
        )?;
        return Ok(empty_record(
            observation,
            DecodeDisposition::malformed("invalid_graph_identity"),
            prior_state,
        ));
    };
    let source = make_source(
        record,
        context,
        &session_id,
        native_type,
        Some(native_record_id.clone()),
    )?;
    let observation = make_observation(
        record,
        context,
        source.clone(),
        schema_fingerprint,
        DecodeStatus::Known,
    )?;
    let identity = SemanticIdentity {
        session_id,
        uuid: native_record_id,
        assistant_record_id,
        occurred_at,
    };
    decode_activity_record(
        record,
        context,
        prior_state,
        ActivityRecord {
            native_type,
            source: &source,
            observation,
            identity: &identity,
            graph: &graph,
        },
    )
}

struct RecordEnvelope {
    native_type: String,
    session_id: Option<String>,
    native_record_id: Option<String>,
    timestamp: Option<String>,
    source: SourceRef,
    observation: SourceObservation,
}

fn decode_envelope(
    record: &dyn RecordSource,
    context: &DecodeContext,
    prior_state: &DecoderState,
    envelope: RecordEnvelope,
) -> Result<DecodedRecord, DecodeError> {
    if !is_known_type(&envelope.native_type) {
        return Ok(empty_record(
            envelope.observation,
            DecodeDisposition::unknown_type(&envelope.native_type),
            prior_state,
        ));
    }
    if is_metadata_type(&envelope.native_type)
        && !matches!(envelope.native_type.as_str(), "mode" | "permission-mode")
    {
        return Ok(empty_record(
            envelope.observation,
            DecodeDisposition::Known,
            prior_state,
        ));
    }
    let (Some(session_id), Some(uuid), Some(timestamp)) = (
        envelope.session_id,
        envelope.native_record_id,
        envelope.timestamp,
    ) else {
        return Ok(empty_record(
            envelope.observation,
            DecodeDisposition::Known,
            prior_state,
        ));
    };
    let identity = SemanticIdentity {
        assistant_record_id: opaque_graph_id_from_bytes(
            "assistant-record",
            &session_id,
            uuid.as_bytes(),
        ),
        session_id,
        uuid,
        occurred_at: OffsetDateTime::parse(&timestamp, &Rfc3339)
            .map_err(|_| DecodeError::Malformed("invalid-timestamp".to_owned()))?,
    };
    decode_activity_record(
        record,
        context,
        prior_state,
        ActivityRecord {
            native_type: &envelope.native_type,
            source: &envelope.source,
            observation: envelope.observation,
            identity: &identity,
            graph: &GraphFields::default(),
        },
    )
}

struct ActivityRecord<'a> {
    native_type: &'a str,
    source: &'a SourceRef,
    observation: SourceObservation,
    identity: &'a SemanticIdentity,
    graph: &'a GraphFields,
}

fn decode_activity_record(
    record: &dyn RecordSource,
    context: &DecodeContext,
    prior_state: &DecoderState,
    activity: ActivityRecord<'_>,
) -> Result<DecodedRecord, DecodeError> {
    let ActivityRecord {
        native_type,
        source,
        observation,
        identity,
        graph,
    } = activity;
    let mut state = ClaudeStateV1::decode(prior_state)?;
    let mut output = Output::default();
    let spawn_request_event_id = graph
        .source_assistant_record_id
        .as_deref()
        .and_then(|assistant| state.assistant_spawn_request(assistant));
    let scope = EventScope {
        context,
        source,
        identity,
        parent_record_id: graph.parent_record_id.as_deref(),
        spawn_request_event_id: spawn_request_event_id.as_deref(),
    };
    let decoded = (|| {
        // Terminal records may close retained starts, but never manufacture a
        // new lifecycle after the bounded start history has expired.
        if native_type != "result" {
            emit_agent_starts(scope, &graph.agent_ids, &mut state, &mut output)?;
        }
        if graph.is_sidechain {
            emit_diagnostic(
                scope,
                &mut output,
                322,
                "sidechain-relationship",
                "relationship.sidechain",
                b"normalized sidechain relationship observed",
                PrivacyLabel::SyncEligible,
            )?;
        }
        emit_context_change(record, scope, &mut state, &mut output)?;
        let terminal_outcome = match native_type {
            "user" => {
                decode_user(record, scope, &mut state, &mut output)?;
                None
            }
            "assistant" => {
                decode_assistant(record, scope, &mut state, &mut output)?;
                None
            }
            "system" => {
                decode_system(record, scope, &mut output)?;
                None
            }
            "result" => decode_result(record)?,
            _ => None,
        };
        if let Some(outcome) = terminal_outcome {
            emit_agent_finishes(scope, &graph.agent_ids, outcome, &mut state, &mut output)?;
        }
        Ok(())
    })();
    if matches!(decoded, Err(DecodeError::OutputTooLarge)) {
        return Ok(empty_record(
            observation,
            DecodeDisposition::oversized("claude-output"),
            prior_state,
        ));
    }
    decoded?;
    let next_state = state.encode_bounded()?;
    output.reserve(next_state.as_bytes().len())?;
    Ok(DecodedRecord::new(
        DecodedRecordDraft {
            observation,
            events: output.events,
            evidence: output.evidence,
            disposition: DecodeDisposition::Known,
            next_state,
            semantic_bytes: output.semantic_bytes,
        },
        prior_state,
    ))
}

fn empty_record(
    observation: SourceObservation,
    disposition: DecodeDisposition,
    prior_state: &DecoderState,
) -> DecodedRecord {
    DecodedRecord::new(
        DecodedRecordDraft {
            observation,
            events: Vec::new(),
            evidence: Vec::new(),
            disposition,
            next_state: prior_state.clone(),
            semantic_bytes: 0,
        },
        prior_state,
    )
}

struct SemanticIdentity {
    session_id: String,
    uuid: String,
    assistant_record_id: String,
    occurred_at: OffsetDateTime,
}

#[derive(Default)]
struct GraphFields {
    parent_record_id: Option<String>,
    source_assistant_record_id: Option<String>,
    agent_ids: Vec<String>,
    is_sidechain: bool,
}

enum GraphCapture {
    Valid(GraphFields),
    Invalid,
}

#[derive(Clone, Copy)]
struct EventScope<'a> {
    context: &'a DecodeContext,
    source: &'a SourceRef,
    identity: &'a SemanticIdentity,
    parent_record_id: Option<&'a str>,
    spawn_request_event_id: Option<&'a str>,
}

#[derive(Clone, Copy)]
struct ContentOwner<'a> {
    project_root: Option<&'a Path>,
    event_id: &'a EventId,
    source_identity: &'a SourceIdentity,
    ordinal: u32,
}

#[derive(Default)]
struct Output {
    events: Vec<ActivityEventV1>,
    evidence: Vec<DecodedEvidence>,
    semantic_bytes: usize,
}

impl Output {
    fn reserve(&mut self, bytes: usize) -> Result<(), DecodeError> {
        let next = self
            .semantic_bytes
            .checked_add(bytes)
            .and_then(|value| value.checked_add(SEMANTIC_HEADROOM))
            .ok_or(DecodeError::OutputTooLarge)?;
        if next > MAX_RECORD_SEMANTIC_BYTES {
            return Err(DecodeError::OutputTooLarge);
        }
        self.semantic_bytes = self
            .semantic_bytes
            .checked_add(bytes)
            .ok_or(DecodeError::OutputTooLarge)?;
        Ok(())
    }

    fn push_event(&mut self, event: ActivityEventV1) -> Result<(), DecodeError> {
        if self.events.len() == MAX_EVENTS_PER_RECORD {
            return Err(DecodeError::OutputTooLarge);
        }
        let bytes = serde_json::to_vec(&event)
            .map_err(|_| DecodeError::Malformed("measure-event".to_owned()))?
            .len();
        self.reserve(bytes)?;
        self.events.push(event);
        Ok(())
    }

    fn push_evidence(&mut self, evidence: DecodedEvidence) -> Result<(), DecodeError> {
        if self.evidence.len() == MAX_EVIDENCE_PER_RECORD {
            return Err(DecodeError::OutputTooLarge);
        }
        self.reserve(
            evidence
                .plaintext
                .len()
                .checked_add(512)
                .ok_or(DecodeError::OutputTooLarge)?,
        )?;
        self.evidence.push(evidence);
        Ok(())
    }
}

fn decode_user(
    record: &dyn RecordSource,
    scope: EventScope<'_>,
    state: &mut ClaudeStateV1,
    output: &mut Output,
) -> Result<(), DecodeError> {
    let mut message_parts = Vec::new();
    let CollectedBlocks {
        mut blocks,
        mut remaining_projection_bytes,
    } = collect_blocks(record)?;
    if let Some(content) = capture_single_string(
        record,
        &["message", "content"],
        MAX_CAPTURE_BYTES.min(remaining_projection_bytes),
        false,
    )?
    .0
    {
        charge_projection(&mut remaining_projection_bytes, content.bytes.len())?;
        message_parts.push(content);
    }
    if let Some(content) =
        capture_message_block_text(record, &blocks, &mut remaining_projection_bytes)?
    {
        message_parts.push(content);
    }
    prepare_top_result_fallback(record, &mut blocks, &mut remaining_projection_bytes)?;
    let mut deferred_results = Vec::new();
    for (index, block) in blocks {
        if block.kind.as_deref() == Some("tool_result") {
            deferred_results.push((index, block));
        }
    }
    if let Some(content) = combine_text(message_parts)? {
        emit_message(scope, Actor::Human, content, 0, output)?;
        state.set_last_human_turn(scope.identity.uuid.clone())?;
    }
    for (index, block) in deferred_results {
        emit_tool_result(scope, state, output, index, block)?;
    }
    Ok(())
}

fn decode_assistant(
    record: &dyn RecordSource,
    scope: EventScope<'_>,
    state: &mut ClaudeStateV1,
    output: &mut Output,
) -> Result<(), DecodeError> {
    let mut message_parts = Vec::new();
    let CollectedBlocks {
        blocks,
        mut remaining_projection_bytes,
    } = collect_blocks(record)?;
    if let Some(content) = capture_single_string(
        record,
        &["message", "content"],
        MAX_CAPTURE_BYTES.min(remaining_projection_bytes),
        false,
    )?
    .0
    {
        charge_projection(&mut remaining_projection_bytes, content.bytes.len())?;
        message_parts.push(content);
    }
    if let Some(content) =
        capture_message_block_text(record, &blocks, &mut remaining_projection_bytes)?
    {
        message_parts.push(content);
    }
    let mut deferred_tools = Vec::new();
    for (index, block) in blocks {
        if block.kind.as_deref() == Some("tool_use") {
            deferred_tools.push((index, block));
        }
    }
    if let Some(content) = combine_text(message_parts)? {
        emit_message(scope, Actor::Agent, content, 0, output)?;
    }
    let mut eligible_spawns = Vec::new();
    for (index, block) in deferred_tools {
        let emitted = emit_tool_request(
            scope,
            state,
            output,
            index,
            block,
            scope.context.project_root.as_deref(),
        )?;
        if emitted.eligible_spawn {
            eligible_spawns.push(emitted.event_id);
        }
    }
    let spawn_request = match eligible_spawns.as_slice() {
        [event_id] => Some(event_id.as_str().to_owned()),
        _ => None,
    };
    state.set_assistant_spawn(scope.identity.assistant_record_id.clone(), spawn_request)?;
    if assistant_error_present(record)? {
        emit_private_diagnostic(
            scope,
            output,
            362,
            "assistant-error",
            "error",
            b"assistant error observed",
        )?;
    }
    Ok(())
}

#[derive(Default)]
struct Block {
    kind: Option<String>,
    text: Option<SecureCapturedString>,
    tool_id: Option<String>,
    tool_name: Option<String>,
    raw_input: Option<SecureCapturedString>,
    file_path: Option<Zeroizing<String>>,
    result_id: Option<String>,
    result_text: Vec<(usize, SecureCapturedString)>,
    is_error: Option<bool>,
}

struct CollectedBlocks {
    blocks: BTreeMap<usize, Block>,
    remaining_projection_bytes: usize,
}

struct IdentityDigest {
    total_bytes: u64,
    hash: String,
}

enum IdentityCapture {
    Missing,
    Valid(IdentityDigest),
    Invalid,
}

enum TextCapture {
    Missing,
    Valid(Zeroizing<String>),
    Invalid,
}

fn capture_graph_fields(
    record: &dyn RecordSource,
    session_id: &str,
) -> Result<GraphCapture, DecodeError> {
    let Ok(parent_record_id) = normalized_optional_graph_id(
        capture_identity_digest(record, &["parentUuid"], MAX_ID_BYTES)?,
        "record",
        session_id,
    ) else {
        return Ok(GraphCapture::Invalid);
    };
    let Ok(source_assistant_record_id) = normalized_optional_graph_id(
        capture_identity_digest(record, &["sourceToolAssistantUUID"], MAX_ID_BYTES)?,
        "assistant-record",
        session_id,
    ) else {
        return Ok(GraphCapture::Invalid);
    };
    let mut agent_ids = Vec::with_capacity(2);
    for path in [&["agentId"][..], &["attributionAgent"][..]] {
        let Ok(agent) = normalized_optional_graph_id(
            capture_identity_digest(record, path, MAX_ID_BYTES)?,
            "agent",
            session_id,
        ) else {
            return Ok(GraphCapture::Invalid);
        };
        if let Some(agent) = agent
            && !agent_ids.iter().any(|known| known == &agent)
        {
            agent_ids.push(agent);
        }
    }
    let is_sidechain = match capture_optional_bool(record, &["isSidechain"])? {
        Ok(value) => value.unwrap_or(false),
        Err(()) => return Ok(GraphCapture::Invalid),
    };
    Ok(GraphCapture::Valid(GraphFields {
        parent_record_id,
        source_assistant_record_id,
        agent_ids,
        is_sidechain,
    }))
}

fn normalized_optional_graph_id(
    capture: IdentityCapture,
    domain: &str,
    session_id: &str,
) -> Result<Option<String>, ()> {
    match capture {
        IdentityCapture::Missing => Ok(None),
        IdentityCapture::Valid(identity) => Ok(Some(opaque_graph_id(
            domain,
            session_id,
            identity.total_bytes,
            &identity.hash,
        ))),
        IdentityCapture::Invalid => Err(()),
    }
}

fn capture_identity_digest(
    record: &dyn RecordSource,
    path: &[&str],
    limit: usize,
) -> Result<IdentityCapture, DecodeError> {
    let mut reader = BoundedJsonReader::new(record.open()?);
    let matches = reader.capture_matches(path, 0, 2, 8)?;
    if matches.len() > 1 {
        return Ok(IdentityCapture::Invalid);
    }
    Ok(matches
        .into_iter()
        .next()
        .map_or(IdentityCapture::Missing, |captured| match captured.value {
            CapturedValue::String(value)
                if value.total_bytes > 0 && value.total_bytes <= limit as u64 =>
            {
                IdentityCapture::Valid(IdentityDigest {
                    total_bytes: value.total_bytes,
                    hash: value.hash,
                })
            }
            CapturedValue::Scalar(value) if value == "null" => IdentityCapture::Missing,
            CapturedValue::String(_) | CapturedValue::Scalar(_) | CapturedValue::Container => {
                IdentityCapture::Invalid
            }
        }))
}

fn capture_nullable_text(
    record: &dyn RecordSource,
    path: &[&str],
    limit: usize,
) -> Result<TextCapture, DecodeError> {
    let mut reader = BoundedJsonReader::new(record.open()?);
    let matches = reader.capture_matches(path, limit, 2, limit.saturating_mul(2))?;
    if matches.len() > 1 {
        return Ok(TextCapture::Invalid);
    }
    Ok(matches
        .into_iter()
        .next()
        .map_or(TextCapture::Missing, |captured| match captured.value {
            CapturedValue::String(mut value)
                if !value.truncated
                    && value.total_bytes > 0
                    && value.total_bytes <= limit as u64 =>
            {
                String::from_utf8(value.take_bytes()).map_or(TextCapture::Invalid, |value| {
                    TextCapture::Valid(Zeroizing::new(value))
                })
            }
            CapturedValue::Scalar(value) if value == "null" => TextCapture::Missing,
            CapturedValue::String(_) | CapturedValue::Scalar(_) | CapturedValue::Container => {
                TextCapture::Invalid
            }
        }))
}

fn capture_optional_bool(
    record: &dyn RecordSource,
    path: &[&str],
) -> Result<Result<Option<bool>, ()>, DecodeError> {
    let mut reader = BoundedJsonReader::new(record.open()?);
    let matches = reader.capture_matches(path, 5, 2, 10)?;
    if matches.len() > 1 {
        return Ok(Err(()));
    }
    Ok(matches
        .into_iter()
        .next()
        .map_or(Ok(None), |captured| match captured.value {
            CapturedValue::Scalar(value) if value == "true" => Ok(Some(true)),
            CapturedValue::Scalar(value) if value == "false" => Ok(Some(false)),
            CapturedValue::Scalar(value) if value == "null" => Ok(None),
            CapturedValue::String(_) | CapturedValue::Scalar(_) | CapturedValue::Container => {
                Err(())
            }
        }))
}

fn opaque_graph_id(domain: &str, session_id: &str, total_bytes: u64, hash: &str) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"agbox-claude-graph-identity-v1");
    hasher.update(&(domain.len() as u64).to_le_bytes());
    hasher.update(domain.as_bytes());
    hasher.update(b"claude");
    hasher.update(&(session_id.len() as u64).to_le_bytes());
    hasher.update(session_id.as_bytes());
    hasher.update(&total_bytes.to_le_bytes());
    hasher.update(hash.as_bytes());
    format!("claude_graph_{}", &hasher.finalize().to_hex()[..48])
}

fn opaque_graph_id_from_bytes(domain: &str, session_id: &str, value: &[u8]) -> String {
    opaque_graph_id(
        domain,
        session_id,
        u64::try_from(value.len()).unwrap_or(u64::MAX),
        blake3::hash(value).to_hex().as_str(),
    )
}

fn emit_agent_starts(
    scope: EventScope<'_>,
    agent_ids: &[String],
    state: &mut ClaudeStateV1,
    output: &mut Output,
) -> Result<(), DecodeError> {
    for (index, agent_id) in agent_ids.iter().enumerate() {
        if !state.observe_agent(agent_id)? {
            continue;
        }
        let ordinal = 300_u32
            .checked_add(u32::try_from(index).map_err(|_| DecodeError::OutputTooLarge)?)
            .ok_or(DecodeError::OutputTooLarge)?;
        let source_identity = source_identity(scope.source, scope.context);
        let event = make_event(
            scope,
            EventId::from_source(&source_identity, ordinal),
            SemanticKey::from_native(
                Provider::Claude,
                &scope.identity.session_id,
                "agent-start",
                agent_id,
            ),
            Actor::System,
            scope.spawn_request_event_id.map(str::to_owned),
            None,
            EventPayload::AgentStarted {
                native_agent_id: agent_id.clone(),
            },
        )?;
        output.push_event(event)?;
    }
    Ok(())
}

fn emit_agent_finishes(
    scope: EventScope<'_>,
    agent_ids: &[String],
    outcome: ActionOutcome,
    state: &mut ClaudeStateV1,
    output: &mut Output,
) -> Result<(), DecodeError> {
    for (index, agent_id) in agent_ids.iter().enumerate() {
        if !state.finish_agent(agent_id, outcome)? {
            continue;
        }
        let ordinal = 340_u32
            .checked_add(u32::try_from(index).map_err(|_| DecodeError::OutputTooLarge)?)
            .ok_or(DecodeError::OutputTooLarge)?;
        let source_identity = source_identity(scope.source, scope.context);
        let event = make_event(
            scope,
            EventId::from_source(&source_identity, ordinal),
            SemanticKey::from_native(
                Provider::Claude,
                &scope.identity.session_id,
                "agent-finish",
                agent_id,
            ),
            Actor::System,
            scope.spawn_request_event_id.map(str::to_owned),
            None,
            EventPayload::AgentFinished {
                native_agent_id: agent_id.clone(),
                outcome,
            },
        )?;
        output.push_event(event)?;
    }
    Ok(())
}

fn decode_system(
    record: &dyn RecordSource,
    scope: EventScope<'_>,
    output: &mut Output,
) -> Result<(), DecodeError> {
    let subtype = optional_identifier(record, &["subtype"], MAX_ID_BYTES)?;
    let Some(subtype) = subtype.as_deref() else {
        return Ok(());
    };
    match subtype {
        "compact_boundary" | "compacted" | "context_compacted" => {
            let source_identity = source_identity(scope.source, scope.context);
            let event = make_event(
                scope,
                EventId::from_source(&source_identity, 320),
                SemanticKey::from_native(
                    Provider::Claude,
                    &scope.identity.session_id,
                    "context-compact",
                    &scope.identity.uuid,
                ),
                Actor::System,
                None,
                None,
                EventPayload::ContextCompacted {
                    summary_hash: Some(scope.source.record_hash().to_owned()),
                },
            )?;
            output.push_event(event)?;
            Ok(())
        }
        "turn_duration" => {
            let source_identity = source_identity(scope.source, scope.context);
            let event = make_event(
                scope,
                EventId::from_source(&source_identity, 321),
                SemanticKey::from_native(
                    Provider::Claude,
                    &scope.identity.session_id,
                    "turn-finish",
                    &scope.identity.uuid,
                ),
                Actor::System,
                None,
                None,
                EventPayload::TurnFinished {
                    outcome: ActionOutcome::Succeeded,
                },
            )?;
            output.push_event(event)?;
            Ok(())
        }
        "stop_hook_summary" => {
            emit_private_diagnostic(
                scope,
                output,
                360,
                "stop-hook-summary",
                "info",
                b"stop hook summary observed",
            )?;
            Ok(())
        }
        "away_summary" => {
            emit_private_diagnostic(
                scope,
                output,
                361,
                "away-summary",
                "info",
                b"away summary observed",
            )?;
            Ok(())
        }
        _ => Ok(()),
    }
}

fn decode_result(record: &dyn RecordSource) -> Result<Option<ActionOutcome>, DecodeError> {
    let subtype = optional_identifier(record, &["subtype"], MAX_ID_BYTES)?;
    Ok(match subtype.as_deref() {
        Some("success" | "agent_finished") => Some(ActionOutcome::Succeeded),
        Some("error" | "failed") => Some(ActionOutcome::Failed),
        Some("cancelled") => Some(ActionOutcome::Cancelled),
        _ => None,
    })
}

fn assistant_error_present(record: &dyn RecordSource) -> Result<bool, DecodeError> {
    let mut positive_locations = 0_u8;
    for path in [&["error"][..], &["message", "error"][..]] {
        let matches = capture_matches_bounded(record, path, MAX_ID_BYTES, MAX_ID_BYTES * 2)?;
        if matches.len() > 1 {
            return Err(DecodeError::Malformed(
                "duplicate-assistant-error".to_owned(),
            ));
        }
        if let Some(captured) = matches.into_iter().next() {
            let present = match captured.value {
                CapturedValue::String(value)
                    if value.truncated || value.total_bytes > MAX_ID_BYTES as u64 =>
                {
                    return Err(DecodeError::Malformed(
                        "oversized-assistant-error".to_owned(),
                    ));
                }
                CapturedValue::String(value) => value.total_bytes > 0,
                CapturedValue::Scalar(value) if value == "true" => true,
                CapturedValue::Scalar(value) if matches!(value.as_str(), "false" | "null") => false,
                CapturedValue::Scalar(_) | CapturedValue::Container => {
                    return Err(DecodeError::Malformed("invalid-assistant-error".to_owned()));
                }
            };
            if present {
                positive_locations = positive_locations.saturating_add(1);
            }
        }
    }
    if positive_locations > 1 {
        return Err(DecodeError::Malformed(
            "duplicate-assistant-error".to_owned(),
        ));
    }
    Ok(positive_locations == 1)
}

fn emit_private_diagnostic(
    scope: EventScope<'_>,
    output: &mut Output,
    ordinal: u32,
    semantic_kind: &str,
    level: &str,
    safe_message: &[u8],
) -> Result<(), DecodeError> {
    emit_diagnostic(
        scope,
        output,
        ordinal,
        semantic_kind,
        level,
        safe_message,
        PrivacyLabel::PrivateLocal,
    )
}

#[allow(clippy::too_many_arguments)]
fn emit_diagnostic(
    scope: EventScope<'_>,
    output: &mut Output,
    ordinal: u32,
    semantic_kind: &str,
    level: &str,
    safe_message: &[u8],
    privacy: PrivacyLabel,
) -> Result<(), DecodeError> {
    let source_identity = source_identity(scope.source, scope.context);
    let event_id = EventId::from_source(&source_identity, ordinal);
    let message = make_content(
        capture_from_derived(safe_message)?,
        DisclosureClass::ObservedState,
        "text/plain",
        ContentOwner {
            project_root: None,
            event_id: &event_id,
            source_identity: &source_identity,
            ordinal,
        },
        output,
    )?;
    let event = make_event_with_privacy(
        scope,
        event_id,
        SemanticKey::from_native(
            Provider::Claude,
            &scope.identity.session_id,
            semantic_kind,
            &scope.identity.uuid,
        ),
        Actor::System,
        None,
        None,
        EventPayload::DiagnosticObserved {
            level: level.to_owned(),
            message,
        },
        privacy,
    )?;
    output.push_event(event)
}

fn emit_context_change(
    record: &dyn RecordSource,
    scope: EventScope<'_>,
    state: &mut ClaudeStateV1,
    output: &mut Output,
) -> Result<(), DecodeError> {
    let cwd = optional_bounded_text(record, &["cwd"], MAX_PATH_BYTES)?.map(Zeroizing::new);
    let normalized_cwd = cwd
        .as_deref()
        .and_then(|value| normalize_context_path(value, scope.context.project_root.as_deref()));
    let mode = optional_bounded_text(record, &["mode"], MAX_TOOL_NAME_BYTES)?.map(Zeroizing::new);
    let permission_mode = optional_bounded_text(record, &["permissionMode"], MAX_TOOL_NAME_BYTES)?
        .or(optional_bounded_text(
            record,
            &["permission-mode"],
            MAX_TOOL_NAME_BYTES,
        )?)
        .map(Zeroizing::new);
    let branch = capture_single_string(record, &["gitBranch"], 0, true)?.0;
    let branch_hash = branch.map(|value| {
        let mut hasher = blake3::Hasher::new();
        hasher.update(b"agbox-claude-branch-v1");
        hasher.update(&value.total_bytes.to_le_bytes());
        hasher.update(value.hash.as_bytes());
        hasher.finalize().to_hex().to_string()
    });
    let mode = mode
        .as_ref()
        .and_then(|value| canonical_context_mode(value.as_str()));
    let permission_mode = permission_mode
        .as_ref()
        .and_then(|value| canonical_permission_mode(value.as_str()));
    let Some(snapshot) = state.merge_context(normalized_cwd, mode, permission_mode, branch_hash)?
    else {
        return Ok(());
    };
    let mut fields = Vec::new();
    if let Some(cwd) = snapshot.cwd {
        fields.push(format!("cwd={cwd}"));
    }
    if let Some(mode) = snapshot.mode {
        fields.push(format!("mode={mode}"));
    }
    if let Some(permission) = snapshot.permission {
        fields.push(format!("permission={permission}"));
    }
    let branch_hash = snapshot.branch_hash;
    let context_text = fields.join(";");
    let mut fingerprint = blake3::Hasher::new();
    fingerprint.update(b"agbox-claude-context-v1");
    fingerprint.update(context_text.as_bytes());
    if let Some(branch_hash) = &branch_hash {
        fingerprint.update(branch_hash.as_bytes());
    }
    let context_hash = fingerprint.finalize().to_hex().to_string();

    let source_identity = source_identity(scope.source, scope.context);
    let event_id = EventId::from_source(&source_identity, 256);
    let content_ref = make_content(
        capture_from_derived(context_text.as_bytes())?,
        DisclosureClass::ObservedState,
        "text/plain",
        ContentOwner {
            project_root: None,
            event_id: &event_id,
            source_identity: &source_identity,
            ordinal: 256,
        },
        output,
    )?;
    let event = make_event(
        scope,
        event_id,
        SemanticKey::from_native(
            Provider::Claude,
            &scope.identity.session_id,
            "session-context",
            &context_hash,
        ),
        Actor::System,
        None,
        None,
        EventPayload::SessionContextChanged {
            context: content_ref,
            branch_hash,
        },
    )?;
    output.push_event(event)
}

fn combine_text(
    parts: Vec<SecureCapturedString>,
) -> Result<Option<SecureCapturedString>, DecodeError> {
    if parts.is_empty() {
        return Ok(None);
    }
    if parts.len() == 1 {
        return Ok(parts.into_iter().next());
    }
    if parts.iter().any(|part| part.truncated) {
        return Ok(None);
    }
    let mut total_bytes = 0_u64;
    let mut hasher = blake3::Hasher::new();
    let mut bytes = Zeroizing::new(Vec::with_capacity(
        MAX_CAPTURE_BYTES.min(parts.iter().map(|part| part.bytes.len()).sum::<usize>()),
    ));
    let mut capture_open = true;
    for (index, part) in parts.iter().enumerate() {
        if index > 0 {
            total_bytes = total_bytes
                .checked_add(1)
                .ok_or(DecodeError::OutputTooLarge)?;
            hasher.update(b"\n");
            if capture_open && bytes.len() < MAX_CAPTURE_BYTES {
                bytes.push(b'\n');
            } else {
                capture_open = false;
            }
        }
        if part.total_bytes
            != u64::try_from(part.bytes.len()).map_err(|_| DecodeError::OutputTooLarge)?
        {
            return Err(DecodeError::Malformed(
                "invalid-complete-text-capture".to_owned(),
            ));
        }
        total_bytes = total_bytes
            .checked_add(part.total_bytes)
            .ok_or(DecodeError::OutputTooLarge)?;
        hasher.update(&part.bytes);
        if capture_open
            && bytes
                .len()
                .checked_add(part.bytes.len())
                .is_some_and(|length| length <= MAX_CAPTURE_BYTES)
        {
            bytes.extend_from_slice(&part.bytes);
        } else {
            capture_open = false;
        }
    }
    Ok(Some(SecureCapturedString {
        bytes,
        total_bytes,
        hash: hasher.finalize().to_hex().to_string(),
        truncated: !capture_open || total_bytes > MAX_CAPTURE_BYTES as u64,
    }))
}

fn capture_message_block_text(
    record: &dyn RecordSource,
    blocks: &BTreeMap<usize, Block>,
    remaining_projection_bytes: &mut usize,
) -> Result<Option<SecureCapturedString>, DecodeError> {
    let selected_indices = blocks
        .iter()
        .filter(|(_, block)| block.kind.as_deref() == Some("text") && block.text.is_some())
        .map(|(index, _)| vec![*index])
        .collect::<Vec<_>>();
    if selected_indices.is_empty() {
        return Ok(None);
    }
    let mut reader = BoundedJsonReader::new(record.open()?);
    let capture = reader.capture_joined_matches(
        &["message", "content", "text"],
        &selected_indices,
        MAX_CAPTURE_BYTES.min(*remaining_projection_bytes),
        MAX_MATCHES,
    )?;
    if let Some(value) = &capture {
        charge_projection(remaining_projection_bytes, value.bytes.len())?;
    }
    Ok(capture)
}

fn prepare_top_result_fallback(
    record: &dyn RecordSource,
    blocks: &mut BTreeMap<usize, Block>,
    remaining_projection_bytes: &mut usize,
) -> Result<(), DecodeError> {
    let result_indices = blocks
        .iter()
        .filter(|(_, block)| block.kind.as_deref() == Some("tool_result"))
        .map(|(index, _)| *index)
        .collect::<Vec<_>>();
    let [result_index] = result_indices.as_slice() else {
        return Ok(());
    };
    let block = blocks
        .get_mut(result_index)
        .ok_or_else(|| DecodeError::Malformed("missing-tool-result-block".to_owned()))?;
    if block.result_text.is_empty() {
        let fallback = collect_top_result_text(record, *remaining_projection_bytes)?;
        let retained = fallback.iter().try_fold(0_usize, |total, value| {
            total
                .checked_add(value.bytes.len())
                .ok_or(DecodeError::OutputTooLarge)
        })?;
        charge_projection(remaining_projection_bytes, retained)?;
        block.result_text.extend(fallback.into_iter().enumerate());
    }
    if block.is_error.is_none() {
        block.is_error = optional_bool(record, &["toolUseResult", "isError"])?;
    }
    Ok(())
}

fn charge_projection(remaining: &mut usize, retained: usize) -> Result<(), DecodeError> {
    *remaining = remaining
        .checked_sub(retained)
        .ok_or(DecodeError::OutputTooLarge)?;
    Ok(())
}

fn collect_blocks(record: &dyn RecordSource) -> Result<CollectedBlocks, DecodeError> {
    let mut blocks = BTreeMap::<usize, Block>::new();
    let projection_budget = MAX_RECORD_SEMANTIC_BYTES
        .checked_sub(SEMANTIC_HEADROOM)
        .ok_or(DecodeError::OutputTooLarge)?;
    collect_request_fields(record, &mut blocks, projection_budget)?;
    collect_result_fields(record, &mut blocks, projection_budget)?;
    let remaining_projection_bytes = projection_budget.saturating_sub(projected_bytes(&blocks)?);
    Ok(CollectedBlocks {
        blocks,
        remaining_projection_bytes,
    })
}

fn collect_request_fields(
    record: &dyn RecordSource,
    blocks: &mut BTreeMap<usize, Block>,
    projection_budget: usize,
) -> Result<(), DecodeError> {
    insert_string_field(
        blocks,
        capture_matches(record, &["message", "content", "type"], MAX_ID_BYTES)?,
        |block, value| {
            set_once(
                &mut block.kind,
                required_identifier(value, MAX_ID_BYTES, "block.type")?,
            )
        },
    )?;
    insert_capture_field(
        blocks,
        capture_matches_bounded(record, &["message", "content", "text"], 0, 0)?,
        |block, value| set_once(&mut block.text, value),
    )?;
    ensure_projection_budget(blocks, projection_budget)?;
    insert_string_field(
        blocks,
        capture_matches(record, &["message", "content", "id"], MAX_ID_BYTES)?,
        |block, value| {
            set_once(
                &mut block.tool_id,
                required_identifier(value, MAX_ID_BYTES, "tool_use.id")?,
            )
        },
    )?;
    insert_string_field(
        blocks,
        capture_matches(record, &["message", "content", "name"], MAX_TOOL_NAME_BYTES)?,
        |block, value| {
            set_once(
                &mut block.tool_name,
                required_identifier(value, MAX_TOOL_NAME_BYTES, "tool_use.name")?,
            )
        },
    )?;
    let remaining = projection_budget.saturating_sub(projected_bytes(blocks)?);
    insert_capture_field(
        blocks,
        capture_raw_matches(record, &["message", "content", "input"], remaining)?,
        |block, value| set_once(&mut block.raw_input, value),
    )?;
    ensure_projection_budget(blocks, projection_budget)?;
    for path in [
        &["message", "content", "input", "file_path"][..],
        &["message", "content", "input", "path"][..],
    ] {
        insert_string_field(
            blocks,
            capture_matches(record, path, MAX_PATH_BYTES)?,
            |block, mut value| {
                if value.truncated || value.total_bytes > MAX_PATH_BYTES as u64 {
                    return Ok(());
                }
                let value = Zeroizing::new(
                    String::from_utf8(value.take_bytes())
                        .map_err(|_| DecodeError::Malformed("invalid-tool-path".to_owned()))?,
                );
                match &block.file_path {
                    Some(existing) if existing != &value => {
                        Err(DecodeError::Malformed("ambiguous-tool-path".to_owned()))
                    }
                    Some(_) => Err(DecodeError::Malformed("duplicate-tool-path".to_owned())),
                    None => {
                        block.file_path = Some(value);
                        Ok(())
                    }
                }
            },
        )?;
    }
    Ok(())
}

fn collect_result_fields(
    record: &dyn RecordSource,
    blocks: &mut BTreeMap<usize, Block>,
    projection_budget: usize,
) -> Result<(), DecodeError> {
    insert_string_field(
        blocks,
        capture_matches(record, &["message", "content", "tool_use_id"], MAX_ID_BYTES)?,
        |block, value| {
            set_once(
                &mut block.result_id,
                required_identifier(value, MAX_ID_BYTES, "tool_result.tool_use_id")?,
            )
        },
    )?;
    let remaining = projection_budget.saturating_sub(projected_bytes(blocks)?);
    collect_direct_result_text(record, blocks, remaining)?;
    let remaining = projection_budget.saturating_sub(projected_bytes(blocks)?);
    collect_nested_result_text(record, blocks, remaining)?;
    ensure_projection_budget(blocks, projection_budget)?;
    insert_bool_field(
        blocks,
        capture_matches(record, &["message", "content", "is_error"], MAX_ID_BYTES)?,
        |block, value| set_once(&mut block.is_error, value),
    )?;
    Ok(())
}

fn collect_direct_result_text(
    record: &dyn RecordSource,
    blocks: &mut BTreeMap<usize, Block>,
    remaining: usize,
) -> Result<(), DecodeError> {
    let matches = capture_matches_bounded(
        record,
        &["message", "content", "content"],
        MAX_CAPTURE_BYTES.min(remaining),
        remaining,
    )?;
    let mut seen = HashSet::new();
    for captured in matches {
        let index = array_index(&captured)?;
        if !seen.insert(index) {
            return Err(DecodeError::Malformed("duplicate-content-field".to_owned()));
        }
        match captured.value {
            CapturedValue::String(value) => {
                blocks
                    .entry(index)
                    .or_default()
                    .result_text
                    .push((0, value));
            }
            CapturedValue::Container => {}
            CapturedValue::Scalar(_) => {
                return Err(DecodeError::Malformed(
                    "invalid-tool-result-content".to_owned(),
                ));
            }
        }
    }
    Ok(())
}

#[derive(Default)]
struct TypedText {
    kind: Option<String>,
    text: Option<SecureCapturedString>,
    indices: Option<Vec<usize>>,
}

fn collect_nested_result_text(
    record: &dyn RecordSource,
    blocks: &mut BTreeMap<usize, Block>,
    remaining: usize,
) -> Result<(), DecodeError> {
    let mut nested = BTreeMap::<(usize, usize), TypedText>::new();
    insert_nested_kind(
        &mut nested,
        capture_matches(
            record,
            &["message", "content", "content", "type"],
            MAX_ID_BYTES,
        )?,
    )?;
    insert_nested_text(
        &mut nested,
        capture_matches_bounded(record, &["message", "content", "content", "text"], 0, 0)?,
    )?;
    let mut selected_by_block = BTreeMap::<usize, Vec<Vec<usize>>>::new();
    for ((outer, _), value) in nested {
        if value.kind.as_deref() == Some("text")
            && value.text.is_some()
            && let Some(indices) = value.indices
        {
            selected_by_block.entry(outer).or_default().push(indices);
        }
    }
    let mut remaining = remaining;
    for (outer, selected_indices) in selected_by_block {
        let mut reader = BoundedJsonReader::new(record.open()?);
        if let Some(text) = reader.capture_joined_matches(
            &["message", "content", "content", "text"],
            &selected_indices,
            MAX_CAPTURE_BYTES.min(remaining),
            MAX_MATCHES,
        )? {
            remaining = remaining
                .checked_sub(text.bytes.len())
                .ok_or(DecodeError::OutputTooLarge)?;
            blocks.entry(outer).or_default().result_text.push((1, text));
        }
    }
    Ok(())
}

fn insert_nested_kind(
    nested: &mut BTreeMap<(usize, usize), TypedText>,
    matches: Vec<CapturedMatch>,
) -> Result<(), DecodeError> {
    let mut seen = HashSet::new();
    for captured in matches {
        let key = nested_index(&captured)?;
        if !seen.insert(key) {
            return Err(DecodeError::Malformed(
                "duplicate-nested-result-field".to_owned(),
            ));
        }
        if let CapturedValue::String(value) = captured.value {
            let entry = nested.entry(key).or_default();
            entry.indices = Some(captured.array_indices);
            entry.kind = Some(required_identifier(value, MAX_ID_BYTES, "result.type")?);
        }
    }
    Ok(())
}

fn insert_nested_text(
    nested: &mut BTreeMap<(usize, usize), TypedText>,
    matches: Vec<CapturedMatch>,
) -> Result<(), DecodeError> {
    let mut seen = HashSet::new();
    for captured in matches {
        let key = nested_index(&captured)?;
        if !seen.insert(key) {
            return Err(DecodeError::Malformed(
                "duplicate-nested-result-field".to_owned(),
            ));
        }
        if let CapturedValue::String(value) = captured.value {
            let entry = nested.entry(key).or_default();
            entry.indices = Some(captured.array_indices);
            entry.text = Some(value);
        }
    }
    Ok(())
}

fn nested_index(captured: &CapturedMatch) -> Result<(usize, usize), DecodeError> {
    match captured.array_indices.as_slice() {
        [outer] => Ok((*outer, 0)),
        [outer, inner] => Ok((*outer, *inner)),
        _ => Err(DecodeError::Malformed(
            "invalid-nested-result-location".to_owned(),
        )),
    }
}

fn projected_bytes(blocks: &BTreeMap<usize, Block>) -> Result<usize, DecodeError> {
    blocks.values().try_fold(0_usize, |total, block| {
        [&block.text, &block.raw_input]
            .into_iter()
            .flatten()
            .try_fold(total, |subtotal, capture| {
                subtotal
                    .checked_add(capture.bytes.len())
                    .ok_or(DecodeError::OutputTooLarge)
            })?
            .checked_add(
                block
                    .result_text
                    .iter()
                    .map(|(_, capture)| capture.bytes.len())
                    .sum::<usize>(),
            )
            .ok_or(DecodeError::OutputTooLarge)
    })
}

fn ensure_projection_budget(
    blocks: &BTreeMap<usize, Block>,
    limit: usize,
) -> Result<(), DecodeError> {
    if projected_bytes(blocks)? > limit {
        Err(DecodeError::OutputTooLarge)
    } else {
        Ok(())
    }
}

fn emit_message(
    scope: EventScope<'_>,
    actor: Actor,
    captured_content: SecureCapturedString,
    local_ordinal: u32,
    output: &mut Output,
) -> Result<(), DecodeError> {
    let source_identity = source_identity(scope.source, scope.context);
    let event_id = EventId::from_source(&source_identity, local_ordinal);
    let disclosure = match actor {
        Actor::Human => DisclosureClass::HumanIntent,
        Actor::Agent => DisclosureClass::AgentStatement,
        _ => DisclosureClass::ObservedState,
    };
    let content_ref = make_content(
        captured_content,
        disclosure,
        "text/plain",
        ContentOwner {
            project_root: None,
            event_id: &event_id,
            source_identity: &source_identity,
            ordinal: local_ordinal,
        },
        output,
    )?;
    let event = make_event(
        scope,
        event_id,
        SemanticKey::from_native(
            Provider::Claude,
            &scope.identity.session_id,
            "message",
            &scope.identity.uuid,
        ),
        actor,
        None,
        None,
        EventPayload::MessageCreated {
            content: content_ref,
        },
    )?;
    output.push_event(event)
}

struct EmittedToolRequest {
    event_id: EventId,
    eligible_spawn: bool,
}

fn emit_tool_request(
    scope: EventScope<'_>,
    state: &mut ClaudeStateV1,
    output: &mut Output,
    index: usize,
    block: Block,
    project_root: Option<&Path>,
) -> Result<EmittedToolRequest, DecodeError> {
    let tool_id = block
        .tool_id
        .ok_or(DecodeError::MissingIdentity("tool_use.id"))?;
    let tool_name = block
        .tool_name
        .ok_or(DecodeError::MissingIdentity("tool_use.name"))?;
    let raw_input = block
        .raw_input
        .ok_or(DecodeError::MissingIdentity("tool_use.input"))?;
    let project_path = block
        .file_path
        .as_deref()
        .and_then(|path| normalize_project_path(path, project_root));
    let local_ordinal = ordinal(index, 1)?;
    let source_identity = source_identity(scope.source, scope.context);
    let event_id = EventId::from_source(&source_identity, local_ordinal);
    let input_hash = raw_input.hash.clone();
    let input = make_content(
        raw_input,
        DisclosureClass::AgentStatement,
        "application/json",
        ContentOwner {
            project_root,
            event_id: &event_id,
            source_identity: &source_identity,
            ordinal: local_ordinal,
        },
        output,
    )?;
    let event = make_event(
        scope,
        event_id.clone(),
        SemanticKey::from_native(
            Provider::Claude,
            &scope.identity.session_id,
            "action-request",
            &tool_id,
        ),
        Actor::Agent,
        Some(tool_id.clone()),
        None,
        EventPayload::ActionRequested {
            native_action_id: tool_id.clone(),
            tool_name: tool_name.clone(),
            input,
        },
    )?;
    output.push_event(event)?;
    let eligible_spawn = is_agent_spawn_tool(&tool_name);
    state.insert_tool(ToolLink {
        tool_use_id: tool_id,
        request_event_id: event_id.as_str().to_owned(),
        tool_name,
        input_hash,
        project_relative_path: project_path,
    })?;
    Ok(EmittedToolRequest {
        event_id,
        eligible_spawn,
    })
}

fn emit_tool_result(
    scope: EventScope<'_>,
    state: &mut ClaudeStateV1,
    output: &mut Output,
    index: usize,
    mut block: Block,
) -> Result<(), DecodeError> {
    let result_id = block
        .result_id
        .ok_or(DecodeError::MissingIdentity("tool_result.tool_use_id"))?;
    let Some(link) = state.take_tool(&result_id) else {
        return Ok(());
    };
    block.result_text.sort_by_key(|(ordinal, _)| *ordinal);
    let result_parts = block
        .result_text
        .into_iter()
        .map(|(_, text)| text)
        .collect::<Vec<_>>();
    let safe_output = combine_text(result_parts)?;
    let failed = block.is_error.unwrap_or(false);
    let outcome = if failed {
        ActionOutcome::Failed
    } else {
        ActionOutcome::Succeeded
    };
    let local_ordinal = ordinal(index, 2)?;
    let source_identity = source_identity(scope.source, scope.context);
    let event_id = EventId::from_source(&source_identity, local_ordinal);
    let output_ref = safe_output
        .map(|value| {
            make_content(
                value,
                DisclosureClass::ToolResult,
                "text/plain",
                ContentOwner {
                    project_root: None,
                    event_id: &event_id,
                    source_identity: &source_identity,
                    ordinal: local_ordinal,
                },
                output,
            )
        })
        .transpose()?;
    let event = make_event(
        scope,
        event_id.clone(),
        SemanticKey::from_native(
            Provider::Claude,
            &scope.identity.session_id,
            "action-finish",
            &result_id,
        ),
        Actor::Tool,
        Some(result_id.clone()),
        Some(link.request_event_id.clone()),
        EventPayload::ActionFinished {
            native_action_id: result_id.clone(),
            outcome,
            output: output_ref,
        },
    )?;
    output.push_event(event)?;

    emit_artifact_change(
        scope,
        output,
        index,
        failed,
        result_id,
        (&source_identity, &event_id),
        link,
    )?;
    Ok(())
}

fn emit_artifact_change(
    scope: EventScope<'_>,
    output: &mut Output,
    index: usize,
    failed: bool,
    result_id: String,
    identity: (&SourceIdentity, &EventId),
    link: ToolLink,
) -> Result<(), DecodeError> {
    if failed || !is_write_tool(&link.tool_name) {
        return Ok(());
    }
    let Some(path) = link.project_relative_path else {
        return Ok(());
    };
    let (source_identity, parent_event_id) = identity;
    let artifact_ordinal = ordinal(index, 3)?;
    let artifact_event_id = EventId::from_source(source_identity, artifact_ordinal);
    let path_capture = capture_from_derived(path.as_bytes())?;
    let path_ref = make_content(
        path_capture,
        DisclosureClass::ObservedState,
        "text/uri-list",
        ContentOwner {
            project_root: None,
            event_id: &artifact_event_id,
            source_identity,
            ordinal: artifact_ordinal,
        },
        output,
    )?;
    let event = make_event(
        scope,
        artifact_event_id,
        SemanticKey::from_native(
            Provider::Claude,
            &scope.identity.session_id,
            "artifact",
            &result_id,
        ),
        Actor::Tool,
        Some(result_id),
        Some(parent_event_id.as_str().to_owned()),
        EventPayload::ArtifactChanged {
            path: path_ref,
            operation: link.tool_name,
            content_hash: None,
        },
    )?;
    output.push_event(event)
}

fn collect_top_result_text(
    record: &dyn RecordSource,
    retained_budget: usize,
) -> Result<Vec<SecureCapturedString>, DecodeError> {
    for path in [&["toolUseResult"][..], &["toolUseResult", "content"][..]] {
        if let Some(value) =
            capture_single_string(record, path, retained_budget.min(MAX_CAPTURE_BYTES), true)?.0
        {
            return Ok(vec![value]);
        }
    }
    let output = collect_top_typed_text(
        record,
        &["toolUseResult", "type"],
        &["toolUseResult", "text"],
        retained_budget,
    )?;
    if !output.is_empty() {
        return Ok(output);
    }
    collect_top_typed_text(
        record,
        &["toolUseResult", "content", "type"],
        &["toolUseResult", "content", "text"],
        retained_budget,
    )
}

fn collect_top_typed_text(
    record: &dyn RecordSource,
    type_path: &[&str],
    text_path: &[&str],
    retained_budget: usize,
) -> Result<Vec<SecureCapturedString>, DecodeError> {
    let mut values = BTreeMap::<Vec<usize>, TypedText>::new();
    let mut seen = HashSet::new();
    for captured in capture_matches(record, type_path, MAX_ID_BYTES)? {
        let key = captured.array_indices;
        if !seen.insert(key.clone()) {
            return Err(DecodeError::Malformed(
                "duplicate-top-result-field".to_owned(),
            ));
        }
        if let CapturedValue::String(value) = captured.value {
            let entry = values.entry(key.clone()).or_default();
            entry.indices = Some(key);
            entry.kind = Some(required_identifier(value, MAX_ID_BYTES, "result.type")?);
        }
    }
    seen.clear();
    for captured in capture_matches_bounded(record, text_path, 0, 0)? {
        let key = captured.array_indices;
        if !seen.insert(key.clone()) {
            return Err(DecodeError::Malformed(
                "duplicate-top-result-field".to_owned(),
            ));
        }
        if let CapturedValue::String(value) = captured.value {
            let entry = values.entry(key.clone()).or_default();
            entry.indices = Some(key);
            entry.text = Some(value);
        }
    }
    let selected_indices = values
        .into_values()
        .filter_map(|value| {
            (value.kind.as_deref() == Some("text") && value.text.is_some())
                .then_some(value.indices)
                .flatten()
        })
        .collect::<Vec<_>>();
    if selected_indices.is_empty() {
        return Ok(Vec::new());
    }
    let mut reader = BoundedJsonReader::new(record.open()?);
    Ok(reader
        .capture_joined_matches(
            text_path,
            &selected_indices,
            MAX_CAPTURE_BYTES.min(retained_budget),
            MAX_MATCHES,
        )?
        .into_iter()
        .collect())
}

fn make_event(
    scope: EventScope<'_>,
    event_id: EventId,
    semantic_key: SemanticKey,
    actor: Actor,
    correlation_id: Option<String>,
    causation_id: Option<String>,
    payload: EventPayload,
) -> Result<ActivityEventV1, DecodeError> {
    make_event_with_privacy(
        scope,
        event_id,
        semantic_key,
        actor,
        correlation_id,
        causation_id,
        payload,
        PrivacyLabel::SyncEligible,
    )
}

#[allow(clippy::too_many_arguments)]
fn make_event_with_privacy(
    scope: EventScope<'_>,
    event_id: EventId,
    semantic_key: SemanticKey,
    actor: Actor,
    correlation_id: Option<String>,
    causation_id: Option<String>,
    payload: EventPayload,
    privacy: PrivacyLabel,
) -> Result<ActivityEventV1, DecodeError> {
    let session_id = SessionId::parse_wire(&scope.identity.session_id)
        .ok_or_else(|| DecodeError::Malformed("invalid-session-id".to_owned()))?;
    ActivityEventV1::new(ActivityEventDraft {
        event_id,
        semantic_key,
        schema_version: 1,
        occurred_at: scope.identity.occurred_at,
        observed_at: scope.context.observed_at,
        project_id: scope.context.project_id.clone(),
        session_id,
        turn_id: Some(scope.identity.uuid.clone()),
        actor,
        correlation_id,
        causation_id: scope
            .parent_record_id
            .map(std::borrow::ToOwned::to_owned)
            .or(causation_id),
        source: scope.source.clone(),
        payload,
        privacy,
    })
    .map_err(|_| DecodeError::Malformed("invalid-event".to_owned()))
}

fn make_content(
    mut capture: SecureCapturedString,
    disclosure: DisclosureClass,
    media_type: &'static str,
    owner: ContentOwner<'_>,
    output: &mut Output,
) -> Result<ContentRef, DecodeError> {
    let plaintext = std::mem::take(&mut capture.bytes);
    let text = std::str::from_utf8(&plaintext)
        .map_err(|_| DecodeError::Malformed("invalid-content-utf8".to_owned()))?;
    let redacted = RedactionPolicy::new()
        .and_then(|policy| policy.redact(text, owner.project_root, disclosure))
        .map_err(|_| DecodeError::Malformed("redaction-failed".to_owned()))?;
    let evidence_id = EvidenceId::from_source(owner.source_identity, owner.ordinal);
    let locator = (!capture.truncated).then(|| LocalLocator::Evidence {
        evidence_id: evidence_id.clone(),
    });
    let content = ContentRef::bounded(
        capture.take_hash(),
        capture.total_bytes,
        media_type,
        locator,
        disclosure,
        Some(redacted),
    )
    .map_err(|_| DecodeError::Malformed("invalid-content".to_owned()))?;
    if !capture.truncated {
        output.push_evidence(DecodedEvidence {
            evidence_id,
            owner_event_id: owner.event_id.clone(),
            content: content.clone(),
            plaintext,
        })?;
    }
    Ok(content)
}

fn make_source(
    record: &dyn RecordSource,
    context: &DecodeContext,
    session_id: &str,
    native_type: &str,
    native_record_id: Option<String>,
) -> Result<SourceRef, DecodeError> {
    SourceRef::new(SourceRefDraft {
        provider: Provider::Claude,
        format: context.format.clone(),
        native_session_id: session_id.to_owned(),
        native_record_type: native_type.to_owned(),
        native_record_id,
        source_generation: context.source_generation,
        byte_offset: record.start(),
        ordinal: None,
        record_hash: record.record_hash().to_owned(),
        decoder_version: DECODER_VERSION.to_owned(),
    })
    .map_err(|_| DecodeError::Malformed("invalid-source".to_owned()))
}

fn make_observation(
    record: &dyn RecordSource,
    context: &DecodeContext,
    source: SourceRef,
    schema_fingerprint: &str,
    status: DecodeStatus,
) -> Result<SourceObservation, DecodeError> {
    let range = ByteRange::new(record.start(), record.end())
        .map_err(|_| DecodeError::Malformed("invalid-record-range".to_owned()))?;
    let source_id = context.source_id.clone();
    let length = record
        .end()
        .checked_sub(record.start())
        .ok_or_else(|| DecodeError::Malformed("invalid-record-range".to_owned()))?;
    let bounded_record = ContentRef::bounded(
        record.record_hash().to_owned(),
        length,
        "application/x-ndjson",
        Some(LocalLocator::SourceRange {
            source_id,
            generation: context.source_generation,
            byte_start: record.start(),
            byte_end: record.end(),
        }),
        DisclosureClass::ObservedState,
        None,
    )
    .map_err(|_| DecodeError::Malformed("invalid-record-content".to_owned()))?;
    SourceObservation::new(SourceObservationDraft {
        observation_id: format!(
            "obs_{}",
            &blake3::hash(
                format!(
                    "{}:{}:{}:{}",
                    context.source_id,
                    context.source_generation,
                    record.start(),
                    record.record_hash(),
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
    .map_err(|_| DecodeError::Malformed("invalid-observation".to_owned()))
}

fn source_identity(source: &SourceRef, context: &DecodeContext) -> SourceIdentity {
    SourceIdentity {
        provider: Provider::Claude,
        source_id: context.source_id.clone(),
        generation: source.source_generation(),
        byte_offset: source.byte_offset(),
        record_hash: source.record_hash().to_owned(),
    }
}

fn capture_matches(
    record: &dyn RecordSource,
    path: &[&str],
    limit: usize,
) -> Result<Vec<CapturedMatch>, DecodeError> {
    let aggregate = limit
        .checked_mul(MAX_MATCHES)
        .unwrap_or(MAX_RECORD_SEMANTIC_BYTES)
        .min(MAX_RECORD_SEMANTIC_BYTES);
    capture_matches_bounded(record, path, limit, aggregate)
}

fn capture_matches_bounded(
    record: &dyn RecordSource,
    path: &[&str],
    limit: usize,
    max_retained_bytes: usize,
) -> Result<Vec<CapturedMatch>, DecodeError> {
    let mut reader = BoundedJsonReader::new(record.open()?);
    reader.capture_matches(path, limit, MAX_MATCHES, max_retained_bytes)
}

fn capture_raw_matches(
    record: &dyn RecordSource,
    path: &[&str],
    max_retained_bytes: usize,
) -> Result<Vec<CapturedMatch>, DecodeError> {
    let mut reader = BoundedJsonReader::new(record.open()?);
    reader.capture_raw_matches(path, MAX_CAPTURE_BYTES, MAX_MATCHES, max_retained_bytes)
}

fn capture_single_string(
    record: &dyn RecordSource,
    path: &[&str],
    limit: usize,
    require_direct: bool,
) -> Result<(Option<SecureCapturedString>, String), DecodeError> {
    let mut reader = BoundedJsonReader::new(record.open()?);
    let matches = reader.capture_matches(path, limit, 2, limit.saturating_mul(2))?;
    let schema = reader
        .schema_fingerprint()
        .ok_or_else(|| DecodeError::Malformed("missing-schema".to_owned()))?
        .to_owned();
    if matches.len() > 1 {
        return Err(DecodeError::Malformed(
            "duplicate-selected-field".to_owned(),
        ));
    }
    let value = if let Some(captured) = matches.into_iter().next() {
        if require_direct && !captured.array_indices.is_empty() {
            return Err(DecodeError::Malformed(
                "invalid-identity-location".to_owned(),
            ));
        }
        match captured.value {
            CapturedValue::String(value) => Some(value),
            CapturedValue::Container => None,
            CapturedValue::Scalar(_) => {
                return Err(DecodeError::Malformed("selected-non-string".to_owned()));
            }
        }
    } else {
        None
    };
    Ok((value, schema))
}

fn optional_identifier(
    record: &dyn RecordSource,
    path: &[&str],
    limit: usize,
) -> Result<Option<String>, DecodeError> {
    capture_single_string(record, path, limit, true)?
        .0
        .map(|value| required_identifier(value, limit, "identifier"))
        .transpose()
}

fn optional_bounded_text(
    record: &dyn RecordSource,
    path: &[&str],
    limit: usize,
) -> Result<Option<String>, DecodeError> {
    let value = capture_single_string(record, path, limit, true)?.0;
    let Some(mut value) = value else {
        return Ok(None);
    };
    if value.truncated || value.total_bytes > limit as u64 {
        return Ok(None);
    }
    String::from_utf8(value.take_bytes())
        .map(Some)
        .map_err(|_| DecodeError::Malformed("invalid-context-text".to_owned()))
}

fn optional_bool(record: &dyn RecordSource, path: &[&str]) -> Result<Option<bool>, DecodeError> {
    let matches = capture_matches(record, path, 5)?;
    if matches.len() > 1 {
        return Err(DecodeError::Malformed(
            "duplicate-selected-field".to_owned(),
        ));
    }
    matches
        .into_iter()
        .next()
        .map(|captured| captured_bool(captured.value))
        .transpose()
}

fn required_identifier(
    value: SecureCapturedString,
    limit: usize,
    field: &'static str,
) -> Result<String, DecodeError> {
    let value = captured_text(value, limit, field)?;
    if bounded_identifier(&value, limit) {
        Ok(value)
    } else {
        Err(DecodeError::Malformed(format!("invalid-{field}")))
    }
}

fn safe_native_type(mut value: SecureCapturedString) -> String {
    if value.truncated || value.total_bytes > MAX_ID_BYTES as u64 {
        return "invalid-native-type".to_owned();
    }
    String::from_utf8(value.take_bytes())
        .ok()
        .filter(|value| bounded_identifier(value, MAX_ID_BYTES))
        .unwrap_or_else(|| "invalid-native-type".to_owned())
}

fn captured_text(
    mut value: SecureCapturedString,
    limit: usize,
    field: &'static str,
) -> Result<String, DecodeError> {
    if value.truncated || value.total_bytes > limit as u64 {
        return Err(DecodeError::Malformed(format!("oversized-{field}")));
    }
    String::from_utf8(value.take_bytes())
        .map_err(|_| DecodeError::Malformed(format!("invalid-{field}-utf8")))
}

fn captured_bool(value: CapturedValue) -> Result<bool, DecodeError> {
    match value {
        CapturedValue::Scalar(value) if value == "true" => Ok(true),
        CapturedValue::Scalar(value) if value == "false" => Ok(false),
        _ => Err(DecodeError::Malformed("invalid-boolean".to_owned())),
    }
}

fn array_index(captured: &CapturedMatch) -> Result<usize, DecodeError> {
    if captured.array_indices.len() != 1 {
        return Err(DecodeError::Malformed(
            "invalid-content-block-location".to_owned(),
        ));
    }
    Ok(captured.array_indices[0])
}

fn insert_capture_field<F>(
    blocks: &mut BTreeMap<usize, Block>,
    matches: Vec<CapturedMatch>,
    mut insert: F,
) -> Result<(), DecodeError>
where
    F: FnMut(&mut Block, SecureCapturedString) -> Result<(), DecodeError>,
{
    let mut seen = HashSet::new();
    for captured in matches {
        let index = array_index(&captured)?;
        if !seen.insert(index) {
            return Err(DecodeError::Malformed("duplicate-content-field".to_owned()));
        }
        let CapturedValue::String(value) = captured.value else {
            return Err(DecodeError::Malformed("selected-non-string".to_owned()));
        };
        insert(blocks.entry(index).or_default(), value)?;
    }
    Ok(())
}

fn insert_string_field<F>(
    blocks: &mut BTreeMap<usize, Block>,
    matches: Vec<CapturedMatch>,
    insert: F,
) -> Result<(), DecodeError>
where
    F: FnMut(&mut Block, SecureCapturedString) -> Result<(), DecodeError>,
{
    insert_capture_field(blocks, matches, insert)
}

fn insert_bool_field<F>(
    blocks: &mut BTreeMap<usize, Block>,
    matches: Vec<CapturedMatch>,
    mut insert: F,
) -> Result<(), DecodeError>
where
    F: FnMut(&mut Block, bool) -> Result<(), DecodeError>,
{
    let mut seen = HashSet::new();
    for captured in matches {
        let index = array_index(&captured)?;
        if !seen.insert(index) {
            return Err(DecodeError::Malformed("duplicate-content-field".to_owned()));
        }
        let value = captured_bool(captured.value)?;
        insert(blocks.entry(index).or_default(), value)?;
    }
    Ok(())
}

fn set_once<T>(slot: &mut Option<T>, value: T) -> Result<(), DecodeError> {
    if slot.is_some() {
        Err(DecodeError::Malformed("duplicate-content-field".to_owned()))
    } else {
        *slot = Some(value);
        Ok(())
    }
}

fn normalize_project_path(value: &str, root: Option<&Path>) -> Option<String> {
    if value.len() > MAX_PATH_BYTES || value.chars().any(char::is_control) {
        return None;
    }
    let root = root?;
    let path = Path::new(value);
    let relative = if path.is_absolute() {
        path.strip_prefix(root).ok()?
    } else {
        path
    };
    if relative.as_os_str().is_empty()
        || !relative
            .components()
            .all(|component| matches!(component, Component::Normal(_)))
    {
        return None;
    }
    let relative = relative.to_string_lossy();
    let normalized = format!("$PROJECT/{relative}");
    (normalized.len() <= MAX_PATH_BYTES).then_some(normalized)
}

fn normalize_context_path(value: &str, trusted_root: Option<&Path>) -> Option<String> {
    let root = trusted_root?;
    if !root.is_absolute()
        || !root
            .components()
            .all(|component| matches!(component, Component::RootDir | Component::Normal(_)))
    {
        return None;
    }
    let path = Path::new(value);
    if !path.is_absolute()
        || !path
            .components()
            .all(|component| matches!(component, Component::RootDir | Component::Normal(_)))
    {
        return None;
    }
    let relative = path.strip_prefix(root).ok()?;
    if relative.as_os_str().is_empty() {
        return Some("$PROJECT".to_owned());
    }
    let relative = relative.to_string_lossy();
    let normalized = format!("$PROJECT/{relative}");
    (normalized.len() <= MAX_PATH_BYTES).then_some(normalized)
}

fn is_write_tool(value: &str) -> bool {
    matches!(
        value.to_ascii_lowercase().as_str(),
        "write" | "edit" | "multiedit" | "notebookedit"
    )
}

fn is_agent_spawn_tool(value: &str) -> bool {
    matches!(
        value.to_ascii_lowercase().as_str(),
        "task" | "agent" | "taskcreate"
    )
}

fn ordinal(index: usize, kind: u32) -> Result<u32, DecodeError> {
    u32::try_from(index)
        .ok()
        .and_then(|value| value.checked_mul(4))
        .and_then(|value| value.checked_add(kind))
        .ok_or(DecodeError::OutputTooLarge)
}

fn capture_from_derived(value: &[u8]) -> Result<SecureCapturedString, DecodeError> {
    let length = u64::try_from(value.len()).map_err(|_| DecodeError::OutputTooLarge)?;
    Ok(SecureCapturedString {
        bytes: Zeroizing::new(value.to_vec()),
        total_bytes: length,
        hash: blake3::hash(value).to_hex().to_string(),
        truncated: false,
    })
}

fn is_metadata_type(value: &str) -> bool {
    matches!(
        value,
        "attachment"
            | "file-history-snapshot"
            | "last-prompt"
            | "mode"
            | "permission-mode"
            | "queue-operation"
            | "ai-title"
    )
}

fn is_known_type(value: &str) -> bool {
    matches!(value, "user" | "assistant" | "system" | "result") || is_metadata_type(value)
}
