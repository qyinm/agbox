use agbox_adapters::project_hint;
use agbox_core::Provider;

#[test]
fn bounded_provider_hints_are_not_project_authority() {
    let claude = project_hint(Provider::Claude, br#"{"cwd":"/safe/project"}"#)
        .unwrap_or_else(|_| panic!("valid fixture"));
    assert_eq!(
        claude.as_ref().map(agbox_adapters::ProjectHint::as_path),
        Some(std::path::Path::new("/safe/project"))
    );
    let codex = project_hint(Provider::Codex, br#"{"payload":{"cwd":"/safe/project"}}"#)
        .unwrap_or_else(|_| panic!("valid fixture"));
    assert_eq!(
        codex.as_ref().map(agbox_adapters::ProjectHint::as_path),
        Some(std::path::Path::new("/safe/project"))
    );
    assert!(
        project_hint(Provider::Claude, br#"{"cwd":"relative"}"#)
            .unwrap_or_else(|_| panic!("valid fixture"))
            .is_none()
    );
}
