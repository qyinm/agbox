use agbox_core::{
    ContractId, WorkId, WorkStatus,
    api::{CorrectableField, WorkSummary},
};

const MAX_WORK: usize = 100;

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
    OpenSelected,
    Back,
    SubmitCorrection {
        field: CorrectableField,
        value: String,
    },
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
}

impl App {
    #[must_use]
    pub fn fixture() -> Self {
        let work_id = WorkId::parse_wire("work_fixture").unwrap_or_else(|| unreachable!());
        let contract_id =
            ContractId::parse_wire("contract_fixture").unwrap_or_else(|| unreachable!());
        Self {
            status: None,
            work: vec![WorkSummary {
                work_id,
                contract_id,
                revision: 1,
                status: WorkStatus::Active,
                objective: Some("Bound memory".into()),
                summary: "Fixture work".into(),
            }],
            selected: 0,
            focus: Focus::List,
        }
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
    /// Applies one bounded UI message without performing IPC itself.
    ///
    /// # Errors
    ///
    /// Returns an error for a missing selection or an invalid correction.
    pub fn update(&mut self, message: Message) -> Result<Option<Effect>, AppError> {
        match message {
            Message::SelectStatus(status) => {
                self.status = Some(status);
                self.selected = 0;
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
                self.focus = Focus::List;
                Ok(None)
            }
            Message::SubmitCorrection { field, value } => {
                if value.is_empty() || value.len() > 4096 {
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
            Message::Quit => Ok(Some(Effect::Quit)),
        }
    }
}
