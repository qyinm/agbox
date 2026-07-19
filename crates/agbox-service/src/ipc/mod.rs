//! Bounded, owner-only local application protocol.

#![allow(clippy::missing_errors_doc)]

#[cfg(unix)]
mod unix;

use std::{
    collections::HashSet,
    fmt,
    path::{Path, PathBuf},
    sync::Arc,
};

use agbox_core::{
    Provider,
    api::{AppRequest, AppResponse},
    limits::MAX_IPC_FRAME_BYTES,
};
use agbox_ingest::ProjectResolver;
use async_trait::async_trait;
use bytes::Bytes;
use futures::{SinkExt, StreamExt};
use interprocess::local_socket::traits::StreamCommon;
use serde::{Deserialize, Serialize};
use tokio::{
    sync::{OwnedSemaphorePermit, RwLock, Semaphore},
    task::JoinSet,
};
use tokio_util::{
    codec::{Framed, LengthDelimitedCodec},
    sync::CancellationToken,
};
use uuid::Uuid;

use crate::{ApplicationService, RequestActor, RequestScope, ServiceError};

#[cfg(unix)]
pub use interprocess::local_socket::tokio::Stream;

pub const IPC_PROTOCOL_VERSION: u16 = 1;
pub const MAX_IPC_CONNECTIONS: usize = 16;
pub const MAX_IN_FLIGHT_PER_CONNECTION: usize = 4;
pub const MAX_IN_FLIGHT_GLOBAL: usize = 32;
const MAX_REQUESTS_PER_CONNECTION: usize = 1_024;
const MAX_PROJECT_ROOT_BYTES: usize = 4_096;

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct IpcHello {
    pub protocol_version: u16,
    pub project_root: PathBuf,
    pub actor: WireActor,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WireActor {
    HumanCli,
    HumanTui,
    Agent { provider: Provider },
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct IpcRequest {
    pub request_id: Uuid,
    pub body: AppRequest,
}

#[derive(Clone, Deserialize, Serialize)]
pub struct IpcResponse {
    pub request_id: Uuid,
    pub body: Result<AppResponse, PublicServiceError>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub struct PublicServiceError {
    pub code: PublicErrorCode,
    pub message: PublicMessage,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PublicErrorCode {
    Busy,
    InvalidRequest,
    Denied,
    Unavailable,
    Internal,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PublicMessage {
    DaemonBusy,
    RequestInvalid,
    OperationDenied,
    EvidenceUnavailable,
    ServiceUnavailable,
}

impl fmt::Debug for IpcHello {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("IpcHello")
            .field("protocol_version", &self.protocol_version)
            .field("project_root_bytes", &self.project_root.as_os_str().len())
            .field("actor", &self.actor)
            .finish()
    }
}

impl fmt::Debug for IpcRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("IpcRequest")
            .field("request_id", &self.request_id)
            .field("body", &self.body)
            .finish()
    }
}

impl fmt::Debug for IpcResponse {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("IpcResponse")
            .field("request_id", &self.request_id)
            .field(
                "response_class",
                &self.body.as_ref().map_or("error", |_| "ok"),
            )
            .finish()
    }
}

#[derive(Debug, thiserror::Error)]
pub enum IpcError {
    #[error("IPC frame exceeds the bounded wire limit")]
    FrameTooLarge,
    #[error("IPC peer is not owned by the daemon user")]
    PeerDenied,
    #[error("another daemon is already running")]
    AlreadyRunning,
    #[error("IPC socket path is unsafe")]
    UnsafeSocketPath,
    #[error("IPC bind failed")]
    BindFailed,
    #[error("IPC accept failed")]
    AcceptFailed,
    #[error("IPC connection is busy")]
    Busy,
    #[error("IPC hello is invalid")]
    InvalidHello,
    #[error("IPC request is invalid")]
    InvalidRequest,
    #[error("IPC project is unavailable")]
    ProjectUnavailable,
    #[error("IPC transport failed")]
    Transport,
}

#[async_trait]
pub trait ScopedRequestHandler: Send + Sync {
    async fn dispatch(
        &self,
        scope: RequestScope,
        request: AppRequest,
    ) -> Result<AppResponse, ServiceError>;
}

/// Keeps the singleton socket reserved while the daemon initializes its
/// credential-backed store. Requests are never routed until the real scoped
/// handler has been installed.
#[derive(Default)]
pub struct DeferredRequestHandler {
    active: RwLock<Option<Arc<dyn ScopedRequestHandler>>>,
}

impl fmt::Debug for DeferredRequestHandler {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DeferredRequestHandler")
            .finish_non_exhaustive()
    }
}

