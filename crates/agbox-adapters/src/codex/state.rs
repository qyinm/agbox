use std::{
    collections::VecDeque,
    fmt,
    io::{self, Write},
};

use serde::{Deserialize, Serialize};

use agbox_core::ActionOutcome;

use crate::{DecodeError, DecoderState, MAX_DECODER_STATE_BYTES};

const MAX_CALLS: usize = 128;
const MAX_COMPLETED_KEYS: usize = 128;
const MAX_CALL_ID_BYTES: usize = 48;
const MAX_EVENT_ID_BYTES: usize = 128;
const MAX_COMPACT_HASH_BYTES: usize = 43;
const MAX_RECORD_HASH_BYTES: usize = 128;
const MAX_OPERATION_BYTES: usize = 8;
const MAX_NATIVE_TYPE_BYTES: usize = 128;
const MAX_SCHEMA_BYTES: usize = 128;

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
    pending_results: VecDeque<PendingResult>,
    completed_semantic_keys: VecDeque<RankedKey>,
    last_ordinal: Option<u64>,
    continuation: Option<Continuation>,
}

impl fmt::Debug for CodexStateV1 {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CodexStateV1")
            .field("history_mode", &self.history_mode)
            .field("unresolved_call_count", &self.unresolved_calls.len())
            .field("pending_result_count", &self.pending_results.len())
            .field("completed_key_count", &self.completed_semantic_keys.len())
            .field("has_last_ordinal", &self.last_ordinal.is_some())
            .field("has_continuation", &self.continuation.is_some())
            .finish()
    }
}

#[derive(Clone, Deserialize, Serialize)]
pub(super) struct PendingResult(
    pub String,
    pub String,
    pub u8,
    pub PendingOutcome,
    pub Option<StagedContent>,
    pub Option<StagedArtifact>,
);

#[derive(Clone, Deserialize, Serialize)]
pub(super) struct StagedContent(pub String, pub u64);

#[derive(Clone, Deserialize, Serialize)]
pub(super) struct StagedArtifact(pub String, pub u64, pub String);

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
pub(super) enum PendingOutcome {
    #[serde(rename = "s")]
    Succeeded,
    #[serde(rename = "f")]
    Failed,
    #[serde(rename = "c")]
    Cancelled,
    #[serde(rename = "u")]
    Unknown,
}

impl fmt::Debug for PendingResult {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PendingResult")
            .field("request_event_id", &self.1)
            .field("rank", &self.2)
            .field("outcome", &self.3)
            .field("has_output", &self.4.is_some())
            .field("has_artifact", &self.5.is_some())
            .finish_non_exhaustive()
    }
}

#[derive(Clone, Deserialize, Serialize)]
pub(super) struct CallLink(pub String, pub String, pub Option<StagedArtifact>);

impl fmt::Debug for CallLink {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CallLink")
            .field("request_event_id", &self.1)
            .field("has_fallback_artifact", &self.2.is_some())
            .finish_non_exhaustive()
    }
}

#[derive(Clone, Deserialize, Serialize)]
struct RankedKey(String, u8);

