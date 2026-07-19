//! Deterministic bounded rendering for the work view.

use ratatui::{
    Frame,
    layout::{Constraint, Layout},
    widgets::{Block, Borders, Paragraph},
};

use super::App;

/// Renders only bounded work metadata; evidence and source paths are excluded.
pub fn render(frame: &mut Frame, app: &App) {
    let areas = Layout::vertical([Constraint::Length(3), Constraint::Min(3)]).split(frame.area());
    frame.render_widget(
        Paragraph::new("agbox · local work handoff · q quit · r refresh · c correct").block(
            Block::default()
                .borders(Borders::ALL)
                .title("Project / daemon"),
        ),
        areas[0],
    );
    let rows = app
        .visible_work()
        .into_iter()
        .map(|work| {
            format!(
                "{} · {:?} · r{} · {}",
                work.work_id.as_str(),
                work.status,
                work.revision,
                work.summary
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    frame.render_widget(
        Paragraph::new(rows).block(Block::default().borders(Borders::ALL).title("Work")),
        areas[1],
    );
}
