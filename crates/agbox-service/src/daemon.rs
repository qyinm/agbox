//! Supervised lifecycle for the owner-only local daemon.

#![allow(clippy::missing_errors_doc)]

use std::{fmt, sync::Arc};

use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::ipc::{IpcError, LocalIpcServer};

#[derive(Debug, thiserror::Error)]
pub enum DaemonError {
    #[error("IPC lifecycle failed")]
    Ipc(#[from] IpcError),
    #[error("daemon task failed")]
    TaskFailed,
}

pub struct Daemon {
    cancel: CancellationToken,
    server: Arc<LocalIpcServer>,
    accept_task: JoinHandle<Result<(), IpcError>>,
}

/// Runtime components initialized after singleton socket reservation.
/// Ingestion, watcher, and maintenance supervisors attach here as their
/// concrete runtime handles become available.
#[derive(Debug)]
pub struct Components {
    server: LocalIpcServer,
}

impl Components {
    #[must_use]
    pub fn new(server: LocalIpcServer) -> Self {
        Self { server }
    }
}

impl fmt::Debug for Daemon {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Daemon")
            .field("socket", &self.server.socket_path())
            .finish_non_exhaustive()
    }
}

impl Daemon {
    pub fn run(components: Components) -> Result<Self, DaemonError> {
        let cancel = CancellationToken::new();
        let server = Arc::new(components.server);
        let accept_server = server.clone();
        let accept_cancel = cancel.clone();
        let accept_task =
            tokio::spawn(async move { accept_server.serve_until(accept_cancel).await });
        Ok(Self {
            cancel,
            server,
            accept_task,
        })
    }

    #[must_use]
    pub fn socket_path(&self) -> &std::path::Path {
        self.server.socket_path()
    }

    pub async fn shutdown(self) -> Result<(), DaemonError> {
        self.cancel.cancel();
        self.accept_task
            .await
            .map_err(|_| DaemonError::TaskFailed)??;
        self.server.remove_socket()?;
        Ok(())
    }
}
