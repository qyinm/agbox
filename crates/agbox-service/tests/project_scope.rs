#![allow(clippy::unwrap_used)]

use agbox_core::{
    EvidenceId, ProjectId, WorkId,
    api::{AppRequest, AppResponse, EvidenceAvailability, EvidenceDisclosure},
};
use agbox_service::{
    ApplicationService, EvidenceReader, RequestActor, StoreWriter, WorkReader,
    app::test_support::scope,
};
use agbox_store::{AuditRecord, EvidenceMetadata, ForgetOutcome, ForgetTarget, StoreError};
use async_trait::async_trait;
use time::OffsetDateTime;

#[derive(Debug)]
struct Reader;
#[async_trait]
impl WorkReader for Reader {
    async fn list_work(
        &self,
        _: &ProjectId,
        _: Option<agbox_core::WorkStatus>,
        _: u16,
    ) -> Result<Vec<agbox_core::api::WorkSummary>, StoreError> {
        Ok(vec![])
    }
    async fn work(
        &self,
        _: &ProjectId,
        _: &WorkId,
    ) -> Result<Option<agbox_core::api::WorkDetail>, StoreError> {
        Ok(None)
    }
    async fn evidence_owner(
        &self,
        project: &ProjectId,
        _: &EvidenceId,
    ) -> Result<Option<EvidenceMetadata>, StoreError> {
        if project.as_str() == "project_b" {
            Ok(Some(EvidenceMetadata {
                evidence_id: EvidenceId::for_test("ev_b"),
                project_id: project.clone(),
                work_id: None,
                event_id: None,
                contract_id: None,
                revision: None,
                media_type: "text/plain".into(),
                redacted_preview: "preview".into(),
                availability: EvidenceAvailability::Available,
            }))
        } else {
            Ok(None)
        }
    }
    async fn search_work(
        &self,
        _: &ProjectId,
        _: String,
        _: u16,
    ) -> Result<Vec<agbox_core::api::SearchHit>, StoreError> {
        Ok(vec![])
    }
}
#[derive(Debug)]
struct Writer;
#[async_trait]
impl StoreWriter for Writer {
    async fn record_audit(&self, _: AuditRecord) -> Result<(), StoreError> {
        Ok(())
    }
    async fn forget(
        &self,
        _: ForgetTarget,
        _: &'static str,
        _: OffsetDateTime,
    ) -> Result<ForgetOutcome, StoreError> {
        Ok(ForgetOutcome {
            deletion_job_id: "job".into(),
            deleted_rows: 0,
            pending_blobs: 0,
        })
    }
}
#[derive(Debug)]
struct Vault;
impl EvidenceReader for Vault {
    fn get(
        &self,
        _: &EvidenceId,
        _: &EvidenceMetadata,
    ) -> Result<Vec<u8>, agbox_service::ServiceError> {
        Ok(vec![])
    }
}

#[tokio::test]
async fn evidence_cannot_cross_project_scope() {
    let service = ApplicationService::new(Reader, Writer, Vault);
    let response = service
        .handle(
            scope(
                ProjectId::for_test("project_a"),
                RequestActor::Agent(agbox_core::Provider::Codex),
            ),
            AppRequest::GetEvidence {
                evidence_id: EvidenceId::for_test("ev_b"),
                disclosure: EvidenceDisclosure::Redacted,
            },
        )
        .await
        .unwrap();
    assert!(matches!(response, AppResponse::NotFound));
}
