#![allow(clippy::unwrap_used)]

use agbox_cli::{InitOptions, Initializer, platform::test_support::FixturePlatform};

#[tokio::test]
async fn repeated_init_is_semantically_idempotent_and_preserves_unknown_settings() {
    let platform = FixturePlatform::from_fixtures(
        "tests/fixtures/claude-user.json",
        "tests/fixtures/claude-settings.json",
        "tests/fixtures/codex-config.toml",
    )
    .unwrap();
    let initializer = Initializer::new(platform.clone());

    initializer.run(InitOptions::default()).await.unwrap();
    let once = platform.snapshot().unwrap();
    initializer.run(InitOptions::default()).await.unwrap();
    let twice = platform.snapshot().unwrap();

    assert_eq!(once, twice);
    assert_eq!(twice.claude["unknownPlugin"]["keep"], true);
    assert_eq!(twice.codex["unrelated"]["keep"], "yes");
    assert_eq!(twice.codex["notify"], "existing-notify --json");
    assert_eq!(twice.launch_agents, vec!["com.agbox.runtime"]);
}
