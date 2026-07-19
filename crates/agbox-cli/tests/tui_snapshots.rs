use agbox_cli::tui::{App, render::render};
use agbox_core::{ContractId, WorkId, WorkStatus, api::WorkSummary};
use ratatui::{Terminal, backend::TestBackend};

#[test]
fn bounded_work_view_contains_no_evidence_or_paths() {
    let backend = TestBackend::new(80, 24);
    let mut terminal = Terminal::new(backend).unwrap_or_else(|_| panic!("test terminal"));
    terminal
        .draw(|frame| render(frame, &App::fixture()))
        .unwrap_or_else(|_| panic!("draw"));
    let screen = terminal
        .backend()
        .buffer()
        .content()
        .iter()
        .map(ratatui::buffer::Cell::symbol)
        .collect::<String>();
    assert!(screen.contains("agbox"));
    assert!(screen.contains("work_fixture"));
    assert!(!screen.contains("/Users/"));
    assert!(!screen.contains("UNTRUSTED EVIDENCE DATA"));
}

#[test]
fn renderer_redacts_path_like_summary_text() {
    let backend = TestBackend::new(120, 35);
    let mut terminal = Terminal::new(backend).unwrap_or_else(|_| panic!("test terminal"));
    let mut app = App::from_work(vec![WorkSummary {
        work_id: WorkId::parse_wire("work_fixture").unwrap_or_else(|| panic!("work id")),
        contract_id: ContractId::parse_wire("contract_fixture")
            .unwrap_or_else(|| panic!("contract id")),
        revision: 1,
        status: WorkStatus::Active,
        objective: Some("Inspect /Users/alice/private/project".into()),
        summary: "Changed /Users/alice/private/project/src/main.rs".into(),
    }]);
    app.update(agbox_cli::tui::Message::OpenSelected)
        .unwrap_or_else(|_| panic!("open detail"));
    terminal
        .draw(|frame| render(frame, &app))
        .unwrap_or_else(|_| panic!("draw"));
    let screen = terminal
        .backend()
        .buffer()
        .content()
        .iter()
        .map(ratatui::buffer::Cell::symbol)
        .collect::<String>();
    assert!(!screen.contains("/Users/"));
    assert!(screen.contains("<redacted-path>"));
}