#[derive(Clone, Deserialize, Serialize)]
pub(super) struct Continuation(
    pub String,
    pub u64,
    pub u64,
    pub String,
    pub Option<u64>,
    pub String,
    pub String,
    pub u32,
    pub String,
);

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
            (HistoryMode::Paginated, Some(previous), Some(current)) => {
                let expected = previous
                    .checked_add(1)
                    .ok_or_else(|| DecodeError::Malformed("codex-ordinal-exhausted".to_owned()))?;
                if current != expected {
                    return Err(DecodeError::Malformed("codex-ordinal-gap".to_owned()));
                }
                self.last_ordinal = Some(current);
                Ok(())
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
            .retain(|candidate| candidate.0 != link.0);
        self.pending_results
            .retain(|candidate| candidate.0 != link.0);
        if self.unresolved_calls.len() + self.pending_results.len() >= MAX_CALLS {
            return Err(DecodeError::StateTooLarge);
        }
        self.unresolved_calls.push_back(link);
        self.fit_serialized_bound()
    }

    pub fn take_call(&mut self, call_id: &str) -> Option<CallLink> {
        let index = self
            .unresolved_calls
            .iter()
            .position(|candidate| candidate.0 == call_id)?;
        self.unresolved_calls.remove(index)
    }

    pub fn call(&self, call_id: &str) -> Option<&CallLink> {
        self.unresolved_calls
            .iter()
            .find(|candidate| candidate.0 == call_id)
    }

    pub fn pending_result(&self, call_id: &str) -> Option<&PendingResult> {
        self.pending_results
            .iter()
            .find(|candidate| candidate.0 == call_id)
    }

    pub fn take_pending_result(&mut self, call_id: &str) -> Option<PendingResult> {
        let index = self
            .pending_results
            .iter()
            .position(|candidate| candidate.0 == call_id)?;
        self.pending_results.remove(index)
    }

    pub fn stage_result(&mut self, candidate: PendingResult) -> Result<(), DecodeError> {
        candidate.validate()?;
        if let Some(existing) = self
            .pending_results
            .iter_mut()
            .find(|existing| existing.0 == candidate.0)
        {
            if candidate.2 > existing.2 {
                *existing = candidate;
            }
            return self.fit_serialized_bound();
        }
        if self.unresolved_calls.len() + self.pending_results.len() >= MAX_CALLS {
            return Err(DecodeError::StateTooLarge);
        }
        self.pending_results.push_back(candidate);
        self.fit_serialized_bound()
    }

    pub fn peek_pending_result(&self) -> Option<&PendingResult> {
        self.pending_results.front()
    }

    pub fn pop_pending_result(&mut self) -> Option<PendingResult> {
        self.pending_results.pop_front()
    }

    pub fn pending_result_count(&self) -> usize {
        self.pending_results.len()
    }

    pub fn completed_rank(&self, key: &str) -> Option<u8> {
        self.completed_semantic_keys
            .iter()
            .find(|candidate| candidate.0 == key)
            .map(|candidate| candidate.1)
    }

    /// Records the strongest observation and returns whether this semantic key
    /// has never emitted a result inside the reconciliation window.
    pub fn observe_result(&mut self, key: String, rank: u8) -> Result<bool, DecodeError> {
        if rank == 0 || !bounded_identifier(&key, MAX_CALL_ID_BYTES) {
            return Err(DecodeError::Malformed(
                "invalid-codex-result-key".to_owned(),
            ));
        }
        if let Some(existing) = self
            .completed_semantic_keys
            .iter_mut()
            .find(|candidate| candidate.0 == key)
        {
            existing.1 = existing.1.max(rank);
            self.fit_serialized_bound()?;
            return Ok(false);
        }
        self.completed_semantic_keys.push_back(RankedKey(key, rank));
        while self.completed_semantic_keys.len() > MAX_COMPLETED_KEYS {
            let _ = self.completed_semantic_keys.pop_front();
        }
        self.fit_serialized_bound()?;
        Ok(true)
    }

    pub fn continuation(&self) -> Option<&Continuation> {
        self.continuation.as_ref()
    }

    pub fn set_continuation(&mut self, continuation: Option<Continuation>) {
        self.continuation = continuation;
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
        if self
            .unresolved_calls
            .len()
            .checked_add(self.pending_results.len())
            .is_none_or(|count| count > MAX_CALLS)
            || self.completed_semantic_keys.len() > MAX_COMPLETED_KEYS
        {
            return Err(DecodeError::Malformed(
                "invalid-codex-state-count".to_owned(),
            ));
        }
        for call in &self.unresolved_calls {
            call.validate()?;
        }
        for pending in &self.pending_results {
            pending.validate()?;
        }
        if self.completed_semantic_keys.iter().any(|candidate| {
            !bounded_identifier(&candidate.0, MAX_CALL_ID_BYTES) || !(1..=3).contains(&candidate.1)
        }) {
            return Err(DecodeError::Malformed(
                "invalid-codex-ranked-key".to_owned(),
            ));
        }
        if let Some(continuation) = &self.continuation {
            continuation.validate()?;
        }
        Ok(())
    }

    fn fit_serialized_bound(&mut self) -> Result<(), DecodeError> {
        while serialized_len(self)? > MAX_DECODER_STATE_BYTES {
            if self.completed_semantic_keys.pop_front().is_some() {
                continue;
            }
            return Err(DecodeError::StateTooLarge);
        }
        Ok(())
    }
}

