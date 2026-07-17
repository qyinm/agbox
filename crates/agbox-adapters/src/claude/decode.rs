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
    json::{CapturedMatch, CapturedString, CapturedValue},
};

use super::state::{ClaudeStateV1, ToolLink, bounded_identifier};

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

        let semantic = matches!(native_type.as_str(), "user" | "assistant" | "system");
        let session_id = optional_identifier(record, &["sessionId"], MAX_ID_BYTES)?;
        let native_record_id = optional_identifier(record, &["uuid"], MAX_ID_BYTES)?;
        let timestamp = optional_identifier(record, &["timestamp"], MAX_ID_BYTES)?;

        if semantic {
            if session_id.is_none() {
                return Err(DecodeError::MissingIdentity("sessionId"));
            }
            if native_record_id.is_none() {
                return Err(DecodeError::MissingIdentity("uuid"));
            }
            if timestamp.is_none() {
                return Err(DecodeError::MissingIdentity("timestamp"));
            }
        }

        let source_session = session_id.as_deref().unwrap_or("metadata-session");
        let source = make_source(
            record,
            context,
            source_session,
            &native_type,
            native_record_id.clone(),
        )?;
        let observation = make_observation(
            record,
            context,
            source.clone(),
            &schema_fingerprint,
            if is_known_type(&native_type) {
                DecodeStatus::Known
            } else {
                DecodeStatus::UnknownType
            },
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
        session_id,
        uuid,
        occurred_at: OffsetDateTime::parse(&timestamp, &Rfc3339)
            .map_err(|_| DecodeError::Malformed("invalid-timestamp".to_owned()))?,
    };
    decode_activity_record(
        record,
        context,
        prior_state,
        &envelope.native_type,
        &envelope.source,
        envelope.observation,
        &identity,
    )
}

fn decode_activity_record(
    record: &dyn RecordSource,
    context: &DecodeContext,
    prior_state: &DecoderState,
    native_type: &str,
    source: &SourceRef,
    observation: SourceObservation,
    identity: &SemanticIdentity,
) -> Result<DecodedRecord, DecodeError> {
    let mut state = ClaudeStateV1::decode(prior_state)?;
    let mut output = Output::default();
    let scope = EventScope {
        context,
        source,
        identity,
    };
    emit_context_change(record, scope, &mut state, &mut output)?;
    let decoded = match native_type {
        "user" => decode_user(record, context, source, identity, &mut state, &mut output),
        "assistant" => decode_assistant(record, context, source, identity, &mut state, &mut output),
        _ => Ok(()),
    };
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
    occurred_at: OffsetDateTime,
}

#[derive(Clone, Copy)]
struct EventScope<'a> {
    context: &'a DecodeContext,
    source: &'a SourceRef,
    identity: &'a SemanticIdentity,
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
    context: &DecodeContext,
    source: &SourceRef,
    identity: &SemanticIdentity,
    state: &mut ClaudeStateV1,
    output: &mut Output,
) -> Result<(), DecodeError> {
    let mut message_parts = Vec::new();
    if let Some(content) =
        capture_single_string(record, &["message", "content"], MAX_CAPTURE_BYTES, false)?.0
    {
        message_parts.push(content);
    }
    let blocks = collect_blocks(record)?;
    let mut deferred_results = Vec::new();
    for (index, mut block) in blocks {
        match block.kind.as_deref() {
            Some("text") => {
                if let Some(text) = block.text.take() {
                    message_parts.push(text);
                }
            }
            Some("tool_result") => {
                deferred_results.push((index, block));
            }
            _ => {}
        }
    }
    if let Some(content) = combine_text(message_parts)? {
        let scope = EventScope {
            context,
            source,
            identity,
        };
        emit_message(scope, Actor::Human, content, 0, output)?;
        state.set_last_human_turn(identity.uuid.clone())?;
    }
    for (index, block) in deferred_results {
        let scope = EventScope {
            context,
            source,
            identity,
        };
        emit_tool_result(record, scope, state, output, index, block)?;
    }
    Ok(())
}

