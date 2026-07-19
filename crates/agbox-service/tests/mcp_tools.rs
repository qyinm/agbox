use std::sync::Arc;

use agbox_core::{
    EvidenceId,
    api::{AppRequest, AppResponse, EvidenceAvailability, EvidenceDisclosure, EvidenceView},
};
use agbox_service::{
    AppClient, ClientError, GetEvidenceInput, HandoffMcpServer, ListWorkInput, mcp::GetWorkInput,
};
use async_trait::async_trait;
use rmcp::{handler::server::wrapper::Parameters, model::ContentBlock};
use tokio::sync::Mutex;

#[derive(Debug)]
struct FakeClient {
    response: AppResponse,
    requests: Mutex<Vec<AppRequest>>,
}

#[async_trait]
impl AppClient for FakeClient {
    async fn call(&self, request: AppRequest) -> Result<AppResponse, ClientError> {
        self.requests.lock().await.push(request);
        Ok(self.response.clone())
    }
}

fn text(result: &rmcp::model::CallToolResult) -> &str {
    let Some(ContentBlock::Text(content)) = result.content.first() else {
        panic!("tool result must contain one text block");
    };
    &content.text
}

#[test]
fn exposes_exactly_the_five_read_only_tools() {
    assert_eq!(
        *HandoffMcpServer::tool_names(),
        [
            "get_current_work",
            "get_evidence",
            "get_work",
            "list_work",
            "search_work",
        ]
    );
    assert_eq!(
        HandoffMcpServer::get_current_work_tool_attr().name,
        "get_current_work"
    );
    assert_eq!(
        HandoffMcpServer::get_evidence_tool_attr().name,
        "get_evidence"
    );
    assert_eq!(HandoffMcpServer::get_work_tool_attr().name, "get_work");
    assert_eq!(HandoffMcpServer::list_work_tool_attr().name, "list_work");
    assert_eq!(
        HandoffMcpServer::search_work_tool_attr().name,
        "search_work"
    );
}

#[tokio::test]
async fn evidence_tool_forces_redacted_disclosure_and_never_returns_raw_bytes() {
    let Some(evidence_id) = EvidenceId::parse_wire("evidence_fixture") else {
        panic!("fixture evidence id must be valid");
    };
    let client = Arc::new(FakeClient {
        response: AppResponse::Evidence(EvidenceView {
            evidence_id: evidence_id.clone(),
            media_type: "text/plain".into(),
            untrusted_data: true,
            availability: EvidenceAvailability::Available,
            redacted_preview: "safe preview".into(),
            raw: Some(b"top-secret-raw-evidence".to_vec()),
        }),
        requests: Mutex::new(Vec::new()),
    });
    let server = HandoffMcpServer::new(client.clone());

    let result = server
        .get_evidence(Parameters(GetEvidenceInput {
            evidence_id: evidence_id.as_str().into(),
        }))
        .await
        .unwrap_or_else(|_| panic!("valid evidence query must succeed"));

    let output = text(&result);
    assert!(output.starts_with("UNTRUSTED EVIDENCE DATA."));
    assert!(!output.contains("top-secret-raw-evidence"));
    let requests = client.requests.lock().await;
    assert!(matches!(
        requests.as_slice(),
        [AppRequest::GetEvidence {
            disclosure: EvidenceDisclosure::Redacted,
            ..
        }]
    ));
}

#[tokio::test]
async fn missing_work_is_a_stable_not_found_protocol_error() {
    let client = Arc::new(FakeClient {
        response: AppResponse::NotFound,
        requests: Mutex::new(Vec::new()),
    });
    let server = HandoffMcpServer::new(client);

    let error = server
        .get_work(Parameters(GetWorkInput {
            work_id: "work_cross_project".into(),
        }))
        .await
        .err()
        .unwrap_or_else(|| panic!("missing work must return a protocol error"));

    assert_eq!(error.code, rmcp::model::ErrorCode::RESOURCE_NOT_FOUND);
    assert_eq!(error.message, "agbox item not found");
    assert!(error.data.is_none());
}

#[tokio::test]
async fn list_limit_is_rejected_above_the_public_bound_without_a_daemon_call() {
    let client = Arc::new(FakeClient {
        response: AppResponse::NotFound,
        requests: Mutex::new(Vec::new()),
    });
    let server = HandoffMcpServer::new(client.clone());

    let error = server
        .list_work(Parameters(ListWorkInput {
            status: None,
            limit: Some(101),
        }))
        .await
        .err()
        .unwrap_or_else(|| panic!("an oversized page request must be rejected"));

    assert_eq!(error.code, rmcp::model::ErrorCode::INVALID_PARAMS);
    assert!(client.requests.lock().await.is_empty());
}

#[tokio::test]
async fn oversized_redacted_evidence_is_never_emitted_to_mcp() {
    let Some(evidence_id) = EvidenceId::parse_wire("evidence_large_fixture") else {
        panic!("fixture evidence id must be valid");
    };
    let client = Arc::new(FakeClient {
        response: AppResponse::Evidence(EvidenceView {
            evidence_id: evidence_id.clone(),
            media_type: "text/plain".into(),
            untrusted_data: true,
            availability: EvidenceAvailability::Available,
            redacted_preview: "x".repeat(64 * 1024),
            raw: None,
        }),
        requests: Mutex::new(Vec::new()),
    });
    let server = HandoffMcpServer::new(client);

    let error = server
        .get_evidence(Parameters(GetEvidenceInput {
            evidence_id: evidence_id.as_str().into(),
        }))
        .await
        .err()
        .unwrap_or_else(|| panic!("oversized evidence must be rejected"));

    assert_eq!(error.code, rmcp::model::ErrorCode::INTERNAL_ERROR);
}
