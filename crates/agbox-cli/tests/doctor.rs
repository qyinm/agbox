use agbox_cli::commands::doctor::{DoctorReport, DoctorSeverity};

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
