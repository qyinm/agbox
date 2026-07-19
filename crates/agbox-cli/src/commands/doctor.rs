//! Non-sensitive health diagnostics.

/// Stable doctor severity that never hides failures behind warnings.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DoctorSeverity {
    Healthy,
    Warning,
    Failing,
}

/// One bounded check result suitable for text or JSON rendering.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DoctorCheck {
    pub code: &'static str,
    pub severity: DoctorSeverity,
    pub remediation: &'static str,
}

/// Aggregated diagnostic state.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DoctorReport {
    pub checks: Vec<DoctorCheck>,
}

impl DoctorReport {
    /// Returns a conservative local-only baseline without reading evidence or secrets.
    #[must_use]
    pub fn baseline(daemon_reachable: bool) -> Self {
        Self {
            checks: vec![
                DoctorCheck {
                    code: "paths.owner_only",
                    severity: DoctorSeverity::Healthy,
                    remediation: "",
                },
                DoctorCheck {
                    code: "daemon.ipc",
                    severity: if daemon_reachable {
                        DoctorSeverity::Healthy
                    } else {
                        DoctorSeverity::Failing
                    },
                    remediation: "run agbox daemon start",
                },
                DoctorCheck {
                    code: "network.public_listener",
                    severity: DoctorSeverity::Healthy,
                    remediation: "",
                },
            ],
        }
    }
    #[must_use]
    pub fn is_healthy(&self) -> bool {
        self.checks
            .iter()
            .all(|check| check.severity == DoctorSeverity::Healthy)
    }
}
