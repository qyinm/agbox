#![allow(clippy::missing_errors_doc)]

//! Filesystem and service-manager boundary used by setup.

#[cfg(target_os = "macos")]
pub mod macos;

use std::{
    fmt,
    path::{Path, PathBuf},
};

use crate::paths::AgboxPaths;

/// Semantic result of one idempotent setup operation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Change {
    Changed,
    Unchanged,
    Unsupported,
}

/// A structured definition of the one managed native service.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ServiceSpec {
    pub label: &'static str,
    pub executable: PathBuf,
    pub program_arguments: Vec<String>,
}

/// Safe platform-boundary errors.  They intentionally contain no file contents.
#[derive(Debug, thiserror::Error)]
pub enum PlatformError {
    #[error("the platform path is unsafe")]
    UnsafePath,
    #[error("the platform home directory is unavailable")]
    HomeUnavailable,
    #[error("the agbox executable is unavailable")]
    ExecutableUnavailable,
    #[error("the platform filesystem operation failed")]
    Filesystem,
    #[error("the platform service operation failed")]
    Service,
    #[error("the platform configuration is invalid")]
    InvalidConfiguration,
}

/// Minimal capability boundary for safe, testable native setup.
pub trait Platform: Send + Sync {
    fn home(&self) -> Result<PathBuf, PlatformError>;
    fn paths(&self) -> Result<AgboxPaths, PlatformError>;
    fn executable(&self) -> Result<PathBuf, PlatformError>;
    fn create_private_paths(&self, paths: &AgboxPaths) -> Result<Change, PlatformError>;
    fn read_file(&self, path: &Path) -> Result<Option<Vec<u8>>, PlatformError>;
    fn write_private_file(&self, path: &Path, contents: &[u8]) -> Result<Change, PlatformError>;
    fn install_service(&self, spec: &ServiceSpec) -> Result<Change, PlatformError>;
    fn start_service(&self, label: &str) -> Result<Change, PlatformError>;
    fn stop_service(&self, label: &str) -> Result<Change, PlatformError>;
    fn retire_legacy_watcher(&self) -> Result<Change, PlatformError>;
}

impl fmt::Display for Change {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Changed => formatter.write_str("changed"),
            Self::Unchanged => formatter.write_str("unchanged"),
            Self::Unsupported => formatter.write_str("unsupported"),
        }
    }
}

/// In-memory platform used by setup tests; it never touches a real home or launchd.
#[cfg(feature = "test-support")]
pub mod test_support {
    use std::{
        collections::{BTreeMap, BTreeSet},
        path::{Path, PathBuf},
        sync::{Arc, Mutex, MutexGuard},
    };

    use super::{Change, Platform, PlatformError, ServiceSpec};
    use crate::paths::AgboxPaths;
    use serde_json::{Value, json};

    const HOME: &str = "/Users/agbox-fixture";
    const EXECUTABLE: &str = "/Users/agbox-fixture/.local/bin/agbox";

    #[derive(Debug, Default)]
    struct FixtureState {
        files: BTreeMap<PathBuf, Vec<u8>>,
        paths_initialized: bool,
        launch_agents: BTreeSet<String>,
        started: BTreeSet<String>,
    }

    /// A semantic fixture snapshot used to assert idempotence and preservation.
    #[derive(Clone, Debug, PartialEq)]
    pub struct FixtureSnapshot {
        pub claude: Value,
        pub codex: Value,
        pub launch_agents: Vec<String>,
    }

    /// An in-memory owner platform backed by initial fixture configuration files.
    #[derive(Clone, Debug)]
    pub struct FixturePlatform {
        state: Arc<Mutex<FixtureState>>,
    }

    impl FixturePlatform {
        pub fn from_fixtures(
            claude_user: &str,
            claude_settings: &str,
            codex_config: &str,
        ) -> Result<Self, PlatformError> {
            let home = PathBuf::from(HOME);
            let mut files = BTreeMap::new();
            files.insert(home.join(".claude.json"), fixture(claude_user)?);
            files.insert(
                home.join(".claude/settings.json"),
                fixture(claude_settings)?,
            );
            files.insert(home.join(".codex/config.toml"), fixture(codex_config)?);
            Ok(Self {
                state: Arc::new(Mutex::new(FixtureState {
                    files,
                    ..FixtureState::default()
                })),
            })
        }

