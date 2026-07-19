//! Input mapping without transport or side effects.

use crossterm::event::{KeyCode, KeyEvent};

use super::Message;

/// Maps the approved work-browser key set to bounded state messages.
#[must_use]
pub fn message_for_key(key: KeyEvent, editing: bool) -> Option<Message> {
    if editing {
        return match key.code {
            KeyCode::Esc => Some(Message::Back),
            KeyCode::Enter => Some(Message::SubmitEditor),
            KeyCode::Backspace => Some(Message::EditorBackspace),
            KeyCode::Char(character) => Some(Message::EditorCharacter(character)),
            _ => None,
        };
    }
    match key.code {
        KeyCode::Char('q') => Some(Message::Quit),
        KeyCode::Char('r') => Some(Message::Notice("refreshing daemon view")),
        KeyCode::Char('c') => Some(Message::BeginCorrection),
        KeyCode::Char('1') => Some(Message::SelectStatus(agbox_core::WorkStatus::Active)),
        KeyCode::Char('2') => Some(Message::SelectStatus(agbox_core::WorkStatus::Blocked)),
        KeyCode::Char('3') => Some(Message::SelectStatus(agbox_core::WorkStatus::Completed)),
        KeyCode::Char('0') => Some(Message::ClearStatus),
        KeyCode::Up | KeyCode::Char('k') => Some(Message::MoveSelection(-1)),
        KeyCode::Down | KeyCode::Char('j') => Some(Message::MoveSelection(1)),
        KeyCode::Enter => Some(Message::OpenSelected),
        KeyCode::Esc => Some(Message::Back),
        _ => None,
    }
}
