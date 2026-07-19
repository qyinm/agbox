//! Bounded work-centered terminal state.

mod app;
pub mod event;
pub mod render;
pub mod terminal;

pub use app::{App, AppError, Effect, Focus, Message};

use std::io;

use crossterm::event::{self as terminal_event, Event};
use ratatui::{Terminal, backend::CrosstermBackend};

use agbox_core::api::WorkSummary;

/// Runs the local, read-only work browser. IPC data must be loaded before this
/// function is entered, so terminal mode never owns a daemon or database.
///
/// # Errors
///
/// Returns terminal setup, draw, or input errors after best-effort cleanup.
pub fn run(work: Vec<WorkSummary>) -> io::Result<()> {
    let mut stdout = io::stdout();
    let _guard = terminal::TerminalGuard::enter(&mut stdout)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;
    let mut app = App::from_work(work);
    loop {
        terminal.draw(|frame| render::render(frame, &app))?;
        if !terminal_event::poll(std::time::Duration::from_millis(250))? {
            continue;
        }
        let Event::Key(key) = terminal_event::read()? else {
            continue;
        };
        let Some(message) = event::message_for_key(key) else {
            continue;
        };
        if matches!(app.update(message), Ok(Some(Effect::Quit))) {
            return Ok(());
        }
    }
}