impl DeferredRequestHandler {
    /// Atomically makes the verified application handler available to IPC.
    pub async fn activate(&self, handler: Arc<dyn ScopedRequestHandler>) {
        *self.active.write().await = Some(handler);
    }
}

#[async_trait]
impl ScopedRequestHandler for DeferredRequestHandler {
    async fn dispatch(
        &self,
        scope: RequestScope,
        request: AppRequest,
    ) -> Result<AppResponse, ServiceError> {
        let handler = self.active.read().await.clone();
        let handler = handler.ok_or(ServiceError::Unavailable)?;
        handler.dispatch(scope, request).await
    }
}

#[async_trait]
impl<R, W, V> ScopedRequestHandler for ApplicationService<R, W, V>
where
    R: crate::WorkReader + Send + Sync,
    W: crate::StoreWriter + Send + Sync,
    V: crate::EvidenceReader + Send + Sync,
{
    async fn dispatch(
        &self,
        scope: RequestScope,
        request: AppRequest,
    ) -> Result<AppResponse, ServiceError> {
        self.handle(scope, request).await
    }
}

#[async_trait]
pub trait PeerVerifier: Send + Sync {
    async fn verify(&self, stream: &Stream) -> Result<(), IpcError>;
}

#[derive(Debug)]
pub struct SameUserPeerVerifier {
    daemon_euid: u32,
}

impl SameUserPeerVerifier {
    #[must_use]
    pub fn current_user() -> Self {
        Self {
            daemon_euid: rustix::process::geteuid().as_raw(),
        }
    }
}

#[async_trait]
impl PeerVerifier for SameUserPeerVerifier {
    async fn verify(&self, stream: &Stream) -> Result<(), IpcError> {
        let peer = stream.peer_creds().map_err(|_| IpcError::PeerDenied)?;
        if peer.euid() == Some(self.daemon_euid) {
            Ok(())
        } else {
            Err(IpcError::PeerDenied)
        }
    }
}

pub struct LocalIpcServer {
    #[cfg(unix)]
    listener: unix::BoundUnixListener,
    handler: Arc<dyn ScopedRequestHandler>,
    verifier: Arc<dyn PeerVerifier>,
    connection_gate: Arc<Semaphore>,
    request_gate: Arc<Semaphore>,
}

impl fmt::Debug for LocalIpcServer {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("LocalIpcServer")
            .field("socket", &self.socket_path())
            .finish_non_exhaustive()
    }
}

impl LocalIpcServer {
    pub async fn bind(
        socket: impl AsRef<Path>,
        handler: Arc<dyn ScopedRequestHandler>,
    ) -> Result<Self, IpcError> {
        Self::bind_with_verifier(
            socket,
            handler,
            Arc::new(SameUserPeerVerifier::current_user()),
        )
        .await
    }

    pub async fn bind_with_verifier(
        socket: impl AsRef<Path>,
        handler: Arc<dyn ScopedRequestHandler>,
        verifier: Arc<dyn PeerVerifier>,
    ) -> Result<Self, IpcError> {
        #[cfg(unix)]
        let listener = unix::BoundUnixListener::bind(socket.as_ref()).await?;
        Ok(Self {
            #[cfg(unix)]
            listener,
            handler,
            verifier,
            connection_gate: Arc::new(Semaphore::new(MAX_IPC_CONNECTIONS)),
            request_gate: Arc::new(Semaphore::new(MAX_IN_FLIGHT_GLOBAL)),
        })
    }

    #[must_use]
    pub fn socket_path(&self) -> &Path {
        #[cfg(unix)]
        return self.listener.path();
        #[allow(unreachable_code)]
        Path::new("")
    }

    pub async fn accept_one(&self) -> Result<AcceptedConnection, IpcError> {
        #[cfg(unix)]
        let stream = self.listener.accept().await?;
        self.verifier.verify(&stream).await?;
        let permit = self
            .connection_gate
            .clone()
            .try_acquire_owned()
            .map_err(|_| IpcError::Busy)?;
        Ok(AcceptedConnection {
            stream,
            handler: self.handler.clone(),
            request_gate: self.request_gate.clone(),
            _connection_permit: permit,
        })
    }

