#![allow(clippy::unwrap_used)]

use std::time::Duration;

use agbox_release_gate::{
    Thresholds,
    run::{Profile, RunOptions, execute},
};

/// Runs the same bounded corpus, writer, IPC, recovery, and measurement path
/// as the release-gate executable. It is manual because its approved append
/// workload is deliberately one minute long; CI invokes it explicitly.
#[tokio::test]
#[ignore = "manual ci smoke exercises the full one-minute gate"]
async fn ci_smoke_executes_the_production_gate_path() {
    let output = tempfile::tempdir().unwrap();
    let binary = std::env::current_exe().unwrap();
    let artifact = execute(RunOptions {
        profile: Profile::CiSmoke,
        duration: Duration::from_mins(1),
        output_directory: output.path().to_path_buf(),
        commit_sha: "0123456789abcdef0123456789abcdef01234567".into(),
        target: "aarch64-apple-darwin".into(),
        binary,
    })
    .await
    .unwrap();
    assert_eq!(artifact.profile, "ci-smoke");
    assert_eq!(artifact.thresholds, Thresholds::release());
    assert!(
        artifact.evaluation.passed,
        "{:?}",
        artifact.evaluation.failures
    );
    assert!(output.path().join("release-gate-report.json").is_file());
}
