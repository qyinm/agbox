//! Input mapping without transport or side effects.

use crossterm::event::{KeyCode, KeyEvent};

use super::Message;

/// Maps a small approved key set to bounded state messages.
#[must_use]
pub fn message_for_key(key: KeyEvent) -> Option<Message> {
    match key.code {
        KeyCode::Char('q') => Some(Message::Quit),
        KeyCode::Enter => Some(Message::OpenSelected),
        KeyCode::Esc => Some(Message::Back),
        _ => None,
    }
}