    pub async fn serve_until(&self, cancel: CancellationToken) -> Result<(), IpcError> {
        let mut tasks = JoinSet::new();
        loop {
            tokio::select! {
                () = cancel.cancelled() => break,
                accepted = self.accept_one() => match accepted {
                    Ok(connection) => {
                        let connection_cancel = cancel.clone();
                        tasks.spawn(async move { connection.serve_until(connection_cancel).await });
                    }
                    Err(IpcError::Busy | IpcError::PeerDenied) => {}
                    Err(error) => return Err(error),
                },
                joined = tasks.join_next(), if !tasks.is_empty() => {
                    match joined {
                        Some(Ok(Ok(()) | Err(IpcError::InvalidRequest | IpcError::FrameTooLarge | IpcError::Transport))) | None => {}
                        Some(Ok(Err(error))) => return Err(error),
                        Some(Err(_)) => return Err(IpcError::Transport),
                    }
                }
            }
        }
        while let Some(joined) = tasks.join_next().await {
            joined.map_err(|_| IpcError::Transport)??;
        }
        Ok(())
    }

    pub fn remove_socket(&self) -> Result<(), IpcError> {
        #[cfg(unix)]
        return self.listener.remove();
        #[allow(unreachable_code)]
        Ok(())
    }
}

pub struct AcceptedConnection {
    stream: Stream,
    handler: Arc<dyn ScopedRequestHandler>,
    request_gate: Arc<Semaphore>,
    _connection_permit: OwnedSemaphorePermit,
}

impl fmt::Debug for AcceptedConnection {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AcceptedConnection")
            .finish_non_exhaustive()
    }
}

impl AcceptedConnection {
    pub async fn serve(self) -> Result<(), IpcError> {
        self.serve_until(CancellationToken::new()).await
    }

    async fn serve_until(self, cancel: CancellationToken) -> Result<(), IpcError> {
        let mut frames = framed(self.stream);
        let hello = tokio::select! {
            () = cancel.cancelled() => return Ok(()),
            hello = read_json::<IpcHello>(&mut frames) => hello?,
        };
        let scope = scope_from_hello(&hello)?;
        let mut seen = HashSet::with_capacity(MAX_REQUESTS_PER_CONNECTION);
        for _ in 0..MAX_REQUESTS_PER_CONNECTION {
            let request = tokio::select! {
                () = cancel.cancelled() => return Ok(()),
                request = read_optional_json::<IpcRequest>(&mut frames) => request?,
            };
            let Some(request) = request else {
                return Ok(());
            };
            if !seen.insert(request.request_id) {
                return Err(IpcError::InvalidRequest);
            }
            let response = match self.request_gate.clone().try_acquire_owned() {
                Ok(_permit) => {
                    let body = tokio::select! {
                        () = cancel.cancelled() => return Ok(()),
                        result = self.handler.dispatch(scope.clone(), request.body) => result.map_err(|error| public_error(&error)),
                    };
                    IpcResponse {
                        request_id: request.request_id,
                        body,
                    }
                }
                Err(_) => IpcResponse {
                    request_id: request.request_id,
                    body: Err(PublicServiceError {
                        code: PublicErrorCode::Busy,
                        message: PublicMessage::DaemonBusy,
                    }),
                },
            };
            tokio::select! {
                () = cancel.cancelled() => return Ok(()),
                result = write_json(&mut frames, &response) => result?,
            }
        }
        Err(IpcError::Busy)
    }
}

pub struct LocalIpcClient {
    frames: Framed<Stream, LengthDelimitedCodec>,
}

impl fmt::Debug for LocalIpcClient {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("LocalIpcClient")
            .finish_non_exhaustive()
    }
}

impl LocalIpcClient {
    pub async fn connect(socket: impl AsRef<Path>, hello: IpcHello) -> Result<Self, IpcError> {
        let stream = unix::connect(socket.as_ref()).await?;
        let mut client = Self {
            frames: framed(stream),
        };
        client.send(&hello).await?;
        Ok(client)
    }

    pub async fn request(&mut self, request: IpcRequest) -> Result<IpcResponse, IpcError> {
        self.send(&request).await?;
        read_json(&mut self.frames).await
    }

    pub async fn send_raw(&mut self, payload: Vec<u8>) -> Result<(), IpcError> {
        if payload.len() > MAX_IPC_FRAME_BYTES {
            return Err(IpcError::FrameTooLarge);
        }
        self.frames
            .send(Bytes::from(payload))
            .await
            .map_err(map_transport_error)
    }

    async fn send<T: Serialize>(&mut self, value: &T) -> Result<(), IpcError> {
        write_json(&mut self.frames, value).await
    }
}

pub fn framed<T>(stream: T) -> Framed<T, LengthDelimitedCodec>
where
    T: tokio::io::AsyncRead + tokio::io::AsyncWrite,
{
    LengthDelimitedCodec::builder()
        .max_frame_length(MAX_IPC_FRAME_BYTES)
        .new_framed(stream)
}

