//! Deterministic bounded rendering for the work view.

use ratatui::{
    Frame,
    layout::{Constraint, Layout},
    text::{Line, Text},
    widgets::{Block, Borders, Paragraph, Wrap},
};

use super::{App, Focus};

/// Renders only bounded work metadata; evidence and source paths are excluded.
pub fn render(frame: &mut Frame, app: &App) {
    let areas = Layout::vertical([
        Constraint::Length(3),
        Constraint::Min(8),
        Constraint::Length(if app.is_editing() { 4 } else { 3 }),
    ])
    .split(frame.area());
    let status = if app.is_stale() {
        "STALE · reconnecting"
    } else {
        "daemon connected"
    };
    frame.render_widget(
        Paragraph::new(format!(
            "agbox · {status} · 1 active · 2 blocked · 3 completed · r refresh · q quit"
        ))
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title("Project / daemon"),
        ),
        areas[0],
    );
    let body = match app.focus() {
        Focus::List => work_lines(app),
        Focus::Contract => contract_lines(app),
    };
    frame.render_widget(
        Paragraph::new(body)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(match app.focus() {
                        Focus::List => "Work · enter detail · ↑↓ select",
                        Focus::Contract => "Contract · c correct objective · esc back",
                    }),
            )
            .wrap(Wrap { trim: true }),
        areas[1],
    );
    let footer = if let Some(editor) = app.editor_value() {
        format!("Objective correction (enter submit, esc cancel): {editor}")
    } else {
        app.notice()
            .unwrap_or("No untrusted evidence is rendered in the work browser.")
            .to_owned()
    };
    frame.render_widget(
        Paragraph::new(footer)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title("Handoff / privacy"),
            )
            .wrap(Wrap { trim: true }),
        areas[2],
    );
}

fn work_lines(app: &App) -> Text<'static> {
    let lines = app
        .visible_work()
        .into_iter()
        .map(|work| {
            Line::from(format!(
                "{} · {:?} · revision {} · {}",
                work.work_id.as_str(),
                work.status,
                work.revision,
                safe_text(&work.summary)
            ))
        })
        .collect::<Vec<_>>();
    Text::from(lines)
}

fn contract_lines(app: &App) -> Text<'static> {
    let Some(work) = app.selected_contract() else {
        return Text::from("No selected work item");
    };
    Text::from(vec![
        Line::from(format!(
            "Work: {} · revision {} · {:?}",
            work.work_id.as_str(),
            work.revision,
            work.status
        )),
        Line::from(format!(
            "Objective: {}",
            work.objective
                .as_deref()
                .map_or_else(|| "unknown".into(), safe_text)
        )),
        Line::from(format!("Summary: {}", safe_text(&work.summary))),
        Line::from("Corrections create a new human assertion; this cached revision is immutable."),
    ])
}

fn safe_text(value: &str) -> String {
    const MAX_BYTES: usize = 512;
    let mut result = String::with_capacity(value.len().min(MAX_BYTES));
    for token in value.split_whitespace() {
        let rendered = if token.starts_with('/') || token.starts_with("~/") {
            "<redacted-path>"
        } else {
            token
        };
        if !result.is_empty() {
            result.push(' ');
        }
        if result.len().saturating_add(rendered.len()) > MAX_BYTES {
            result.push('…');
            break;
        }
        result.push_str(rendered);
    }
    result
}
