use std::time::{Duration, Instant};

use agbox_core::{
    ContractId, WorkId, WorkStatus,
    api::{CorrectableField, WorkSummary},
};

const MAX_WORK: usize = 100;
const MAX_EDITOR_BYTES: usize = 4_096;
const MAX_NOTICE_BYTES: usize = 240;
const INITIAL_RETRY: Duration = Duration::from_millis(500);
const MAX_RETRY: Duration = Duration::from_secs(8);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Focus {
    List,
    Contract,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Effect {
    Refresh,
    CorrectWork {
        work_id: WorkId,
        field: CorrectableField,
        value: String,
    },
    Quit,
}

#[derive(Clone, Debug)]
pub enum Message {
    SelectStatus(WorkStatus),
    ClearStatus,
    MoveSelection(i8),
    OpenSelected,
    Back,
    BeginCorrection,
    EditorCharacter(char),
    EditorBackspace,
    SubmitEditor,
    SubmitCorrection {
        field: CorrectableField,
        value: String,
    },
    ReplaceWork(Vec<WorkSummary>),
    ConnectionLost,
    ConnectionRestored,
    Notice(&'static str),
    Quit,
}

#[derive(Debug, thiserror::Error)]
pub enum AppError {
    #[error("no selected work item")]
    NoSelection,
    #[error("correction exceeds its bound")]
    CorrectionTooLarge,
}

#[derive(Debug)]
pub struct App {
    status: Option<WorkStatus>,
    work: Vec<WorkSummary>,
    selected: usize,
    focus: Focus,
    editor: Option<String>,
    stale: bool,
    retries: u8,
    retry_after: Option<Instant>,
    notice: Option<String>,
}

impl App {
    /// Creates a bounded screen model from already-scoped immutable summaries.
    #[must_use]
    pub fn from_work(mut work: Vec<WorkSummary>) -> Self {
        work.truncate(MAX_WORK);
        Self {
            status: None,
            work,
            selected: 0,
            focus: Focus::List,
            editor: None,
            stale: false,
            retries: 0,
            retry_after: None,
            notice: None,
        }
    }

    #[must_use]
    pub fn fixture() -> Self {
        let work_id = WorkId::parse_wire("work_fixture").unwrap_or_else(|| unreachable!());
        let contract_id =
            ContractId::parse_wire("contract_fixture").unwrap_or_else(|| unreachable!());
        Self::from_work(vec![WorkSummary {
            work_id,
            contract_id,
            revision: 1,
            status: WorkStatus::Active,
            objective: Some("Bound memory".into()),
            summary: "Fixture work".into(),
        }])
    }

    #[must_use]
    pub fn focus(&self) -> Focus {
        self.focus
    }

    #[must_use]
    pub fn visible_work(&self) -> Vec<&WorkSummary> {
        self.work
            .iter()
            .filter(|work| self.status.is_none_or(|status| status == work.status))
            .take(MAX_WORK)
            .collect()
    }

    #[must_use]
    pub fn selected_contract(&self) -> Option<&WorkSummary> {
        self.visible_work().get(self.selected).copied()
    }

    #[must_use]
    pub const fn is_editing(&self) -> bool {
        self.editor.is_some()
    }

    #[must_use]
    pub const fn is_stale(&self) -> bool {
        self.stale
    }

    #[must_use]
    pub fn notice(&self) -> Option<&str> {
        self.notice.as_deref()
    }

    #[must_use]
    pub fn editor_value(&self) -> Option<&str> {
        self.editor.as_deref()
    }

    /// Emits one coalesced retry only after the current bounded backoff delay.
    pub fn retry_effect(&mut self, now: Instant) -> Option<Effect> {
        if !self.stale || self.retry_after.is_some_and(|deadline| deadline > now) {
            return None;
        }
        self.retry_after = Some(now + retry_delay(self.retries));
        Some(Effect::Refresh)
    }

    /// Applies one bounded UI message without performing IPC itself.
    ///
    /// # Errors
    ///
    /// Returns an error for a missing selection or an invalid correction.
    #[allow(clippy::too_many_lines)]
    pub fn update(&mut self, message: Message) -> Result<Option<Effect>, AppError> {
        match message {
            Message::SelectStatus(status) => {
                self.status = Some(status);
                self.selected = 0;
                Ok(None)
            }
            Message::ClearStatus => {
                self.status = None;
                self.selected = 0;
                Ok(None)
            }
            Message::MoveSelection(delta) => {
                let len = self.visible_work().len();
                if len != 0 {
                    let current = isize::try_from(self.selected).unwrap_or(0);
                    let last = isize::try_from(len.saturating_sub(1)).unwrap_or(0);
                    self.selected =
                        usize::try_from((current + isize::from(delta)).clamp(0, last)).unwrap_or(0);
                }
                Ok(None)
            }
            Message::OpenSelected => {
                if self.selected_contract().is_none() {
                    return Err(AppError::NoSelection);
                }
                self.focus = Focus::Contract;
                Ok(None)
            }
            Message::Back => {
                self.editor = None;
                self.focus = Focus::List;
                Ok(None)
            }
            Message::BeginCorrection => {
                if self.focus != Focus::Contract || self.selected_contract().is_none() {
                    return Err(AppError::NoSelection);
                }
                self.editor = Some(String::new());
                Ok(None)
            }
            Message::EditorCharacter(character) => {
                if let Some(editor) = &mut self.editor
                    && editor.len().saturating_add(character.len_utf8()) <= MAX_EDITOR_BYTES
                {
                    editor.push(character);
                }
                Ok(None)
            }
            Message::EditorBackspace => {
                if let Some(editor) = &mut self.editor {
                    let _ = editor.pop();
                }
                Ok(None)
            }
            Message::SubmitEditor => {
                let value = self.editor.take().ok_or(AppError::NoSelection)?;
                self.update(Message::SubmitCorrection {
                    field: CorrectableField::Objective,
                    value,
                })
            }
            Message::SubmitCorrection { field, value } => {
                if value.is_empty() || value.len() > MAX_EDITOR_BYTES {
                    return Err(AppError::CorrectionTooLarge);
                }
                let work_id = self
                    .selected_contract()
                    .ok_or(AppError::NoSelection)?
                    .work_id
                    .clone();
                Ok(Some(Effect::CorrectWork {
                    work_id,
                    field,
                    value,
                }))
            }
            Message::ReplaceWork(mut work) => {
                work.truncate(MAX_WORK);
                self.work = work;
                self.selected = self
                    .selected
                    .min(self.visible_work().len().saturating_sub(1));
                Ok(None)
            }
            Message::ConnectionLost => {
                self.stale = true;
                self.retries = self.retries.saturating_add(1);
                self.retry_after = Some(Instant::now() + retry_delay(self.retries));
                self.notice = Some("daemon unavailable; retaining the last verified view".into());
                Ok(None)
            }
            Message::ConnectionRestored => {
                self.stale = false;
                self.retries = 0;
                self.retry_after = None;
                self.notice = None;
                Ok(None)
            }
            Message::Notice(message) => {
                self.notice = Some(bound(message, MAX_NOTICE_BYTES));
                Ok(None)
            }
            Message::Quit => Ok(Some(Effect::Quit)),
        }
    }
}

fn retry_delay(retries: u8) -> Duration {
    let multiplier = 1_u32 << u32::from(retries.min(4));
    INITIAL_RETRY.saturating_mul(multiplier).min(MAX_RETRY)
}

fn bound(value: &str, limit: usize) -> String {
    if value.len() <= limit {
        return value.to_owned();
    }
    let mut end = limit;
    while !value.is_char_boundary(end) {
        end = end.saturating_sub(1);
    }
    format!("{}…", &value[..end])
}
