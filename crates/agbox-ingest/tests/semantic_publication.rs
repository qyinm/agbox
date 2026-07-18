#![allow(clippy::unwrap_used)]

use agbox_core::{ContractId, WorkId};
use agbox_ingest::test_support::FixtureRuntime;
use agbox_store::{ExtractorWriteBatch, WorkContractRow, WorkWriteBatch};
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
