pub mod app;
pub mod event;
pub mod render;
pub mod terminal;

pub use app::{App, AppError, Effect, Focus, Message};

use std::{io, time::Instant};

use crossterm::event::{self as terminal_event, Event};
use ratatui::{Terminal, backend::CrosstermBackend};

use agbox_core::api::{AppRequest, AppResponse, WorkSummary};
use agbox_service::{AppClient, IpcAppClient};

/// Runs the local, IPC-backed work browser. The client has already completed a
/// verified project handshake; this function never opens a database or starts
/// ingestion itself.
///
/// # Errors
///
/// Returns terminal setup, draw, or input errors after best-effort cleanup.
pub async fn run(work: Vec<WorkSummary>, client: IpcAppClient) -> io::Result<()> {
    let mut stdout = io::stdout();
    let _guard = terminal::TerminalGuard::enter(&mut stdout)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;
    let mut app = App::from_work(work);
    loop {
        if let Some(effect) = app.retry_effect(Instant::now()) {
            execute_effect(&mut app, &client, effect).await;
        }
        terminal.draw(|frame| render::render(frame, &app))?;
        if !terminal_event::poll(std::time::Duration::from_millis(250))? {
            continue;
        }
        let Event::Key(key) = terminal_event::read()? else {
            continue;
        };
        let Some(message) = event::message_for_key(key, app.is_editing()) else {
            continue;
        };
        let manual_refresh =
            matches!(key.code, crossterm::event::KeyCode::Char('r')) && !app.is_editing();
        let effect = app.update(message).ok().flatten();
        if manual_refresh {
            execute_effect(&mut app, &client, Effect::Refresh).await;
        }
        if matches!(effect, Some(Effect::Quit)) {
            return Ok(());
        }
        if let Some(effect) = effect {
            execute_effect(&mut app, &client, effect).await;
        }
    }
}

async fn execute_effect(app: &mut App, client: &IpcAppClient, mut effect: Effect) {
    loop {
        match effect {
            Effect::Quit => return,
            Effect::Refresh => {
                if let Ok(AppResponse::WorkList(page)) = client
                    .call(AppRequest::ListWork {
                        status: None,
                        limit: 100,
                    })
                    .await
                {
                    let _ = app.update(Message::ReplaceWork(page.items));
                    let _ = app.update(Message::ConnectionRestored);
                    return;
                }
                let _ = app.update(Message::ConnectionLost);
                return;
            }
            Effect::CorrectWork {
                work_id,
                field,
                value,
            } => match client
                .call(AppRequest::CorrectWork {
                    work_id,
                    field,
                    value,
                })
                .await
            {
                Ok(AppResponse::Accepted) => {
                    let _ = app.update(Message::Notice("correction recorded; refreshing revision"));
                    effect = Effect::Refresh;
                }
                Ok(AppResponse::NotFound) => {
                    let _ = app.update(Message::Notice("work item is no longer available"));
                    return;
                }
                _ => {
                    let _ = app.update(Message::ConnectionLost);
                    return;
                }
            },
        }
    }
}
