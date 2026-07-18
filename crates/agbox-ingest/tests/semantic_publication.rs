#![allow(clippy::unwrap_used)]

use agbox_core::{
    ContractId, DisclosureClass, EvidenceId, ProjectId, RedactionPolicy, WorkContractRevision,
    WorkContractRevisionDraft, WorkId, WorkStatus,
};
use agbox_ingest::{IngestError, IngestionCoordinator, test_support::FixtureRuntime};
use agbox_store::{ExtractorWriteBatch, WorkContractRow, WorkWriteBatch};
use agbox_workgraph::{DisabledExtractor, ExtractionInput, SemanticPolicy};
use time::{OffsetDateTime, macros::datetime};

fn id<T>(value: &str) -> T
where
    T: FromId,
{
    T::from_id(value)
}

trait FromId {
    fn from_id(value: &str) -> Self;
}

fn extraction_input_contract(project_id: ProjectId, work_id: WorkId) -> WorkContractRevision {
    let redaction = RedactionPolicy::new().unwrap();
    WorkContractRevision::new(WorkContractRevisionDraft {
        contract_id: id("contract-input"),
        work_id,
        revision: 1,
        project_id,
        objective: None,
        status: WorkStatus::Active,
        summary: redaction
            .redact("safe semantic input", None, DisclosureClass::DerivedText)
            .unwrap(),
        completed_steps: Vec::new(),
        next_actions: Vec::new(),
        blockers: Vec::new(),
        constraints: Vec::new(),
        completion_criteria: Vec::new(),
        artifacts: Vec::new(),
        verification: Vec::new(),
        source_runs: Vec::new(),
        evidence_refs: vec![EvidenceId::parse_wire("input-evidence").unwrap()],
        confidence_basis_points: 9_000,
        created_at: datetime!(2026-07-19 13:00 UTC),
        extractor_version: "deterministic-v1".into(),
        disclosure_class: DisclosureClass::DerivedText,
    })
    .unwrap()
}

impl FromId for WorkId {
    fn from_id(value: &str) -> Self {
        WorkId::parse_wire(value).unwrap()
    }
}

impl FromId for ContractId {
    fn from_id(value: &str) -> Self {
        ContractId::parse_wire(value).unwrap()
    }
}

#[allow(clippy::too_many_arguments)]
fn contract_json(
    contract_id: &str,
    work_id: &str,
    project_id: &str,
    revision: u64,
    evidence: &[&str],
    summary: &str,
    created_at: OffsetDateTime,
    extractor_version: &str,
) -> String {
    serde_json::json!({
        "contract_id": contract_id,
        "work_id": work_id,
        "revision": revision,
        "project_id": project_id,
        "objective": "Ship parser",
        "status": "active",
        "summary": summary,
        "completed_steps": [],
        "next_actions": ["run tests"],
        "blockers": [],
        "constraints": [],
        "completion_criteria": [],
        "artifacts": [],
        "verification": [],
        "evidence_refs": evidence,
        "field_evidence": {"status": evidence},
        "evidence_truncated": false,
        "confidence_basis_points": 9_000,
        "created_at": created_at,
        "extractor_version": extractor_version,
        "fact_set_digest": "b3:facts",
        "material_content_hash": format!("b3:contract-{revision}"),
        "projection_state": {
            "actions": {},
            "completion_reopened": false,
            "active_identity_watermark": [],
            "active_identity_truncated": false
        }
    })
    .to_string()
}