        pub fn snapshot(&self) -> Result<FixtureSnapshot, PlatformError> {
            let state = self.lock()?;
            let home = PathBuf::from(HOME);
            let claude = serde_json::from_slice(
                state
                    .files
                    .get(&home.join(".claude.json"))
                    .ok_or(PlatformError::Filesystem)?,
            )
            .map_err(|_| PlatformError::InvalidConfiguration)?;
            let codex_source = state
                .files
                .get(&home.join(".codex/config.toml"))
                .ok_or(PlatformError::Filesystem)?;
            let codex_text = std::str::from_utf8(codex_source)
                .map_err(|_| PlatformError::InvalidConfiguration)?;
            let document = codex_text
                .parse::<toml_edit::DocumentMut>()
                .map_err(|_| PlatformError::InvalidConfiguration)?;
            let unrelated_keep = document["unrelated"]["keep"].as_str().unwrap_or_default();
            let notify = document["notify"].as_str().unwrap_or_default();
            Ok(FixtureSnapshot {
                claude,
                codex: json!({
                    "unrelated": { "keep": unrelated_keep },
                    "notify": notify,
                    "raw": codex_text,
                }),
                launch_agents: state.launch_agents.iter().cloned().collect(),
            })
        }

        fn lock(&self) -> Result<MutexGuard<'_, FixtureState>, PlatformError> {
            self.state.lock().map_err(|_| PlatformError::Filesystem)
        }
    }

    impl Platform for FixturePlatform {
        fn home(&self) -> Result<PathBuf, PlatformError> {
            Ok(PathBuf::from(HOME))
        }

        fn paths(&self) -> Result<AgboxPaths, PlatformError> {
            Ok(AgboxPaths::from_home(Path::new(HOME)))
        }

        fn executable(&self) -> Result<PathBuf, PlatformError> {
            Ok(PathBuf::from(EXECUTABLE))
        }

        fn create_private_paths(&self, _: &AgboxPaths) -> Result<Change, PlatformError> {
            let mut state = self.lock()?;
            if state.paths_initialized {
                Ok(Change::Unchanged)
            } else {
                state.paths_initialized = true;
                Ok(Change::Changed)
            }
        }

        fn read_file(&self, path: &Path) -> Result<Option<Vec<u8>>, PlatformError> {
            Ok(self.lock()?.files.get(path).cloned())
        }

        fn write_private_file(
            &self,
            path: &Path,
            contents: &[u8],
        ) -> Result<Change, PlatformError> {
            let mut state = self.lock()?;
            if state
                .files
                .get(path)
                .is_some_and(|existing| existing == contents)
            {
                return Ok(Change::Unchanged);
            }
            state.files.insert(path.to_path_buf(), contents.to_vec());
            Ok(Change::Changed)
        }

        fn install_service(&self, spec: &ServiceSpec) -> Result<Change, PlatformError> {
            let mut state = self.lock()?;
            if !state.launch_agents.insert(spec.label.into()) {
                return Ok(Change::Unchanged);
            }
            Ok(Change::Changed)
        }

        fn start_service(&self, label: &str) -> Result<Change, PlatformError> {
            let mut state = self.lock()?;
            Ok(if state.started.insert(label.into()) {
                Change::Changed
            } else {
                Change::Unchanged
            })
        }

        fn stop_service(&self, _: &str) -> Result<Change, PlatformError> {
            Ok(Change::Unchanged)
        }

        fn retire_legacy_watcher(&self) -> Result<Change, PlatformError> {
            Ok(Change::Unchanged)
        }
    }

    fn fixture(path: &str) -> Result<Vec<u8>, PlatformError> {
        let direct = PathBuf::from(path);
        let path = if direct.exists() {
            direct
        } else {
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(path)
        };
        std::fs::read(path).map_err(|_| PlatformError::Filesystem)
    }
}
