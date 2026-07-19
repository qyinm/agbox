//! Foreground daemon composition.

use std::sync::Arc;

use agbox_service::{ApplicationService, Components, Daemon, ipc::LocalIpcServer};
use agbox_store::{EvidenceVault, KeyringKeyProvider, StoreRuntime};

use crate::{CliError, args::DaemonCommand, paths::AgboxPaths, platform::Platform};

/// Executes bounded daemon lifecycle commands. Only foreground mode owns the
/// writer; normal lifecycle operations delegate to the native service manager.
///
/// # Errors
///
/// Returns a stable CLI error when the native service manager or bounded log
/// reader cannot complete the requested operation.
pub async fn run(command: DaemonCommand, paths: &AgboxPaths) -> Result<(), CliError> {
    match command {
        DaemonCommand::Start { foreground: true } => foreground(paths).await,
        DaemonCommand::Start { foreground: false } => service_change(true),
        DaemonCommand::Stop => service_change(false),
        DaemonCommand::Logs { follow } => logs(paths, follow),
    }
}

fn service_change(start: bool) -> Result<(), CliError> {
    let executable = std::env::current_exe().map_err(|_| CliError::Unavailable)?;
    let platform = crate::platform::macos::MacOsPlatform::for_current_user(executable)
        .map_err(|_| CliError::Unavailable)?;
    let result = if start {
        platform.start_service("com.agbox.runtime")
    } else {
        platform.stop_service("com.agbox.runtime")
    };
    result.map(|_| ()).map_err(|_| CliError::Unavailable)
}

fn logs(paths: &AgboxPaths, follow: bool) -> Result<(), CliError> {
    if follow {
        return Err(CliError::Unavailable);
    }
    let path = paths.logs.join("agbox.log");
    let metadata = std::fs::symlink_metadata(&path).map_err(|_| CliError::Unavailable)?;
    if metadata.file_type().is_symlink() || !metadata.is_file() || metadata.len() > 1_048_576 {
        return Err(CliError::Unavailable);
    }
    let contents = std::fs::read_to_string(path).map_err(|_| CliError::Unavailable)?;
    let entries = contents.lines().collect::<Vec<_>>();
    let start = entries.len().saturating_sub(200);
    for entry in entries.into_iter().skip(start) {
        if entry.len() > agbox_service::logging::MAX_LOG_ENTRY_BYTES {
            return Err(CliError::Unavailable);
        }
        println!("{entry}");
    }
    Ok(())
}

/// Runs the single-writer foreground service until an interrupt signal arrives.
///
/// # Errors
///
/// Returns a stable CLI error when the store, IPC daemon, signal, or shutdown
/// lifecycle cannot complete.
pub async fn foreground(paths: &AgboxPaths) -> Result<(), CliError> {
    let store = StoreRuntime::start(&paths.state_db)
        .await
        .map_err(|_| CliError::Unavailable)?;
    let vault = EvidenceVault::open(
        paths.evidence.clone(),
        std::sync::Arc::new(KeyringKeyProvider),
    )
    .map_err(|_| CliError::Unavailable)?;
    let application = ApplicationService::new(store.read().clone(), store.writer().clone(), vault);
    let server = LocalIpcServer::bind(paths.socket(), Arc::new(application))
        .await
        .map_err(|_| CliError::Unavailable)?;
    let daemon = Daemon::run(Components::new(server)).map_err(|_| CliError::Unavailable)?;
    tokio::signal::ctrl_c()
        .await
        .map_err(|_| CliError::Unavailable)?;
    daemon.shutdown().await.map_err(|_| CliError::Unavailable)?;
    store.shutdown().await.map_err(|_| CliError::Unavailable)
}