fn decode_assistant(
    record: &dyn RecordSource,
    context: &DecodeContext,
    source: &SourceRef,
    identity: &SemanticIdentity,
    state: &mut ClaudeStateV1,
    output: &mut Output,
) -> Result<(), DecodeError> {
    let mut message_parts = Vec::new();
    if let Some(content) =
        capture_single_string(record, &["message", "content"], MAX_CAPTURE_BYTES, false)?.0
    {
        message_parts.push(content);
    }
    let blocks = collect_blocks(record)?;
    let mut deferred_tools = Vec::new();
    for (index, mut block) in blocks {
        match block.kind.as_deref() {
            Some("text") => {
                if let Some(text) = block.text.take() {
                    message_parts.push(text);
                }
            }
            Some("tool_use") => {
                deferred_tools.push((index, block));
            }
            _ => {}
        }
    }
    if let Some(content) = combine_text(message_parts)? {
        let scope = EventScope {
            context,
            source,
            identity,
        };
        emit_message(scope, Actor::Agent, content, 0, output)?;
    }
    for (index, block) in deferred_tools {
        let scope = EventScope {
            context,
            source,
            identity,
        };
        emit_tool_request(
            scope,
            state,
            output,
            index,
            block,
            context.project_root.as_deref(),
        )?;
    }
    Ok(())
}

#[derive(Default)]
struct Block {
    kind: Option<String>,
    text: Option<CapturedString>,
    tool_id: Option<String>,
    tool_name: Option<String>,
    raw_input: Option<CapturedString>,
    file_path: Option<String>,
    result_id: Option<String>,
    raw_output: Option<CapturedString>,
    is_error: Option<bool>,
}

fn emit_context_change(
    record: &dyn RecordSource,
    scope: EventScope<'_>,
    state: &mut ClaudeStateV1,
    output: &mut Output,
) -> Result<(), DecodeError> {
    let cwd = optional_bounded_text(record, &["cwd"], MAX_PATH_BYTES)?;
    let normalized_cwd = cwd
        .as_deref()
        .and_then(|value| normalize_context_path(value, scope.context.project_root.as_deref()));
    let mode = optional_bounded_text(record, &["mode"], MAX_TOOL_NAME_BYTES)?;
    let permission_mode =
        optional_bounded_text(record, &["permissionMode"], MAX_TOOL_NAME_BYTES)?.or(
            optional_bounded_text(record, &["permission-mode"], MAX_TOOL_NAME_BYTES)?,
        );
    let branch = capture_single_string(record, &["gitBranch"], 0, true)?.0;
    let branch_hash = branch.map(|value| {
        let mut hasher = blake3::Hasher::new();
        hasher.update(b"agbox-claude-branch-v1");
        hasher.update(&value.total_bytes.to_le_bytes());
        hasher.update(value.hash.as_bytes());
        hasher.finalize().to_hex().to_string()
    });

    let mut fields = Vec::new();
    if let Some(cwd) = normalized_cwd {
        fields.push(format!("cwd={cwd}"));
    }
    if let Some(mode) = mode.filter(|value| bounded_identifier(value, MAX_TOOL_NAME_BYTES)) {
        fields.push(format!("mode={mode}"));
    }
    if let Some(permission) =
        permission_mode.filter(|value| bounded_identifier(value, MAX_TOOL_NAME_BYTES))
    {
        fields.push(format!("permission={permission}"));
    }
    if fields.is_empty() && branch_hash.is_none() {
        return Ok(());
    }
    let context_text = fields.join(";");
    let mut fingerprint = blake3::Hasher::new();
    fingerprint.update(b"agbox-claude-context-v1");
    fingerprint.update(context_text.as_bytes());
    if let Some(branch_hash) = &branch_hash {
        fingerprint.update(branch_hash.as_bytes());
    }
    let context_hash = fingerprint.finalize().to_hex().to_string();
    if !state.update_context(context_hash.clone())? {
        return Ok(());
    }

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

fn combine_text(parts: Vec<CapturedString>) -> Result<Option<CapturedString>, DecodeError> {
    if parts.is_empty() {
        return Ok(None);
    }
    if parts.len() == 1 {
        return Ok(parts.into_iter().next());
    }
    let separators = u64::try_from(parts.len() - 1).map_err(|_| DecodeError::OutputTooLarge)?;
    let total_bytes = parts
        .iter()
        .try_fold(separators, |total, part| {
            total.checked_add(part.total_bytes)
        })
        .ok_or(DecodeError::OutputTooLarge)?;
    let mut bytes = Vec::with_capacity(
        MAX_CAPTURE_BYTES.min(usize::try_from(total_bytes).unwrap_or(MAX_CAPTURE_BYTES)),
    );
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"agbox-claude-message-parts-v1");
    let mut capture_open = true;
    for (index, part) in parts.into_iter().enumerate() {
        if index > 0 && capture_open {
            if bytes.len() < MAX_CAPTURE_BYTES {
                bytes.push(b'\n');
            } else {
                capture_open = false;
            }
        }
        hasher.update(&part.total_bytes.to_le_bytes());
        hasher.update(part.hash.as_bytes());
        if capture_open {
            let remaining = MAX_CAPTURE_BYTES.saturating_sub(bytes.len());
            let mut retained = part.bytes.len().min(remaining);
            while retained > 0
                && std::str::from_utf8(&part.bytes)
                    .is_ok_and(|text| !text.is_char_boundary(retained))
            {
                retained -= 1;
            }
            bytes.extend_from_slice(&part.bytes[..retained]);
            if retained < part.bytes.len() || part.truncated {
                capture_open = false;
            }
        }
    }
    Ok(Some(CapturedString {
        bytes,
        total_bytes,
        hash: format!("seq:b3:{}", hasher.finalize().to_hex()),
        truncated: total_bytes > MAX_CAPTURE_BYTES as u64,
    }))
}

