use std::{
    collections::VecDeque,
    fmt,
    io::{self, Write},
    path::{Component, Path},
};

use serde::{Deserialize, Serialize};

use crate::{DecodeError, DecoderState, MAX_DECODER_STATE_BYTES};

const MAX_CALLS: usize = 128;
const MAX_COMPLETED_KEYS: usize = 128;
const MAX_CALL_ID_BYTES: usize = 128;
const MAX_EVENT_ID_BYTES: usize = 128;
const MAX_TOOL_NAME_BYTES: usize = 64;
const MAX_HASH_BYTES: usize = 128;
const MAX_PROJECT_PATH_BYTES: usize = 512;
const MAX_RANKED_KEY_BYTES: usize = 128;

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HistoryMode {
    Legacy,
    Paginated,
    #[default]
    Unknown,
}

#[derive(Clone, Default, Deserialize, Serialize)]
#[serde(default)]
pub(super) struct CodexStateV1 {
    history_mode: HistoryMode,
    unresolved_calls: VecDeque<CallLink>,
    completed_semantic_keys: VecDeque<RankedKey>,
    last_ordinal: Option<u64>,
}

impl fmt::Debug for CodexStateV1 {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CodexStateV1")
            .field("history_mode", &self.history_mode)
            .field("unresolved_call_count", &self.unresolved_calls.len())
            .field("completed_key_count", &self.completed_semantic_keys.len())
            .field("has_last_ordinal", &self.last_ordinal.is_some())
            .finish()
    }
}

#[derive(Clone, Deserialize, Serialize)]
pub(super) struct CallLink {
    #[serde(alias = "call_id", rename = "c")]
    pub call_id: String,
    #[serde(alias = "request_event_id", rename = "e")]
    pub request_event_id: String,
    #[serde(alias = "tool_name", rename = "t")]
    pub tool_name: String,
    #[serde(alias = "input_hash", rename = "h")]
    pub input_hash: String,
    #[serde(
        alias = "project_relative_path",
        default,
        rename = "p",
        skip_serializing_if = "Option::is_none"
    )]
    pub project_relative_path: Option<String>,
}

impl fmt::Debug for CallLink {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CallLink")
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

#[derive(Clone, Deserialize, Serialize)]
struct RankedKey {
    key: String,
    rank: u8,
}

impl CodexStateV1 {
    pub fn decode(state: &DecoderState) -> Result<Self, DecodeError> {
        if state.as_bytes().is_empty() {
            return Ok(Self::default());
        }
        let decoded: Self = serde_json::from_slice(state.as_bytes())
            .map_err(|_| DecodeError::Malformed("invalid-codex-state".to_owned()))?;
        decoded.validate()?;
        Ok(decoded)
    }

    pub const fn history_mode(&self) -> HistoryMode {
        self.history_mode
    }

    pub fn observe_history_mode(&mut self, observed: HistoryMode) {
        self.history_mode = match (self.history_mode, observed) {
            (HistoryMode::Paginated, _) | (_, HistoryMode::Unknown) => self.history_mode,
            (_, HistoryMode::Paginated) => HistoryMode::Paginated,
            (HistoryMode::Unknown | HistoryMode::Legacy, HistoryMode::Legacy) => {
                HistoryMode::Legacy
            }
        };
    }

    pub fn observe_ordinal(&mut self, ordinal: Option<u64>) -> Result<(), DecodeError> {
        match (self.history_mode, self.last_ordinal, ordinal) {
            (HistoryMode::Paginated, Some(previous), Some(current))
                if current != previous.saturating_add(1) =>
            {
                Err(DecodeError::Malformed("codex-ordinal-gap".to_owned()))
            }
            (HistoryMode::Paginated, _, None) => {
                Err(DecodeError::Malformed("codex-missing-ordinal".to_owned()))
            }
            (HistoryMode::Paginated, _, Some(current)) => {
                self.last_ordinal = Some(current);
                Ok(())
            }
            (_, _, _) => Ok(()),
        }
    }

