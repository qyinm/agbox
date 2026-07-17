use std::{
    collections::VecDeque,
    fmt,
    io::{self, Write},
    path::{Component, Path},
};

use serde::{Deserialize, Serialize};

use crate::{DecodeError, DecoderState, MAX_DECODER_STATE_BYTES};

const MAX_UNRESOLVED_TOOLS: usize = 128;
const MAX_KNOWN_AGENTS: usize = 128;
const MAX_TOOL_USE_ID_BYTES: usize = 128;
const MAX_EVENT_ID_BYTES: usize = 128;
const MAX_TOOL_NAME_BYTES: usize = 64;
const MAX_HASH_BYTES: usize = 128;
const MAX_PROJECT_PATH_BYTES: usize = 512;
const SAFE_MODES: &[&str] = &["build", "default", "plan"];
const SAFE_PERMISSION_MODES: &[&str] = &[
    "acceptEdits",
    "bypassPermissions",
    "default",
    "delegate",
    "dontAsk",
    "plan",
    "safe",
];

#[derive(Clone, Default, Deserialize, Serialize)]
#[serde(default)]
pub(super) struct ClaudeStateV1 {
    unresolved_tools: VecDeque<ToolLink>,
    known_agents: VecDeque<String>,
    last_human_turn: Option<String>,
    context: ContextSnapshot,
}

impl fmt::Debug for ClaudeStateV1 {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ClaudeStateV1")
            .field("unresolved_tool_count", &self.unresolved_tools.len())
            .field("known_agent_count", &self.known_agents.len())
            .field("has_last_human_turn", &self.last_human_turn.is_some())
            .field("has_context_cwd", &self.context.cwd.is_some())
            .field("has_context_mode", &self.context.mode.is_some())
            .field("has_context_permission", &self.context.permission.is_some())
            .field("has_context_branch", &self.context.branch_hash.is_some())
            .finish()
    }
}

#[derive(Clone, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(default)]
pub(super) struct ContextSnapshot {
    pub cwd: Option<String>,
    pub mode: Option<String>,
    pub permission: Option<String>,
    pub branch_hash: Option<String>,
}

impl fmt::Debug for ContextSnapshot {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ContextSnapshot")
            .field("has_cwd", &self.cwd.is_some())
            .field("has_mode", &self.mode.is_some())
            .field("has_permission", &self.permission.is_some())
            .field("has_branch_hash", &self.branch_hash.is_some())
            .finish()
    }
}

#[derive(Clone, Deserialize, Serialize)]
pub(super) struct ToolLink {
    pub tool_use_id: String,
    pub request_event_id: String,
    pub tool_name: String,
    pub input_hash: String,
    pub project_relative_path: Option<String>,
}

impl fmt::Debug for ToolLink {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ToolLink")
            .field("request_event_id", &self.request_event_id)
            .field("tool_name_bytes", &self.tool_name.len())
            .field("input_hash", &self.input_hash)
            .field(
                "has_project_relative_path",
                &self.project_relative_path.is_some(),
            )
            .finish_non_exhaustive()
    }
}

impl ClaudeStateV1 {
    pub fn decode(state: &DecoderState) -> Result<Self, DecodeError> {
        if state.as_bytes().is_empty() {
            return Ok(Self::default());
        }
        let decoded: Self = serde_json::from_slice(state.as_bytes())
            .map_err(|_| DecodeError::Malformed("invalid-claude-state".to_owned()))?;
        decoded.validate()?;
        Ok(decoded)
    }

    pub fn set_last_human_turn(&mut self, value: String) -> Result<(), DecodeError> {
        if !bounded_identifier(&value, MAX_TOOL_USE_ID_BYTES) {
            return Err(DecodeError::Malformed(
                "invalid-claude-human-turn".to_owned(),
            ));
        }
        self.last_human_turn = Some(value);
        self.fit_serialized_bound()
    }

    pub fn insert_tool(&mut self, link: ToolLink) -> Result<(), DecodeError> {
        link.validate()?;
        self.unresolved_tools
            .retain(|candidate| candidate.tool_use_id != link.tool_use_id);
        self.unresolved_tools.push_back(link);
        while self.unresolved_tools.len() > MAX_UNRESOLVED_TOOLS {
            let _ = self.unresolved_tools.pop_front();
        }
        self.fit_serialized_bound()
    }

    pub fn take_tool(&mut self, tool_use_id: &str) -> Option<ToolLink> {
        let index = self
            .unresolved_tools
            .iter()
            .position(|link| link.tool_use_id == tool_use_id)?;
        self.unresolved_tools.remove(index)
    }

    pub fn observe_agent(&mut self, agent_id: String) -> Result<bool, DecodeError> {
        if !bounded_identifier(&agent_id, MAX_TOOL_USE_ID_BYTES) {
            return Err(DecodeError::Malformed("invalid-claude-agent-id".to_owned()));
        }
        if self.known_agents.iter().any(|known| known == &agent_id) {
            return Ok(false);
        }
        self.known_agents.push_back(agent_id);
        while self.known_agents.len() > MAX_KNOWN_AGENTS {
            let _ = self.known_agents.pop_front();
        }
        self.fit_serialized_bound()?;
        Ok(true)
    }

    pub fn merge_context(
        &mut self,
        cwd: Option<String>,
        mode: Option<String>,
        permission: Option<String>,
        branch_hash: Option<String>,
    ) -> Result<Option<ContextSnapshot>, DecodeError> {
        let mut merged = self.context.clone();
        if let Some(cwd) = cwd {
            merged.cwd = Some(cwd);
        }
        if let Some(mode) = mode {
            merged.mode = Some(mode);
        }
        if let Some(permission) = permission {
            merged.permission = Some(permission);
        }
        if let Some(branch_hash) = branch_hash {
            merged.branch_hash = Some(branch_hash);
        }
        merged.validate()?;
        if merged == self.context {
            return Ok(None);
        }
        self.context = merged.clone();
        self.fit_serialized_bound()?;
        Ok(Some(merged))
    }

