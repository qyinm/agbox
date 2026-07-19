#![allow(clippy::expect_used, clippy::unwrap_used)]

use agbox_core::Provider;
use agbox_ingest::resolve_source_project;

#[test]
fn adapter_hint_must_independently_resolve_to_a_git_project() {
    let directory = tempfile::tempdir().unwrap();
    std::fs::create_dir(directory.path().join(".git")).unwrap();
    let cwd = directory.path().to_string_lossy();
    let record = format!(r#"{{"type":"event_msg","payload":{{"cwd":"{cwd}"}}}}"#);

    let project = resolve_source_project(Provider::Codex, record.as_bytes())
        .unwrap()
        .expect("absolute workspace hint");
    assert_eq!(project.root, directory.path().canonicalize().unwrap());
}

#[test]
fn invalid_or_relative_hint_never_assigns_a_project() {
    assert!(
        resolve_source_project(Provider::Claude, br#"{"cwd":"relative"}"#.as_slice())
            .unwrap()
            .is_none()
    );
}
