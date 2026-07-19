#![allow(clippy::unwrap_used)]

use agbox_cli::{AgboxPaths, CliError, commands::client::scoped_client};
use agbox_service::ipc::WireActor;

#[tokio::test]
async fn scoped_cli_refuses_non_projects_before_attempting_ipc() {
    let directory = tempfile::tempdir().unwrap();
    let error = scoped_client(
        &AgboxPaths::from_home(directory.path()),
        directory.path(),
        WireActor::HumanCli,
    )
    .await
    .unwrap_err();
    assert!(matches!(error, CliError::InvalidProject));
}

#[tokio::test]
async fn scoped_cli_never_opens_a_database_when_daemon_is_unavailable() {
    let directory = tempfile::tempdir().unwrap();
    std::fs::create_dir(directory.path().join(".git")).unwrap();
    let paths = AgboxPaths::from_home(directory.path());
    let error = scoped_client(&paths, directory.path(), WireActor::HumanCli)
        .await
        .unwrap_err();
    assert!(matches!(error, CliError::Unavailable));
    assert!(!paths.state_db.exists());
}
