#![allow(clippy::unwrap_used)]
use agbox_cli::tui::{App, Effect, Focus, Message};
use agbox_core::{WorkStatus, api::CorrectableField};
use std::time::{Duration, Instant};
#[test]
fn work_filters_and_detail_navigation_are_deterministic() {
    let mut app = App::fixture();
    app.update(Message::SelectStatus(WorkStatus::Blocked))
        .unwrap();
    assert!(
        app.visible_work()
            .iter()
            .all(|work| work.status == WorkStatus::Blocked)
    );
    app.update(Message::SelectStatus(WorkStatus::Active))
        .unwrap();
    app.update(Message::OpenSelected).unwrap();
    assert_eq!(app.focus(), Focus::Contract);
    assert!(app.selected_contract().is_some());
}
#[test]
fn correction_creates_effect_without_mutating_cached_contract() {
    let mut app = App::fixture();
    let original = app.selected_contract().unwrap().work_id.clone();
    let effect = app
        .update(Message::SubmitCorrection {
            field: CorrectableField::Objective,
            value: "Keep source memory bounded".into(),
        })
        .unwrap();
    assert!(matches!(effect, Some(Effect::CorrectWork { .. })));
    assert_eq!(app.selected_contract().unwrap().work_id, original);
}

#[test]
fn editor_and_reconnect_paths_remain_effect_driven() {
    let mut app = App::fixture();
    app.update(Message::OpenSelected).unwrap();
    app.update(Message::BeginCorrection).unwrap();
    for character in "Keep memory bounded".chars() {
        app.update(Message::EditorCharacter(character)).unwrap();
    }
    let effect = app.update(Message::SubmitEditor).unwrap();
    assert!(matches!(effect, Some(Effect::CorrectWork { .. })));
    app.update(Message::ConnectionLost).unwrap();
    assert!(app.is_stale());
    assert!(
        app.retry_effect(Instant::now() + Duration::from_secs(9))
            .is_some()
    );
    app.update(Message::ConnectionRestored).unwrap();
    assert!(!app.is_stale());
}
