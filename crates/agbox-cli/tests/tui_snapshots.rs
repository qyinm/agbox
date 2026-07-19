use agbox_cli::tui::{App, render::render};
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
