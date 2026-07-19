//! Foreground daemon composition.

use std::sync::Arc;

use agbox_core::EvidenceId;
use agbox_service::{
    ApplicationService, Components, Daemon, EvidenceReader, ServiceError, ipc::LocalIpcServer,
};
use agbox_store::StoreRuntime;

use crate::{CliError, paths::AgboxPaths};

#[derive(Debug)]
struct NoRawEvidence;

impl EvidenceReader for NoRawEvidence {
    fn get(
        &self,
        _: &EvidenceId,
        _: &agbox_store::EvidenceMetadata,
    ) -> Result<Vec<u8>, ServiceError> {
        Err(ServiceError::Evidence)
    }
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
    let application =
        ApplicationService::new(store.read().clone(), store.writer().clone(), NoRawEvidence);
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