#[test]
fn semantic_publication_is_disabled_until_explicitly_configured() {
    assert!(
        agbox_workgraph::EndpointPolicy::default()
            .endpoint()
            .is_none()
    );
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn publish_semantic_rejects_cross_project_input_before_extractor_execution() {
    let fixture = FixtureRuntime::codex_records(1).await;
    fixture.drain().await.unwrap();
    let mut events = fixture
        .read_store()
        .events_after(0, 1, 1024 * 1024)
        .await
        .unwrap();
    let event = events.remove(0);
    let project_id = event.event.project_id().clone();
    let work_id = id::<WorkId>("work-semantic-boundary");
    let created_at = datetime!(2026-07-19 13:00 UTC);
    fixture
        .writer()
        .apply_work(WorkWriteBatch {
            visibility_name: "work-visibility-boundary".into(),
            expected_event_seq: 0,
            next_event_seq: event.event_seq,
            next_event_id: event.event.event_id().clone(),
            project_id: project_id.clone(),
            work_id: work_id.clone(),
            status: "active".into(),
            observed_at: created_at,
            evidence_event_ids: vec![event.event.event_id().clone()],
            artifact_ids: Vec::new(),
            edges: Vec::new(),
            contract: WorkContractRow {
                contract_id: id("contract-semantic-boundary"),
                revision: 1,
                contract_json: contract_json(
                    "contract-semantic-boundary",
                    work_id.as_str(),
                    project_id.as_str(),
                    1,
                    &[],
                    "Stored provisional contract",
                    created_at,
                    "deterministic-v1",
                ),
                extractor_version: "deterministic-v1".into(),
                objective: Some("Ship parser".into()),
                summary: "Stored provisional contract".into(),
                completed_steps: Vec::new(),
                next_actions: vec!["run tests".into()],
                blockers: Vec::new(),
                artifacts: Vec::new(),
                verification: Vec::new(),
            },
        })
        .await
        .unwrap();
    let stored: agbox_workgraph::ProvisionalContract = serde_json::from_str(
        &fixture
            .writer()
            .latest_work_contract(project_id.clone(), work_id.clone())
            .await
            .unwrap()
            .unwrap(),
    )
    .unwrap();
    let coordinator =
        IngestionCoordinator::new(fixture.read_store().clone(), fixture.writer().clone(), 1);
    let input = ExtractionInput::bounded(
        extraction_input_contract(
            ProjectId::parse_wire("project-foreign").unwrap(),
            work_id.clone(),
        ),
        Vec::new(),
        Vec::new(),
        Vec::new(),
    )
    .unwrap();
    let policy = SemanticPolicy::for_project(project_id.clone(), []);
    assert!(matches!(
        coordinator
            .publish_semantic(
                project_id.clone(),
                work_id.clone(),
                stored.clone(),
                input,
                &DisabledExtractor,
                &policy,
                event.event.event_id().as_str().into(),
                datetime!(2026-07-19 13:01 UTC),
            )
            .await,
        Err(IngestError::Semantic(
            agbox_workgraph::SemanticError::InvalidProposal
        ))
    ));
    let fabricated_same_project_input = ExtractionInput::bounded(
        extraction_input_contract(project_id.clone(), work_id.clone()),
        Vec::new(),
        Vec::new(),
        Vec::new(),
    )
    .unwrap();
    assert!(matches!(
        coordinator
            .publish_semantic(
                project_id,
                work_id,
                stored,
                fabricated_same_project_input,
                &DisabledExtractor,
                &policy,
                event.event.event_id().as_str().into(),
                datetime!(2026-07-19 13:01 UTC),
            )
            .await,
        Err(IngestError::InvalidGraphMutation)
    ));
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn failed_extractor_run_keeps_provisional_revision_current() {
    let fixture = FixtureRuntime::codex_records(1).await;
    fixture.drain().await.unwrap();
    let stored = fixture
        .read_store()
        .events_after(0, 1, 1024 * 1024)
        .await
        .unwrap();
    let event = &stored[0];
    let project_id = event.event.project_id().clone();
    let work_id = id::<WorkId>("work-semantic");
    let contract_id = id::<ContractId>("contract-semantic");
    let created_at = datetime!(2026-07-19 13:00 UTC);
    let base_json = contract_json(
        "contract-semantic",
        "work-semantic",
        project_id.as_str(),
        1,
        &[],
        "Provisional parser work",
        created_at,
        "deterministic-v1",
    );
    fixture
        .writer()
        .apply_work(WorkWriteBatch {
            visibility_name: "work-visibility-v1".into(),
            expected_event_seq: 0,
            next_event_seq: event.event_seq,
            next_event_id: event.event.event_id().clone(),
            project_id: project_id.clone(),
            work_id: work_id.clone(),
            status: "active".into(),
            observed_at: created_at,
            evidence_event_ids: vec![event.event.event_id().clone()],
            artifact_ids: Vec::new(),
            edges: Vec::new(),
            contract: WorkContractRow {
                contract_id,
                revision: 1,
                contract_json: base_json,
                extractor_version: "deterministic-v1".into(),
                objective: Some("Ship parser".into()),
                summary: "Provisional parser work".into(),
                completed_steps: Vec::new(),
                next_actions: vec!["run tests".into()],
                blockers: Vec::new(),
                artifacts: Vec::new(),
                verification: Vec::new(),
            },
        })
        .await
        .unwrap();
    let receipt = fixture
        .writer()
        .apply_extractor(ExtractorWriteBatch {
            extractor_run_id: "extractor-failed".into(),
            project_id: project_id.clone(),
            work_id: work_id.clone(),
            extractor_version: "loopback-v1".into(),
            input_event_watermark: event.event.event_id().as_str().into(),
            parent_revision: 1,
            parent_material_content_hash: "b3:contract-1".into(),
            status: "failed".into(),
            bounded_error: Some("disabled".into()),
            observed_at: datetime!(2026-07-19 13:01 UTC),
            refined_contract: None,
        })
        .await
        .unwrap();
    assert!(!receipt.replayed);
    assert!(!receipt.revision_inserted);
    let connection = rusqlite::Connection::open(fixture.database_path()).unwrap();
    let revisions: i64 = connection
        .query_row(
            "SELECT count(*) FROM work_contract_revisions WHERE work_id = ?1",
            [work_id.as_str()],
            |row| row.get(0),
        )
        .unwrap();
    let status: String = connection
        .query_row(
            "SELECT status FROM extractor_runs WHERE extractor_run_id = 'extractor-failed'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(revisions, 1);
    assert_eq!(status, "failed");
    let parent_mismatch = ExtractorWriteBatch {
        extractor_run_id: "extractor-parent-mismatch".into(),
        project_id: project_id.clone(),
        work_id: work_id.clone(),
        extractor_version: "loopback-v1".into(),
        input_event_watermark: event.event.event_id().as_str().into(),
        parent_revision: 1,
        parent_material_content_hash: "b3:invented-parent".into(),
        status: "failed".into(),
        bounded_error: Some("disabled".into()),
        observed_at: datetime!(2026-07-19 13:01:30 UTC),
        refined_contract: None,
    };
    assert!(matches!(
        fixture.writer().apply_extractor(parent_mismatch).await,
        Err(agbox_store::StoreError::ImmutableConflict)
    ));

    let refined_at = datetime!(2026-07-19 13:02 UTC);
    let refined_json = contract_json(
        "contract-semantic",
        "work-semantic",
        project_id.as_str(),
        2,
        &[event.event.event_id().as_str()],
        "Refined parser work",
        refined_at,
        "loopback-v1",
    );
    let refined = ExtractorWriteBatch {
        extractor_run_id: "extractor-success".into(),
        project_id: project_id.clone(),
        work_id: work_id.clone(),
        extractor_version: "loopback-v1".into(),
        input_event_watermark: event.event.event_id().as_str().into(),
        parent_revision: 1,
        parent_material_content_hash: "b3:contract-1".into(),
        status: "succeeded".into(),
        bounded_error: None,
        observed_at: refined_at,
        refined_contract: Some(WorkContractRow {
            contract_id: id("contract-semantic"),
            revision: 2,
            contract_json: refined_json,
            extractor_version: "loopback-v1".into(),
            objective: Some("Ship parser".into()),
            summary: "Refined parser work".into(),
            completed_steps: Vec::new(),
            next_actions: vec!["run tests".into()],
            blockers: Vec::new(),
            artifacts: Vec::new(),
            verification: Vec::new(),
        }),
    };
    let first_success = fixture
        .writer()
        .apply_extractor(refined.clone())
        .await
        .unwrap();
    let replay_success = fixture
        .writer()
        .apply_extractor(refined.clone())
        .await
        .unwrap();
    assert!(first_success.revision_inserted);
    assert!(replay_success.replayed);
    let mut mismatch = refined;
    let contract = mismatch.refined_contract.as_mut().unwrap();
    contract.contract_json = contract
        .contract_json
        .replace("b3:contract-2", "b3:contract-other");
    assert!(matches!(
        fixture.writer().apply_extractor(mismatch).await,
        Err(agbox_store::StoreError::ImmutableConflict)
    ));
}
