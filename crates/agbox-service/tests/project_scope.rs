#![allow(clippy::unwrap_used)]

use agbox_core::{
    EvidenceId, ProjectId, WorkId,
    api::{
        AppRequest, AppResponse, CorrectableField, EvidenceAvailability, EvidenceDisclosure,
        WorkDetail,
    },
};
use agbox_service::{
    ApplicationService, EvidenceReader, RequestActor, StoreWriter, WorkReader,
    app::test_support::scope,
};
use agbox_store::{
    AuditRecord, EvidenceMetadata, ForgetOutcome, ForgetTarget, HumanCorrectionReceipt, StoreError,
};
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
        project: &ProjectId,
        work_id: &WorkId,
    ) -> Result<Option<agbox_core::api::WorkDetail>, StoreError> {
        if project.as_str() == "project_a" && work_id.as_str() == "work_a" {
            Ok(Some(WorkDetail {
                work_id: work_id.clone(),
                contract_id: agbox_core::ContractId::parse_wire("contract_a").unwrap(),
                revision: 1,
                status: agbox_core::WorkStatus::Active,
                objective: Some("objective".into()),
                summary: "summary".into(),
                completed_steps: vec![],
                next_actions: vec![],
                blockers: vec![],
                constraints: vec![],
                completion_criteria: vec![],
                artifacts: vec![],
                verification: vec![],
            }))
        } else {
            Ok(None)
        }
    }
    async fn evidence_owner(
        &self,
        project: &ProjectId,
        evidence_id: &EvidenceId,
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
        } else if project.as_str() == "project_a" && evidence_id.as_str() == "ev_raw" {
            Ok(Some(EvidenceMetadata {
                evidence_id: EvidenceId::for_test("ev_raw"),
                project_id: project.clone(),
                work_id: Some(WorkId::for_test("work_a")),
                event_id: None,
                contract_id: Some(agbox_core::ContractId::parse_wire("contract_a").unwrap()),
                revision: Some(1),
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
        _: &ProjectId,
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
    async fn correct(
        &self,
        _: &ProjectId,
        _: WorkId,
        _: &'static str,
        _: String,
        _: &'static str,
        _: OffsetDateTime,
    ) -> Result<HumanCorrectionReceipt, StoreError> {
        Ok(HumanCorrectionReceipt {
            evidence_id: EvidenceId::for_test("correction"),
            assertion_id: "assertion".into(),
            contract_id: agbox_core::ContractId::parse_wire("contract").unwrap(),
            revision: 2,
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
        Ok(vec![0; 64 * 1024])
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

#[tokio::test]
async fn authorized_human_correction_is_accepted_at_the_service_boundary() {
    let service = ApplicationService::new(Reader, Writer, Vault);
    let response = service
        .handle(
            scope(ProjectId::for_test("project_a"), RequestActor::HumanCli),
            AppRequest::CorrectWork {
                work_id: WorkId::for_test("work_a"),
                field: CorrectableField::Summary,
                value: "corrected".into(),
            },
        )
        .await
        .unwrap();
    assert!(matches!(response, AppResponse::Accepted));
}

#[tokio::test]
async fn agent_correction_is_denied_before_writer_access() {
    let service = ApplicationService::new(Reader, Writer, Vault);
    let error = service
        .handle(
            scope(
                ProjectId::for_test("project_a"),
                RequestActor::Agent(agbox_core::Provider::Codex),
            ),
            AppRequest::CorrectWork {
                work_id: WorkId::for_test("work_a"),
                field: CorrectableField::Summary,
                value: "corrected".into(),
            },
        )
        .await
        .unwrap_err();
    assert!(matches!(
        error,
        agbox_service::ServiceError::OperationDenied
    ));
}

#[tokio::test]
async fn raw_evidence_is_bounded_by_the_complete_wire_response() {
    let service = ApplicationService::new(Reader, Writer, Vault);
    let error = service
        .handle(
            scope(ProjectId::for_test("project_a"), RequestActor::HumanCli),
            AppRequest::GetEvidence {
                evidence_id: EvidenceId::for_test("ev_raw"),
                disclosure: EvidenceDisclosure::AuthorizedRaw,
            },
        )
        .await
        .unwrap_err();
    assert!(matches!(
        error,
        agbox_service::ServiceError::EvidenceUnavailable
    ));
}
