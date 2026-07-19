#![allow(clippy::missing_errors_doc, clippy::unused_async)]

//! Idempotent, preserving installation of agent MCP configuration and runtime service.

use std::{path::PathBuf, sync::Arc};

use crate::{
    config::{merge_claude_settings, merge_claude_user, merge_codex_config},
    paths::AgboxPaths,
    platform::{Change, Platform, PlatformError, ServiceSpec},
};

const RUNTIME_LABEL: &str = "com.agbox.runtime";

/// Options accepted by the library setup entry point.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct InitOptions {
    pub quiet: bool,
}

/// A non-sensitive summary suitable for a CLI or doctor command.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InitReport {
    pub paths: Change,
    pub claude: Change,
    pub codex: Change,
    pub legacy: Change,
    pub service: Change,
    pub started: Change,
}

/// Orchestrates setup through a narrow, mockable platform boundary.
#[derive(Clone, Debug)]
pub struct Initializer<P> {
    platform: Arc<P>,
}

impl<P> Initializer<P>
where
    P: Platform,
{
    #[must_use]
    pub fn new(platform: P) -> Self {
        Self {
            platform: Arc::new(platform),
        }
    }

    /// Performs preserving configuration writes before touching the service manager.
    pub async fn run(&self, _options: InitOptions) -> Result<InitReport, PlatformError> {
        let paths = self.platform.paths()?;
        let paths_change = self.platform.create_private_paths(&paths)?;
        let executable = self.platform.executable()?;
        let home = self.platform.home()?;

        let claude = self.merge_claude(&home, &executable)?;
        let codex = self.merge_codex(&home, &executable)?;
        let spec = runtime_spec(executable);
        let legacy = self.platform.retire_legacy_watcher()?;
        let service = self.platform.install_service(&spec)?;
        let started = self.platform.start_service(RUNTIME_LABEL)?;

        Ok(InitReport {
            paths: paths_change,
            claude,
            codex,
            legacy,
            service,
            started,
        })
    }

    fn merge_claude(
        &self,
        home: &std::path::Path,
        executable: &std::path::Path,
    ) -> Result<Change, PlatformError> {
        let user = home.join(".claude.json");
        let settings_path = home.join(".claude/settings.json");
        let existing = self.platform.read_file(&user)?;
        let merged = merge_claude_user(existing.as_deref(), executable)?;
        let user_change = self.platform.write_private_file(&user, &merged)?;
        if let Some(settings) =
            merge_claude_settings(self.platform.read_file(&settings_path)?.as_deref())
        {
            let _ = self
                .platform
                .write_private_file(&settings_path, &settings)?;
        }
        Ok(user_change)
    }

    fn merge_codex(
        &self,
        home: &std::path::Path,
        executable: &std::path::Path,
    ) -> Result<Change, PlatformError> {
        let config = home.join(".codex/config.toml");
        let existing = self.platform.read_file(&config)?;
        let merged = merge_codex_config(existing.as_deref(), executable)?;
        self.platform.write_private_file(&config, &merged)
    }

    /// Returns the generated native service specification without exposing mutable state.
    #[must_use]
    pub fn runtime_spec(executable: PathBuf) -> ServiceSpec {
        runtime_spec(executable)
    }
}

fn runtime_spec(executable: PathBuf) -> ServiceSpec {
    ServiceSpec {
        label: RUNTIME_LABEL,
        executable,
        program_arguments: vec!["daemon".into(), "start".into(), "--foreground".into()],
    }
}

#[allow(dead_code)]
fn _paths_for_report(paths: &AgboxPaths) -> &std::path::Path {
    &paths.root
}