fn scope_from_hello(hello: &IpcHello) -> Result<RequestScope, IpcError> {
    if hello.protocol_version != IPC_PROTOCOL_VERSION
        || hello.project_root.as_os_str().len() > MAX_PROJECT_ROOT_BYTES
    {
        return Err(IpcError::InvalidHello);
    }
    let resolver =
        ProjectResolver::new(&hello.project_root).map_err(|_| IpcError::ProjectUnavailable)?;
    let project = resolver
        .resolve(&hello.project_root)
        .map_err(|_| IpcError::ProjectUnavailable)?;
    let actor = match hello.actor {
        WireActor::HumanCli => RequestActor::HumanCli,
        WireActor::HumanTui => RequestActor::HumanTui,
        WireActor::Agent { provider } => RequestActor::Agent(provider),
    };
    Ok(RequestScope::verified(project.project_id, actor))
}

async fn read_json<T>(frames: &mut Framed<Stream, LengthDelimitedCodec>) -> Result<T, IpcError>
where
    T: for<'de> Deserialize<'de>,
{
    read_optional_json(frames).await?.ok_or(IpcError::Transport)
}

async fn read_optional_json<T>(
    frames: &mut Framed<Stream, LengthDelimitedCodec>,
) -> Result<Option<T>, IpcError>
where
    T: for<'de> Deserialize<'de>,
{
    match frames.next().await {
        Some(Ok(bytes)) => serde_json::from_slice(&bytes)
            .map(Some)
            .map_err(|_| IpcError::InvalidRequest),
        Some(Err(error)) => Err(map_transport_error(error)),
        None => Ok(None),
    }
}

async fn write_json<T>(
    frames: &mut Framed<Stream, LengthDelimitedCodec>,
    value: &T,
) -> Result<(), IpcError>
where
    T: Serialize,
{
    let encoded = serde_json::to_vec(value).map_err(|_| IpcError::InvalidRequest)?;
    if encoded.len() > MAX_IPC_FRAME_BYTES {
        return Err(IpcError::FrameTooLarge);
    }
    frames
        .send(Bytes::from(encoded))
        .await
        .map_err(map_transport_error)
}

fn map_transport_error(error: impl std::fmt::Display) -> IpcError {
    if error.to_string().contains("frame size too big") {
        IpcError::FrameTooLarge
    } else {
        IpcError::Transport
    }
}

fn public_error(error: &ServiceError) -> PublicServiceError {
    match error {
        ServiceError::InvalidRequest => PublicServiceError {
            code: PublicErrorCode::InvalidRequest,
            message: PublicMessage::RequestInvalid,
        },
        ServiceError::DisclosureDenied | ServiceError::OperationDenied => PublicServiceError {
            code: PublicErrorCode::Denied,
            message: PublicMessage::OperationDenied,
        },
        ServiceError::EvidenceUnavailable | ServiceError::Evidence => PublicServiceError {
            code: PublicErrorCode::Unavailable,
            message: PublicMessage::EvidenceUnavailable,
        },
        ServiceError::Unavailable => PublicServiceError {
            code: PublicErrorCode::Unavailable,
            message: PublicMessage::ServiceUnavailable,
        },
        ServiceError::Store(_) => PublicServiceError {
            code: PublicErrorCode::Internal,
            message: PublicMessage::ServiceUnavailable,
        },
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use agbox_core::ProjectId;

    use super::{AppRequest, AppResponse, DeferredRequestHandler, ScopedRequestHandler};
    use crate::{RequestActor, RequestScope, ServiceError};
    use async_trait::async_trait;

    #[derive(Debug)]
    struct Ready;

    #[async_trait]
    impl ScopedRequestHandler for Ready {
        async fn dispatch(
            &self,
            _: RequestScope,
            _: AppRequest,
        ) -> Result<AppResponse, ServiceError> {
            Ok(AppResponse::Accepted)
        }
    }

    #[tokio::test]
    async fn deferred_handler_reserves_ipc_without_dispatching_before_readiness() {
        let handler = DeferredRequestHandler::default();
        let Some(project_id) = ProjectId::parse_wire("project_deferred") else {
            panic!("valid fixture project id");
        };
        let scope = RequestScope::verified(project_id, RequestActor::HumanCli);
        assert!(matches!(
            handler.dispatch(scope.clone(), AppRequest::Health).await,
            Err(ServiceError::Unavailable)
        ));
        handler.activate(Arc::new(Ready)).await;
        assert!(matches!(
            handler.dispatch(scope, AppRequest::Health).await,
            Ok(AppResponse::Accepted)
        ));
    }
}
