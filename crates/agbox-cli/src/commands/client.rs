//! Verified project-scoped IPC client construction.

use std::path::Path;

use agbox_ingest::ProjectResolver;
use agbox_service::{
    IpcAppClient,
    ipc::{IPC_PROTOCOL_VERSION, IpcHello, WireActor},
};

use crate::{CliError, paths::AgboxPaths};

/// Resolves a Git project before opening an owner-only daemon session.
///
/// # Errors
///
/// Returns a stable CLI error when project validation or IPC setup fails.
pub async fn scoped_client(
    paths: &AgboxPaths,
    root: &Path,
    actor: WireActor,
) -> Result<IpcAppClient, CliError> {
    let resolver = ProjectResolver::new(root).map_err(|_| CliError::InvalidProject)?;
    let project = resolver
        .resolve(root)
        .map_err(|_| CliError::InvalidProject)?;
    IpcAppClient::connect(
        paths.socket(),
        IpcHello {
            protocol_version: IPC_PROTOCOL_VERSION,
            project_root: project.root,
            actor,
        },
    )
    .await
    .map_err(|_| CliError::Unavailable)
}
