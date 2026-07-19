//! Native setup and command boundary for agbox.

pub mod args;
pub mod commands;
pub mod config;
pub mod init;
pub mod paths;
pub mod platform;
pub mod tui;

pub use init::{InitOptions, InitReport, Initializer};
pub use paths::AgboxPaths;
pub use platform::{Change, Platform, PlatformError, ServiceSpec};

/// Executes the approved CLI surface.
///
/// # Errors
///
pub async fn run(cli: args::Cli) -> Result<(), CliError> {
    match cli.command {
        args::Command::Mcp { provider } => {
            let root = project_root(cli.project_root)?;
            let provider = match provider {
                args::ProviderArg::Claude => agbox_core::Provider::Claude,
                args::ProviderArg::Codex => agbox_core::Provider::Codex,
            };
            let client = commands::client::scoped_client(
                &AgboxPaths::from_home(&user_home()?),
                &root,
                agbox_service::ipc::WireActor::Agent { provider },
            )
            .await?;
            agbox_service::serve_stdio(agbox_service::HandoffMcpServer::new(std::sync::Arc::new(
                client,
            )))
            .await
            .map_err(|_| CliError::Unavailable)
        }
        _ => Err(CliError::Unavailable),
    }
}

/// Bounded public CLI failures.
#[derive(Debug, thiserror::Error)]
pub enum CliError {
    #[error("agbox daemon is unavailable; run `agbox daemon start`")]
    Unavailable,
    #[error("project root is not a safe Git repository")]
    InvalidProject,
    #[error("owner home directory is unavailable")]
    HomeUnavailable,
}

fn project_root(configured: Option<std::path::PathBuf>) -> Result<std::path::PathBuf, CliError> {
    configured
        .map_or_else(std::env::current_dir, Ok)
        .map_err(|_| CliError::InvalidProject)
}

fn user_home() -> Result<std::path::PathBuf, CliError> {
    std::env::var_os("HOME")
        .map(std::path::PathBuf::from)
        .ok_or(CliError::HomeUnavailable)
}

impl CliError {
    #[must_use]
    pub const fn exit_code(&self) -> u8 {
        69
    }
}
