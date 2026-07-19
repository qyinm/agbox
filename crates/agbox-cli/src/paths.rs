//! Owner-private filesystem layout for the native runtime.

use std::path::{Path, PathBuf};

/// All persistent paths owned by one local agbox installation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AgboxPaths {
    pub root: PathBuf,
    pub state_db: PathBuf,
    pub evidence: PathBuf,
    pub spool: PathBuf,
    pub logs: PathBuf,
    pub runtime: PathBuf,
    pub config: PathBuf,
}

impl AgboxPaths {
    /// Builds the stable layout below a validated user home directory.
    #[must_use]
    pub fn from_home(home: &Path) -> Self {
        let root = home.join(".agbox");
        Self {
            state_db: root.join("state.db"),
            evidence: root.join("evidence"),
            spool: root.join("spool"),
            logs: root.join("logs"),
            runtime: root.join("runtime"),
            config: root.join("config"),
            root,
        }
    }

    /// Returns the directories which must be private before the daemon starts.
    #[must_use]
    pub fn private_directories(&self) -> [&Path; 6] {
        [
            &self.root,
            &self.evidence,
            &self.spool,
            &self.logs,
            &self.runtime,
            &self.config,
        ]
    }
}
