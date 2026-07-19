//! Non-sensitive health diagnostics.

use std::{os::unix::fs::MetadataExt, path::Path};

use serde::Serialize;

use crate::paths::AgboxPaths;

/// Stable doctor severity that never hides failures behind warnings.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DoctorSeverity {
    Healthy,
    Warning,
    Failing,
}

/// One bounded check result suitable for text or JSON rendering.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct DoctorCheck {
    pub code: &'static str,
    pub severity: DoctorSeverity,
    pub remediation: &'static str,
}

/// Aggregated diagnostic state.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
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

    /// Performs local, metadata-only checks. This never opens evidence, logs,
    /// or source transcripts and therefore cannot disclose their contents.
    #[must_use]
    pub fn inspect(paths: &AgboxPaths, daemon_reachable: bool) -> Self {
        let mut report = Self::baseline(daemon_reachable);
        report.checks[0] = DoctorCheck {
            code: "paths.owner_only",
            severity: private_runtime(paths.root.as_path()),
            remediation: "run agbox init",
        };
        report.checks.push(DoctorCheck {
            code: "state_db.v2",
            severity: regular_owner_file(&paths.state_db),
            remediation: "run agbox daemon start",
        });
        for (code, directory) in [
            ("evidence.owner_only", &paths.evidence),
            ("spool.owner_only", &paths.spool),
            ("logs.owner_only", &paths.logs),
            ("runtime.owner_only", &paths.runtime),
            ("config.owner_only", &paths.config),
        ] {
            report.checks.push(DoctorCheck {
                code,
                severity: private_runtime(directory),
                remediation: "run agbox init",
            });
        }
        report.checks.push(DoctorCheck {
            code: "evidence.root_containment",
            severity: contained_child(&paths.root, &paths.evidence),
            remediation: "remove the unsafe evidence path and run agbox init",
        });
        report.checks.push(DoctorCheck {
            code: "legacy.runtime",
            severity: if paths.root.join("agbox.db").exists() {
                DoctorSeverity::Warning
            } else {
                DoctorSeverity::Healthy
            },
            remediation: "legacy agbox.db is ignored; run agbox init to retire the legacy service",
        });
        report
    }
}

fn contained_child(root: &Path, child: &Path) -> DoctorSeverity {
    let Ok(root) = root.canonicalize() else {
        return DoctorSeverity::Failing;
    };
    let Ok(child) = child.canonicalize() else {
        return DoctorSeverity::Failing;
    };
    if child.starts_with(root) {
        DoctorSeverity::Healthy
    } else {
        DoctorSeverity::Failing
    }
}

fn private_runtime(path: &Path) -> DoctorSeverity {
    std::fs::symlink_metadata(path).map_or(DoctorSeverity::Failing, |metadata| {
        if metadata.file_type().is_symlink()
            || !metadata.is_dir()
            || metadata.uid() != rustix::process::geteuid().as_raw()
            || metadata.mode() & 0o077 != 0
        {
            DoctorSeverity::Failing
        } else {
            DoctorSeverity::Healthy
        }
    })
}

fn regular_owner_file(path: &Path) -> DoctorSeverity {
    std::fs::symlink_metadata(path).map_or(DoctorSeverity::Warning, |metadata| {
        if metadata.file_type().is_symlink()
            || !metadata.is_file()
            || metadata.uid() != rustix::process::geteuid().as_raw()
            || metadata.mode() & 0o077 != 0
        {
            DoctorSeverity::Failing
        } else {
            DoctorSeverity::Healthy
        }
    })
}