fn collect_blocks(record: &dyn RecordSource) -> Result<BTreeMap<usize, Block>, DecodeError> {
    let mut blocks = BTreeMap::<usize, Block>::new();
    let projection_budget = MAX_RECORD_SEMANTIC_BYTES
        .checked_sub(SEMANTIC_HEADROOM)
        .ok_or(DecodeError::OutputTooLarge)?;
    collect_request_fields(record, &mut blocks, projection_budget)?;
    collect_result_fields(record, &mut blocks, projection_budget)?;
    Ok(blocks)
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
        capture_matches(record, &["message", "content", "text"], MAX_CAPTURE_BYTES)?,
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
            |block, value| {
                if value.truncated || value.total_bytes > MAX_PATH_BYTES as u64 {
                    return Ok(());
                }
                let value = String::from_utf8(value.bytes)
                    .map_err(|_| DecodeError::Malformed("invalid-tool-path".to_owned()))?;
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
    insert_capture_field(
        blocks,
        capture_raw_matches(record, &["message", "content", "content"], remaining)?,
        |block, value| set_once(&mut block.raw_output, value),
    )?;
    ensure_projection_budget(blocks, projection_budget)?;
    insert_bool_field(
        blocks,
        capture_matches(record, &["message", "content", "is_error"], MAX_ID_BYTES)?,
        |block, value| set_once(&mut block.is_error, value),
    )?;
    Ok(())
}

fn projected_bytes(blocks: &BTreeMap<usize, Block>) -> Result<usize, DecodeError> {
    blocks.values().try_fold(0_usize, |total, block| {
        [&block.text, &block.raw_input, &block.raw_output]
            .into_iter()
            .flatten()
            .try_fold(total, |subtotal, capture| {
                subtotal
                    .checked_add(capture.bytes.len())
                    .ok_or(DecodeError::OutputTooLarge)
            })
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
    captured_content: CapturedString,
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

fn emit_tool_request(
    scope: EventScope<'_>,
    state: &mut ClaudeStateV1,
    output: &mut Output,
    index: usize,
    block: Block,
    project_root: Option<&Path>,
) -> Result<(), DecodeError> {
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
    state.insert_tool(ToolLink {
        tool_use_id: tool_id,
        request_event_id: event_id.as_str().to_owned(),
        tool_name,
        input_hash,
        project_relative_path: project_path,
    })
}

fn emit_tool_result(
    record: &dyn RecordSource,
    scope: EventScope<'_>,
    state: &mut ClaudeStateV1,
    output: &mut Output,
    index: usize,
    block: Block,
) -> Result<(), DecodeError> {
    let result_id = block
        .result_id
        .ok_or(DecodeError::MissingIdentity("tool_result.tool_use_id"))?;
    let Some(link) = state.take_tool(&result_id) else {
        return Ok(());
    };
    let top_output = capture_single_raw(record, &["toolUseResult"])?;
    let raw_output = block.raw_output.or(top_output);
    let result_content_hash = raw_output.as_ref().map(|value| value.hash.clone());
    let top_error = optional_bool(record, &["toolUseResult", "isError"])?;
    let failed = block.is_error.or(top_error).unwrap_or(false);
    let outcome = if failed {
        ActionOutcome::Failed
    } else {
        ActionOutcome::Succeeded
    };
    let local_ordinal = ordinal(index, 2)?;
    let source_identity = source_identity(scope.source, scope.context);
    let event_id = EventId::from_source(&source_identity, local_ordinal);
    let output_ref = raw_output
        .map(|value| {
            make_content(
                value,
                DisclosureClass::ToolResult,
                "application/json",
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

    if !failed
        && is_write_tool(&link.tool_name)
        && let Some(path) = link.project_relative_path
    {
        let artifact_ordinal = ordinal(index, 3)?;
        let artifact_event_id = EventId::from_source(&source_identity, artifact_ordinal);
        let path_capture = capture_from_derived(path.as_bytes())?;
        let path_ref = make_content(
            path_capture,
            DisclosureClass::ObservedState,
            "text/uri-list",
            ContentOwner {
                project_root: None,
                event_id: &artifact_event_id,
                source_identity: &source_identity,
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
            Some(event_id.as_str().to_owned()),
            EventPayload::ArtifactChanged {
                path: path_ref,
                operation: link.tool_name,
                content_hash: result_content_hash,
            },
        )?;
        output.push_event(event)?;
    }
    Ok(())
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
        causation_id,
        source: scope.source.clone(),
        payload,
        privacy: PrivacyLabel::SyncEligible,
    })
    .map_err(|_| DecodeError::Malformed("invalid-event".to_owned()))
}

fn make_content(
    mut capture: CapturedString,
    disclosure: DisclosureClass,
    media_type: &'static str,
    owner: ContentOwner<'_>,
    output: &mut Output,
) -> Result<ContentRef, DecodeError> {
    let plaintext = Zeroizing::new(std::mem::take(&mut capture.bytes));
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
        capture.hash,
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
    let mut reader = BoundedJsonReader::new(record.open()?);
    reader.capture_matches(path, limit, MAX_MATCHES)
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
) -> Result<(Option<CapturedString>, String), DecodeError> {
    let mut reader = BoundedJsonReader::new(record.open()?);
    let matches = reader.capture_matches(path, limit, 2)?;
    let schema = reader
        .schema_fingerprint()
        .ok_or_else(|| DecodeError::Malformed("missing-schema".to_owned()))?
        .to_owned();
    if matches.len() > 1 {
        return Err(DecodeError::Malformed(
            "duplicate-selected-field".to_owned(),
        ));
    }
    let value = matches
        .into_iter()
        .next()
        .map(|captured| {
            if require_direct && !captured.array_indices.is_empty() {
                return Err(DecodeError::Malformed(
                    "invalid-identity-location".to_owned(),
                ));
            }
            match captured.value {
                CapturedValue::String(value) => Ok(value),
                CapturedValue::Scalar(_) => {
                    Err(DecodeError::Malformed("selected-non-string".to_owned()))
                }
            }
        })
        .transpose()?;
    Ok((value, schema))
}

fn capture_single_raw(
    record: &dyn RecordSource,
    path: &[&str],
) -> Result<Option<CapturedString>, DecodeError> {
    let mut reader = BoundedJsonReader::new(record.open()?);
    let matches = reader.capture_raw_matches(path, MAX_CAPTURE_BYTES, 2, MAX_CAPTURE_BYTES)?;
    if matches.len() > 1 {
        return Err(DecodeError::Malformed(
            "duplicate-selected-field".to_owned(),
        ));
    }
    matches
        .into_iter()
        .next()
        .map(|captured| match captured.value {
            CapturedValue::String(value) => Ok(value),
            CapturedValue::Scalar(_) => {
                Err(DecodeError::Malformed("invalid-raw-selection".to_owned()))
            }
        })
        .transpose()
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
    let Some(value) = value else {
        return Ok(None);
    };
    if value.truncated || value.total_bytes > limit as u64 {
        return Ok(None);
    }
    String::from_utf8(value.bytes)
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
    value: CapturedString,
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

fn safe_native_type(value: CapturedString) -> String {
    if value.truncated || value.total_bytes > MAX_ID_BYTES as u64 {
        return "invalid-native-type".to_owned();
    }
    String::from_utf8(value.bytes)
        .ok()
        .filter(|value| bounded_identifier(value, MAX_ID_BYTES))
        .unwrap_or_else(|| "invalid-native-type".to_owned())
}

fn captured_text(
    value: CapturedString,
    limit: usize,
    field: &'static str,
) -> Result<String, DecodeError> {
    if value.truncated || value.total_bytes > limit as u64 {
        return Err(DecodeError::Malformed(format!("oversized-{field}")));
    }
    String::from_utf8(value.bytes)
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
    F: FnMut(&mut Block, CapturedString) -> Result<(), DecodeError>,
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
    F: FnMut(&mut Block, CapturedString) -> Result<(), DecodeError>,
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
    let path = Path::new(value);
    let relative = if path.is_absolute() {
        path.strip_prefix(root?).ok()?
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

fn ordinal(index: usize, kind: u32) -> Result<u32, DecodeError> {
    u32::try_from(index)
        .ok()
        .and_then(|value| value.checked_mul(4))
        .and_then(|value| value.checked_add(kind))
        .ok_or(DecodeError::OutputTooLarge)
}

fn capture_from_derived(value: &[u8]) -> Result<CapturedString, DecodeError> {
    let length = u64::try_from(value.len()).map_err(|_| DecodeError::OutputTooLarge)?;
    Ok(CapturedString {
        bytes: value.to_vec(),
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
    matches!(value, "user" | "assistant" | "system") || is_metadata_type(value)
}