impl CallLink {
    fn validate(&self) -> Result<(), DecodeError> {
        let valid = bounded_identifier(&self.0, MAX_CALL_ID_BYTES)
            && bounded_identifier(&self.1, MAX_EVENT_ID_BYTES)
            && self.2.as_ref().is_none_or(valid_artifact);
        if valid {
            Ok(())
        } else {
            Err(DecodeError::Malformed("invalid-codex-call-link".to_owned()))
        }
    }
}

impl PendingResult {
    fn validate(&self) -> Result<(), DecodeError> {
        let valid = bounded_identifier(&self.0, MAX_CALL_ID_BYTES)
            && bounded_identifier(&self.1, MAX_EVENT_ID_BYTES)
            && (1..=2).contains(&self.2)
            && self
                .4
                .as_ref()
                .is_none_or(|output| bounded_identifier(&output.0, MAX_COMPACT_HASH_BYTES))
            && self.5.as_ref().is_none_or(valid_artifact);
        if valid {
            Ok(())
        } else {
            Err(DecodeError::Malformed(
                "invalid-codex-pending-result".to_owned(),
            ))
        }
    }
}

fn valid_artifact(artifact: &StagedArtifact) -> bool {
    bounded_identifier(&artifact.0, MAX_COMPACT_HASH_BYTES)
        && bounded_identifier(&artifact.2, MAX_OPERATION_BYTES)
}

impl Continuation {
    fn validate(&self) -> Result<(), DecodeError> {
        let valid = bounded_identifier(&self.0, MAX_RECORD_HASH_BYTES)
            && self.1 <= self.2
            && bounded_identifier(&self.3, MAX_NATIVE_TYPE_BYTES)
            && bounded_identifier(&self.5, MAX_SCHEMA_BYTES)
            && !self.6.is_empty()
            && self.6.len() <= MAX_EVENT_ID_BYTES
            && self.8.len() == MAX_COMPACT_HASH_BYTES
            && self
                .8
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'));
        if valid {
            Ok(())
        } else {
            Err(DecodeError::Malformed(
                "invalid-codex-continuation".to_owned(),
            ))
        }
    }
}

impl From<ActionOutcome> for PendingOutcome {
    fn from(value: ActionOutcome) -> Self {
        match value {
            ActionOutcome::Succeeded => Self::Succeeded,
            ActionOutcome::Failed => Self::Failed,
            ActionOutcome::Cancelled => Self::Cancelled,
            ActionOutcome::Unknown => Self::Unknown,
        }
    }
}

impl From<PendingOutcome> for ActionOutcome {
    fn from(value: PendingOutcome) -> Self {
        match value {
            PendingOutcome::Succeeded => Self::Succeeded,
            PendingOutcome::Failed => Self::Failed,
            PendingOutcome::Cancelled => Self::Cancelled,
            PendingOutcome::Unknown => Self::Unknown,
        }
    }
}

pub(super) fn bounded_identifier(value: &str, max_bytes: usize) -> bool {
    !value.is_empty()
        && value.len() <= max_bytes
        && value.bytes().all(|byte| byte.is_ascii_graphic())
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
