//! Native setup and command boundary for agbox.

pub mod args;
pub mod commands;
pub mod config;
pub mod init;
pub mod paths;
pub mod platform;

pub use init::{InitOptions, InitReport, Initializer};
pub use paths::AgboxPaths;
pub use platform::{Change, Platform, PlatformError, ServiceSpec};

/// Executes the approved CLI surface.
///
/// # Errors
///
/// Returns a bounded recovery error until the daemon-backed command handlers
/// are available.
#[allow(clippy::unused_async)]
pub async fn run(cli: args::Cli) -> Result<(), CliError> {
    let _ = cli;
    Err(CliError::Unavailable)
}

/// Bounded public CLI failures.
#[derive(Debug, thiserror::Error)]
pub enum CliError {
    #[error("agbox daemon is unavailable; run `agbox daemon start`")]
    Unavailable,
}

impl CliError {
    #[must_use]
    pub const fn exit_code(&self) -> u8 {
        69
    }
}
