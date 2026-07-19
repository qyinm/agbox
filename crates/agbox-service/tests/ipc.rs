#![allow(clippy::unwrap_used)]

use std::{path::Path, sync::Arc};

use agbox_core::api::{AppRequest, AppResponse};
use agbox_service::{
    RequestScope, ServiceError,
    ipc::{
        IPC_PROTOCOL_VERSION, IpcError, IpcHello, IpcRequest, LocalIpcClient, LocalIpcServer,
        PeerVerifier, ScopedRequestHandler, WireActor,
    },
};
use async_trait::async_trait;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

#[derive(Debug)]
struct Accepted;

#[async_trait]
impl ScopedRequestHandler for Accepted {
    async fn dispatch(&self, _: RequestScope, _: AppRequest) -> Result<AppResponse, ServiceError> {
        Ok(AppResponse::Accepted)
    }
}

#[derive(Debug)]
struct DenyPeer;

#[async_trait]
impl PeerVerifier for DenyPeer {
    async fn verify(&self, _: &agbox_service::ipc::Stream) -> Result<(), IpcError> {
        Err(IpcError::PeerDenied)
    }
}

fn project_root(root: &Path) {
    std::fs::create_dir(root.join(".git")).unwrap();
}

fn short_tempdir() -> tempfile::TempDir {
    tempfile::Builder::new()
        .prefix("a")
        .tempdir_in("/tmp")
        .unwrap()
}

fn hello(root: &Path) -> IpcHello {
    IpcHello {
        protocol_version: IPC_PROTOCOL_VERSION,
        project_root: root.to_path_buf(),
        actor: WireActor::HumanCli,
    }
}

#[tokio::test]
async fn same_user_request_is_bound_once_to_a_verified_project() {
    let directory = short_tempdir();
    project_root(directory.path());
    let socket = directory.path().join("agbox.sock");
    let server = Arc::new(
        LocalIpcServer::bind(&socket, Arc::new(Accepted))
            .await
            .unwrap(),
    );
    let cancel = CancellationToken::new();
    let serving = {
        let server = server.clone();
        let cancel = cancel.clone();
        tokio::spawn(async move { server.serve_until(cancel).await })
    };
    let mut client = LocalIpcClient::connect(&socket, hello(directory.path()))
        .await
        .unwrap();
    let response = client
        .request(IpcRequest {
            request_id: Uuid::new_v4(),
            body: AppRequest::Health,
        })
        .await
        .unwrap();
    assert!(matches!(response.body, Ok(AppResponse::Accepted)));
    cancel.cancel();
    serving.await.unwrap().unwrap();
    server.remove_socket().unwrap();
}

#[tokio::test]
async fn rejects_a_peer_owned_by_another_user() {
    let directory = short_tempdir();
    let socket = directory.path().join("agbox.sock");
    let server = Arc::new(
        LocalIpcServer::bind_with_verifier(&socket, Arc::new(Accepted), Arc::new(DenyPeer))
            .await
            .unwrap(),
    );
    let accepting = {
        let server = server.clone();
        tokio::spawn(async move { server.accept_one().await })
    };
    let _client = LocalIpcClient::connect(&socket, hello(directory.path())).await;
    assert!(matches!(
        accepting.await.unwrap(),
        Err(IpcError::PeerDenied)
    ));
    server.remove_socket().unwrap();
}

#[tokio::test]
async fn rejects_frames_larger_than_one_mebibyte() {
    let directory = short_tempdir();
    project_root(directory.path());
    let socket = directory.path().join("agbox.sock");
    let server = Arc::new(
        LocalIpcServer::bind(&socket, Arc::new(Accepted))
            .await
            .unwrap(),
    );
    let cancel = CancellationToken::new();
    let serving = {
        let server = server.clone();
        let cancel = cancel.clone();
        tokio::spawn(async move { server.serve_until(cancel).await })
    };
    let mut client = LocalIpcClient::connect(&socket, hello(directory.path()))
        .await
        .unwrap();
    let error = client.send_raw(vec![b'x'; 1_048_577]).await.unwrap_err();
    assert!(matches!(error, IpcError::FrameTooLarge));
    cancel.cancel();
    serving.await.unwrap().unwrap();
    server.remove_socket().unwrap();
}

#[tokio::test]
async fn active_socket_is_never_reclaimed_as_stale() {
    let directory = short_tempdir();
    let socket = directory.path().join("agbox.sock");
    let first = LocalIpcServer::bind(&socket, Arc::new(Accepted))
        .await
        .unwrap();
    let second = LocalIpcServer::bind(&socket, Arc::new(Accepted)).await;
    assert!(matches!(second, Err(IpcError::AlreadyRunning)));
    first.remove_socket().unwrap();
}
