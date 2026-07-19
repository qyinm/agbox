#![allow(clippy::unwrap_used)]

use std::{path::Path, sync::Arc};

use agbox_core::{
    ContractId, Provider, WorkId, WorkStatus,
    api::{AppRequest, AppResponse, WorkDetail},
};
use agbox_service::{
    AppClient, IpcAppClient, RequestActor, RequestScope, ServiceError,
    ipc::{IPC_PROTOCOL_VERSION, IpcHello, LocalIpcServer, ScopedRequestHandler, WireActor},
};
use async_trait::async_trait;
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

#[derive(Debug, Default)]
struct HandoffHandler {
    actors: Mutex<Vec<RequestActor>>,
}

#[async_trait]
impl ScopedRequestHandler for HandoffHandler {
    async fn dispatch(
        &self,
        scope: RequestScope,
        request: AppRequest,
    ) -> Result<AppResponse, ServiceError> {
        self.actors.lock().await.push(scope.actor());
        if !matches!(request, AppRequest::CurrentWork) {
            return Err(ServiceError::InvalidRequest);
        }
        Ok(AppResponse::Work(Box::new(WorkDetail {
            work_id: WorkId::parse_wire("work_cross_agent").unwrap(),
            contract_id: ContractId::parse_wire("contract_cross_agent").unwrap(),
            revision: 1,
            status: WorkStatus::Active,
            objective: Some("Complete bounded handoff".into()),
            summary: "Shared immutable handoff".into(),
            completed_steps: Vec::new(),
            next_actions: vec!["Verify the result".into()],
            blockers: Vec::new(),
            constraints: Vec::new(),
            completion_criteria: Vec::new(),
            artifacts: Vec::new(),
            verification: Vec::new(),
        })))
    }
}

fn make_project(root: &Path) {
    std::fs::create_dir(root.join(".git")).unwrap();
}

fn hello(root: &Path, provider: Provider) -> IpcHello {
    IpcHello {
        protocol_version: IPC_PROTOCOL_VERSION,
        project_root: root.to_path_buf(),
        actor: WireActor::Agent { provider },
    }
}

#[tokio::test]
async fn claude_and_codex_read_the_same_current_handoff_over_verified_ipc() {
    let directory = tempfile::Builder::new()
        .prefix("a")
        .tempdir_in("/tmp")
        .unwrap();
    make_project(directory.path());
    let socket = directory.path().join("agbox.sock");
    let handler = Arc::new(HandoffHandler::default());
    let server = Arc::new(
        LocalIpcServer::bind(&socket, handler.clone())
            .await
            .unwrap(),
    );
    let cancel = CancellationToken::new();
    let serving = {
        let server = server.clone();
        let cancel = cancel.clone();
        tokio::spawn(async move { server.serve_until(cancel).await })
    };

    let claude = IpcAppClient::connect(&socket, hello(directory.path(), Provider::Claude))
        .await
        .unwrap();
    let codex = IpcAppClient::connect(&socket, hello(directory.path(), Provider::Codex))
        .await
        .unwrap();
    let claude_work = claude.call(AppRequest::CurrentWork).await.unwrap();
    let codex_work = codex.call(AppRequest::CurrentWork).await.unwrap();
    let (AppResponse::Work(claude_work), AppResponse::Work(codex_work)) = (claude_work, codex_work)
    else {
        panic!("both agents receive the scoped handoff");
    };
    assert_eq!(claude_work.work_id, codex_work.work_id);
    assert_eq!(claude_work.revision, codex_work.revision);
    assert_eq!(
        handler.actors.lock().await.as_slice(),
        [
            RequestActor::Agent(Provider::Claude),
            RequestActor::Agent(Provider::Codex)
        ]
    );

    cancel.cancel();
    serving.await.unwrap().unwrap();
    server.remove_socket().unwrap();
}