    pub fn insert_call(&mut self, link: CallLink) -> Result<(), DecodeError> {
        link.validate()?;
        self.unresolved_calls
            .retain(|candidate| candidate.call_id != link.call_id);
        self.unresolved_calls.push_back(link);
        while self.unresolved_calls.len() > MAX_CALLS {
            let _ = self.unresolved_calls.pop_front();
        }
        self.fit_serialized_bound()
    }

    pub fn take_call(&mut self, call_id: &str) -> Option<CallLink> {
        let index = self
            .unresolved_calls
            .iter()
            .position(|candidate| candidate.call_id == call_id)?;
        self.unresolved_calls.remove(index)
    }

    pub fn call(&self, call_id: &str) -> Option<&CallLink> {
        self.unresolved_calls
            .iter()
            .find(|candidate| candidate.call_id == call_id)
    }

    pub fn completed_rank(&self, key: &str) -> Option<u8> {
        self.completed_semantic_keys
            .iter()
            .find(|candidate| candidate.key == key)
            .map(|candidate| candidate.rank)
    }

    /// Records the strongest observation and returns whether this semantic key
    /// has never emitted a result inside the reconciliation window.
    pub fn observe_result(&mut self, key: String, rank: u8) -> Result<bool, DecodeError> {
        if rank == 0 || !bounded_identifier(&key, MAX_RANKED_KEY_BYTES) {
            return Err(DecodeError::Malformed(
                "invalid-codex-result-key".to_owned(),
            ));
        }
        if let Some(existing) = self
            .completed_semantic_keys
            .iter_mut()
            .find(|candidate| candidate.key == key)
        {
            existing.rank = existing.rank.max(rank);
            self.fit_serialized_bound()?;
            return Ok(false);
        }
        self.completed_semantic_keys
            .push_back(RankedKey { key, rank });
        while self.completed_semantic_keys.len() > MAX_COMPLETED_KEYS {
            let _ = self.completed_semantic_keys.pop_front();
        }
        self.fit_serialized_bound()?;
        Ok(true)
    }

    pub fn encode_bounded(mut self) -> Result<DecoderState, DecodeError> {
        self.fit_serialized_bound()?;
        let bytes = serde_json::to_vec(&self)
            .map_err(|_| DecodeError::Malformed("encode-codex-state".to_owned()))?;
        let mut state = DecoderState::default();
        state.replace(bytes)?;
        Ok(state)
    }

    fn validate(&self) -> Result<(), DecodeError> {
        if self.unresolved_calls.len() > MAX_CALLS
            || self.completed_semantic_keys.len() > MAX_COMPLETED_KEYS
        {
            return Err(DecodeError::Malformed(
                "invalid-codex-state-count".to_owned(),
            ));
        }
        for call in &self.unresolved_calls {
            call.validate()?;
        }
        if self.completed_semantic_keys.iter().any(|candidate| {
            !bounded_identifier(&candidate.key, MAX_RANKED_KEY_BYTES)
                || !(1..=3).contains(&candidate.rank)
        }) {
            return Err(DecodeError::Malformed(
                "invalid-codex-ranked-key".to_owned(),
            ));
        }
        Ok(())
    }

    fn fit_serialized_bound(&mut self) -> Result<(), DecodeError> {
        while serialized_len(self)? > MAX_DECODER_STATE_BYTES {
            if self.completed_semantic_keys.pop_front().is_some() {
                continue;
            }
            if let Some(link) = self
                .unresolved_calls
                .iter_mut()
                .find(|link| link.project_relative_path.is_some())
            {
                link.project_relative_path = None;
                continue;
            }
            if self.unresolved_calls.pop_front().is_none() {
                return Err(DecodeError::StateTooLarge);
            }
        }
        Ok(())
    }
}

impl CallLink {
    fn validate(&self) -> Result<(), DecodeError> {
        let valid = bounded_identifier(&self.call_id, MAX_CALL_ID_BYTES)
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
            Err(DecodeError::Malformed("invalid-codex-call-link".to_owned()))
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
        .map_err(|_| DecodeError::Malformed("measure-codex-state".to_owned()))?;
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
