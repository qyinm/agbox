#![allow(clippy::unwrap_used)]

use agbox_cli::{Initializer, platform::test_support::FixturePlatform};

#[test]
fn runtime_service_has_exact_safe_foreground_arguments() {
    let platform = FixturePlatform::from_fixtures(
        "tests/fixtures/claude-user.json",
        "tests/fixtures/claude-settings.json",
        "tests/fixtures/codex-config.toml",
    )
    .unwrap();
    let _initializer = Initializer::new(platform);
    let spec = Initializer::<FixturePlatform>::runtime_spec(
        "/Users/agbox-fixture/.local/bin/agbox".into(),
    );

    assert_eq!(spec.label, "com.agbox.runtime");
    assert_eq!(spec.program_arguments, ["daemon", "start", "--foreground"]);
}