    pub fn encode_bounded(mut self) -> Result<DecoderState, DecodeError> {
        self.fit_serialized_bound()?;
        let bytes = serde_json::to_vec(&self)
            .map_err(|_| DecodeError::Malformed("encode-claude-state".to_owned()))?;
        let mut state = DecoderState::default();
        state.replace(bytes)?;
        Ok(state)
    }

    fn validate(&self) -> Result<(), DecodeError> {
        if self.unresolved_tools.len() > MAX_UNRESOLVED_TOOLS {
            return Err(DecodeError::Malformed(
                "invalid-claude-state-count".to_owned(),
            ));
        }
        if self.known_agents.len() > MAX_KNOWN_AGENTS
            || self
                .known_agents
                .iter()
                .any(|agent| !bounded_identifier(agent, MAX_TOOL_USE_ID_BYTES))
        {
            return Err(DecodeError::Malformed(
                "invalid-claude-state-agents".to_owned(),
            ));
        }
        if self
            .last_human_turn
            .as_ref()
            .is_some_and(|value| !bounded_identifier(value, MAX_TOOL_USE_ID_BYTES))
        {
            return Err(DecodeError::Malformed(
                "invalid-claude-state-turn".to_owned(),
            ));
        }
        self.context.validate()?;
        for link in &self.unresolved_tools {
            link.validate()?;
        }
        Ok(())
    }

    fn fit_serialized_bound(&mut self) -> Result<(), DecodeError> {
        while serialized_len(self)? > MAX_DECODER_STATE_BYTES {
            if self.unresolved_tools.pop_front().is_none()
                && self.known_agents.pop_front().is_none()
            {
                self.last_human_turn = None;
                if serialized_len(self)? > MAX_DECODER_STATE_BYTES {
                    return Err(DecodeError::StateTooLarge);
                }
            }
        }
        Ok(())
    }
}

impl ContextSnapshot {
    fn validate(&self) -> Result<(), DecodeError> {
        let valid_cwd = self.cwd.as_ref().is_none_or(|value| {
            value == "$PROJECT"
                || (value.starts_with("$PROJECT/")
                    && value.len() <= MAX_PROJECT_PATH_BYTES
                    && !value.chars().any(char::is_control)
                    && Path::new(value.trim_start_matches("$PROJECT/"))
                        .components()
                        .all(|component| matches!(component, Component::Normal(_))))
        });
        let valid = valid_cwd
            && self
                .mode
                .as_ref()
                .is_none_or(|value| SAFE_MODES.contains(&value.as_str()))
            && self
                .permission
                .as_ref()
                .is_none_or(|value| SAFE_PERMISSION_MODES.contains(&value.as_str()))
            && self
                .branch_hash
                .as_ref()
                .is_none_or(|value| bounded_identifier(value, MAX_HASH_BYTES));
        if valid {
            Ok(())
        } else {
            Err(DecodeError::Malformed(
                "invalid-claude-state-context".to_owned(),
            ))
        }
    }
}

pub(super) fn canonical_context_mode(value: &str) -> Option<String> {
    SAFE_MODES
        .iter()
        .find(|candidate| **candidate == value)
        .map(|candidate| (*candidate).to_owned())
}

pub(super) fn canonical_permission_mode(value: &str) -> Option<String> {
    SAFE_PERMISSION_MODES
        .iter()
        .find(|candidate| **candidate == value)
        .map(|candidate| (*candidate).to_owned())
}

impl ToolLink {
    fn validate(&self) -> Result<(), DecodeError> {
        let valid = bounded_identifier(&self.tool_use_id, MAX_TOOL_USE_ID_BYTES)
            && bounded_identifier(&self.request_event_id, MAX_EVENT_ID_BYTES)
            && bounded_identifier(&self.tool_name, MAX_TOOL_NAME_BYTES)
            && bounded_identifier(&self.input_hash, MAX_HASH_BYTES)
            && self
                .project_relative_path
                .as_ref()
                .is_none_or(|path| valid_project_path(path));
        if valid {
            Ok(())
        } else {
            Err(DecodeError::Malformed(
                "invalid-claude-state-link".to_owned(),
            ))
        }
    }
}

pub(super) fn bounded_identifier(value: &str, max_bytes: usize) -> bool {
    !value.is_empty()
        && value.len() <= max_bytes
        && value.bytes().all(|byte| byte.is_ascii_graphic())
}

fn valid_project_path(value: &str) -> bool {
    let Some(relative) = value.strip_prefix("$PROJECT/") else {
        return false;
    };
    !relative.is_empty()
        && value.len() <= MAX_PROJECT_PATH_BYTES
        && !relative.chars().any(char::is_control)
        && Path::new(relative)
            .components()
            .all(|component| matches!(component, Component::Normal(_)))
}

fn serialized_len<T: Serialize>(value: &T) -> Result<usize, DecodeError> {
    let mut counter = ByteCounter::default();
    serde_json::to_writer(&mut counter, value)
        .map_err(|_| DecodeError::Malformed("measure-claude-state".to_owned()))?;
    Ok(counter.bytes)
}

#[derive(Default)]
struct ByteCounter {
    bytes: usize,
}

impl Write for ByteCounter {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        self.bytes = self
            .bytes
            .checked_add(buffer.len())
            .ok_or_else(|| io::Error::other("state byte count overflow"))?;
        Ok(buffer.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
