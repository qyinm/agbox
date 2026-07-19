use std::sync::Arc;

use agbox_core::{
    WorkId,
    api::{AppRequest, AppResponse, BoundedPage, WorkSummary},
};
use agbox_service::{AppClient, ClientError, GetWorkInput, HandoffMcpServer};
use async_trait::async_trait;
use rmcp::handler::server::wrapper::Parameters;
use tokio::sync::Mutex;

#[derive(Debug)]
struct ScopedFakeClient {
    requests: Mutex<Vec<AppRequest>>,
}

#[async_trait]
impl AppClient for ScopedFakeClient {
    async fn call(&self, request: AppRequest) -> Result<AppResponse, ClientError> {
        self.requests.lock().await.push(request);
        Ok(AppResponse::WorkList(BoundedPage::<WorkSummary> {
            items: Vec::new(),
            truncated: false,
        }))
    }
}

#[tokio::test]
async fn work_lookup_cannot_supply_an_alternate_project_scope() {
    let client = Arc::new(ScopedFakeClient {
        requests: Mutex::new(Vec::new()),
    });
    let server = HandoffMcpServer::new(client.clone());
    let Some(work_id) = WorkId::parse_wire("work_only_identifier") else {
        panic!("fixture work id must be valid");
    };

    let result = server
        .get_work(Parameters(GetWorkInput {
            work_id: work_id.as_str().into(),
        }))
        .await;
    assert!(result.is_ok());

    let requests = client.requests.lock().await;
    assert!(matches!(
        requests.as_slice(),
        [AppRequest::GetWork { work_id: requested }] if requested == &work_id
    ));
}
