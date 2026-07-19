#![allow(clippy::unwrap_used)]

use agbox_cli::AgboxPaths;
use agbox_cli::commands::doctor::{DoctorReport, DoctorSeverity};
use std::os::unix::fs::PermissionsExt;

#[test]
fn failures_are_preserved_in_doctor_aggregation() {
    let report = DoctorReport::baseline(false);
    assert!(!report.is_healthy());
    assert!(
        report
            .checks
            .iter()
            .any(|check| check.severity == DoctorSeverity::Failing)
    );
    assert!(
        report
            .checks
            .iter()
            .any(|check| check.code == "network.public_listener")
    );
}

#[test]
fn owner_only_runtime_layout_is_checked_component_by_component() {
    let home = tempfile::tempdir().unwrap();
    let paths = AgboxPaths::from_home(home.path());
    for directory in paths.private_directories() {
        std::fs::create_dir_all(directory).unwrap();
        std::fs::set_permissions(directory, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    std::fs::write(&paths.state_db, b"not a database; metadata-only doctor").unwrap();
    std::fs::set_permissions(&paths.state_db, std::fs::Permissions::from_mode(0o600)).unwrap();

    let report = DoctorReport::inspect(&paths, true);
    assert!(
        report
            .checks
            .iter()
            .any(|check| check.code == "evidence.root_containment"
                && check.severity == DoctorSeverity::Healthy)
    );
    assert!(report.checks.iter().any(
        |check| check.code == "runtime.owner_only" && check.severity == DoctorSeverity::Healthy
    ));
}
