#![allow(clippy::unwrap_used)]

use std::sync::Arc;

use agbox_core::api::{AppRequest, AppResponse};
use agbox_service::{
    Components, Daemon, RequestScope, ServiceError,
    ipc::{LocalIpcServer, ScopedRequestHandler},
};
use async_trait::async_trait;

#[derive(Debug)]
struct Accepted;

#[async_trait]
impl ScopedRequestHandler for Accepted {
    async fn dispatch(&self, _: RequestScope, _: AppRequest) -> Result<AppResponse, ServiceError> {
        Ok(AppResponse::Accepted)
    }
}

#[tokio::test]
async fn daemon_shutdown_removes_its_socket() {
    let directory = tempfile::Builder::new()
        .prefix("a")
        .tempdir_in("/tmp")
        .unwrap();
    let socket = directory.path().join("agbox.sock");
    let server = LocalIpcServer::bind(&socket, Arc::new(Accepted))
        .await
        .unwrap();
    let daemon = Daemon::run(Components::new(server)).unwrap();
    assert!(daemon.socket_path().exists());
    daemon.shutdown().await.unwrap();
    assert!(!socket.exists());
}
