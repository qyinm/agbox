use std::fmt;

use agbox_core::{
    EvidenceId, ProjectId, Provider, WorkId, WorkStatus,
    api::{
        AppRequest, AppResponse, BoundedPage, EvidenceAvailability, EvidenceDisclosure,
        EvidenceView, HealthSnapshot, SearchHit, WorkDetail, WorkSummary,
    },
    limits::MAX_IPC_FRAME_BYTES,
};
use agbox_store::{
    AuditRecord, EvidenceContext, EvidenceMetadata, EvidenceOwnerRef, EvidenceVault, ForgetOutcome,
    ForgetTarget, ReadStore, StoreError, WriterHandle,
};
use async_trait::async_trait;
use time::OffsetDateTime;

const MAX_PAGE_BYTES: usize = MAX_IPC_FRAME_BYTES - 16 * 1024;
const MAX_EVIDENCE_RESPONSE_BYTES: usize = 64 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RequestActor {
    HumanCli,
    HumanTui,
    Agent(Provider),
}

#[derive(Clone, Eq, PartialEq)]
pub struct RequestScope {
    project_id: ProjectId,
    actor: RequestActor,
}

impl RequestScope {
    #[allow(dead_code)]
    pub(crate) fn verified(project_id: ProjectId, actor: RequestActor) -> Self {
        Self { project_id, actor }
    }
    #[must_use]
    pub fn project_id(&self) -> &ProjectId {
        &self.project_id
    }
    #[must_use]
    pub fn actor(&self) -> RequestActor {
        self.actor
    }
}

impl fmt::Debug for RequestScope {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RequestScope")
            .field("project_id", &self.project_id)
            .field("actor", &self.actor)
            .finish()
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ServiceError {
    #[error("store operation failed")]
    Store(#[from] StoreError),
    #[error("request exceeds its bounded input limit")]
    InvalidRequest,
    #[error("requested disclosure is not authorized for this scope")]
    DisclosureDenied,
    #[error("operation is not authorized for this scope")]
    OperationDenied,
    #[error("evidence is unavailable")]
    EvidenceUnavailable,
    #[error("evidence operation failed")]
    Evidence,
}

#[async_trait]
pub trait WorkReader: Send + Sync {
    async fn list_work(
        &self,
        project: &ProjectId,
        status: Option<WorkStatus>,
        limit: u16,
    ) -> Result<Vec<WorkSummary>, StoreError>;
    async fn work(
        &self,
        project: &ProjectId,
        work_id: &WorkId,
    ) -> Result<Option<WorkDetail>, StoreError>;
    async fn evidence_owner(
        &self,
        project: &ProjectId,
        evidence_id: &EvidenceId,
    ) -> Result<Option<EvidenceMetadata>, StoreError>;
    async fn search_work(
        &self,
        project: &ProjectId,
        query: String,
        limit: u16,
    ) -> Result<Vec<SearchHit>, StoreError>;
}

#[async_trait]
impl WorkReader for ReadStore {
    async fn list_work(
        &self,
        p: &ProjectId,
        s: Option<WorkStatus>,
        l: u16,
    ) -> Result<Vec<WorkSummary>, StoreError> {
        self.list_work(p, s, l).await
    }
    async fn work(&self, p: &ProjectId, w: &WorkId) -> Result<Option<WorkDetail>, StoreError> {
        self.work(p, w).await
    }
    async fn evidence_owner(
        &self,
        p: &ProjectId,
        e: &EvidenceId,
    ) -> Result<Option<EvidenceMetadata>, StoreError> {
        self.evidence_owner(p, e).await
    }
    async fn search_work(
        &self,
        p: &ProjectId,
        q: String,
        l: u16,
    ) -> Result<Vec<SearchHit>, StoreError> {
        self.search_work(p, q, l).await
    }
}

#[async_trait]
pub trait StoreWriter: Send + Sync {
    async fn record_audit(&self, record: AuditRecord) -> Result<(), StoreError>;
    async fn forget(
        &self,
        target: ForgetTarget,
        actor: &'static str,
        observed_at: OffsetDateTime,
    ) -> Result<ForgetOutcome, StoreError>;
}

#[async_trait]
impl StoreWriter for WriterHandle {
    async fn record_audit(&self, record: AuditRecord) -> Result<(), StoreError> {
        self.record_audit(record).await
    }
    async fn forget(
        &self,
        target: ForgetTarget,
        actor: &'static str,
        observed_at: OffsetDateTime,
    ) -> Result<ForgetOutcome, StoreError> {
        self.forget(target, actor, observed_at).await
    }
}

pub trait EvidenceReader: Send + Sync {
    fn get(
        &self,
        evidence_id: &EvidenceId,
        owner: &EvidenceMetadata,
    ) -> Result<Vec<u8>, ServiceError>;
}

impl EvidenceReader for EvidenceVault {
    fn get(
        &self,
        evidence_id: &EvidenceId,
        owner: &EvidenceMetadata,
    ) -> Result<Vec<u8>, ServiceError> {
        let source = match (&owner.event_id, &owner.work_id) {
            (Some(event), _) => EvidenceOwnerRef::Event(event),
            (_, Some(work)) => EvidenceOwnerRef::Work(work),
            _ => return Err(ServiceError::Evidence),
        };
        self.get(
            evidence_id,
            EvidenceContext {
                project_id: &owner.project_id,
                owner: source,
            },
        )
        .map(|data| data.to_vec())
        .map_err(|_| ServiceError::Evidence)
    }
}

#[derive(Debug)]
pub struct ApplicationService<R, W, V> {
    reader: R,
    writer: W,
    vault: V,
}
impl<R, W, V> ApplicationService<R, W, V> {
    #[must_use]
    pub fn new(reader: R, writer: W, vault: V) -> Self {
        Self {
            reader,
            writer,
            vault,
        }
    }
}

impl<R, W, V> ApplicationService<R, W, V>
where
    R: WorkReader,
    W: StoreWriter,
    V: EvidenceReader,
{
    pub async fn handle(
        &self,
        scope: RequestScope,
        request: AppRequest,
    ) -> Result<AppResponse, ServiceError> {
        match request {
            AppRequest::GetEvidence {
                evidence_id,
                disclosure,
            } => self.evidence(scope, evidence_id, disclosure).await,
            AppRequest::ListWork { status, limit } => {
                let rows = self
                    .reader
                    .list_work(scope.project_id(), status, limit.clamp(1, 100))
                    .await?;
                Ok(AppResponse::WorkList(bounded_page(rows)?))
            }
            AppRequest::GetWork { work_id } => {
                match self.reader.work(scope.project_id(), &work_id).await? {
                    Some(value) => {
                        self.audit(&scope, "handoff.work.read", Some(work_id), "ok")
                            .await?;
                        ensure_detail_size(&value)?;
                        Ok(AppResponse::Work(Box::new(value)))
                    }
                    None => Ok(AppResponse::NotFound),
                }
            }
            AppRequest::CurrentWork => {
                let active = self
                    .reader
                    .list_work(scope.project_id(), Some(WorkStatus::Active), 1)
                    .await?;
                let selected = if active.is_empty() {
                    self.reader
                        .list_work(scope.project_id(), Some(WorkStatus::Blocked), 1)
                        .await?
                } else {
                    active
                };
                match selected.into_iter().next() {
                    Some(row) => match self.reader.work(scope.project_id(), &row.work_id).await? {
                        Some(value) => {
                            self.audit(&scope, "handoff.work.read", Some(row.work_id), "ok")
                                .await?;
                            ensure_detail_size(&value)?;
                            Ok(AppResponse::Work(Box::new(value)))
                        }
                        None => Ok(AppResponse::NotFound),
                    },
                    None => Ok(AppResponse::NotFound),
                }
            }
            AppRequest::SearchWork { query, limit } => {
                if query.is_empty() || query.len() > 1024 {
                    return Err(ServiceError::InvalidRequest);
                }
                let rows = self
                    .reader
                    .search_work(scope.project_id(), query, limit.clamp(1, 100))
                    .await?;
                self.audit(&scope, "handoff.search", None, "ok").await?;
                Ok(AppResponse::Search(bounded_page(rows)?))
            }
            AppRequest::ForgetWork { work_id } => {
                self.require_cli(&scope)?;
                if self
                    .reader
                    .work(scope.project_id(), &work_id)
                    .await?
                    .is_none()
                {
                    return Ok(AppResponse::NotFound);
                }
                let _ = self
                    .writer
                    .forget(
                        ForgetTarget::Work(work_id),
                        "human_cli",
                        OffsetDateTime::now_utc(),
                    )
                    .await?;
                Ok(AppResponse::Accepted)
            }
            AppRequest::ForgetProject => {
                self.require_cli(&scope)?;
                let _ = self
                    .writer
                    .forget(
                        ForgetTarget::Project(scope.project_id().clone()),
                        "human_cli",
                        OffsetDateTime::now_utc(),
                    )
                    .await?;
                Ok(AppResponse::Accepted)
            }
            // A correction must create new immutable HumanIntent evidence and
            // a new contract revision. Fail closed until that writer operation
            // exists rather than claiming an unstored correction succeeded.
            AppRequest::CorrectWork { .. } => Err(ServiceError::OperationDenied),
            AppRequest::Health => Ok(AppResponse::Health(HealthSnapshot { ready: true })),
        }
    }

    async fn evidence(
        &self,
        scope: RequestScope,
        evidence_id: EvidenceId,
        disclosure: EvidenceDisclosure,
    ) -> Result<AppResponse, ServiceError> {
        let Some(owner) = self
            .reader
            .evidence_owner(scope.project_id(), &evidence_id)
            .await?
        else {
            return Ok(AppResponse::NotFound);
        };
        self.audit(&scope, "handoff.evidence.read", owner.work_id.clone(), "ok")
            .await?;
        match disclosure {
            EvidenceDisclosure::Redacted => Ok(AppResponse::Evidence(view_redacted(owner))),
            EvidenceDisclosure::AuthorizedRaw
                if matches!(scope.actor(), RequestActor::HumanCli) =>
            {
                if !matches!(owner.availability, EvidenceAvailability::Available) {
                    return Ok(AppResponse::Evidence(view_redacted(owner)));
                }
                let raw = self.vault.get(&evidence_id, &owner)?;
                if raw.len() > MAX_EVIDENCE_RESPONSE_BYTES {
                    return Err(ServiceError::EvidenceUnavailable);
                }
                self.audit(&scope, "handoff.evidence.raw", owner.work_id.clone(), "ok")
                    .await?;
                Ok(AppResponse::Evidence(EvidenceView {
                    evidence_id,
                    media_type: owner.media_type,
                    untrusted_data: true,
                    availability: owner.availability,
                    redacted_preview: owner.redacted_preview,
                    raw: Some(raw),
                }))
            }
            EvidenceDisclosure::AuthorizedRaw => Err(ServiceError::DisclosureDenied),
        }
    }
    async fn audit(
        &self,
        scope: &RequestScope,
        kind: &'static str,
        work_id: Option<WorkId>,
        result: &'static str,
    ) -> Result<(), ServiceError> {
        self.writer
            .record_audit(AuditRecord {
                kind,
                project_id: scope.project_id().clone(),
                work_id,
                actor: actor_name(scope.actor()),
                result,
                observed_at: OffsetDateTime::now_utc(),
            })
            .await?;
        Ok(())
    }
    fn require_cli(&self, scope: &RequestScope) -> Result<(), ServiceError> {
        if matches!(scope.actor(), RequestActor::HumanCli) {
            Ok(())
        } else {
            Err(ServiceError::OperationDenied)
        }
    }
}

fn view_redacted(owner: EvidenceMetadata) -> EvidenceView {
    EvidenceView {
        evidence_id: owner.evidence_id,
        media_type: owner.media_type,
        untrusted_data: true,
        availability: owner.availability,
        redacted_preview: owner.redacted_preview,
        raw: None,
    }
}
fn actor_name(actor: RequestActor) -> &'static str {
    match actor {
        RequestActor::HumanCli => "human_cli",
        RequestActor::HumanTui => "human_tui",
        RequestActor::Agent(Provider::Claude) => "agent_claude",
        RequestActor::Agent(Provider::Codex) => "agent_codex",
    }
}
fn bounded_page<T: serde::Serialize>(rows: Vec<T>) -> Result<BoundedPage<T>, ServiceError> {
    let mut items = Vec::new();
    let mut used = 0_usize;
    let mut truncated = false;
    for row in rows {
        let bytes = serde_json::to_vec(&row)
            .map_err(|_| ServiceError::InvalidRequest)?
            .len();
        if used
            .checked_add(bytes)
            .is_none_or(|size| size > MAX_PAGE_BYTES)
        {
            truncated = true;
            break;
        }
        used += bytes;
        items.push(row);
    }
    Ok(BoundedPage { items, truncated })
}
fn ensure_detail_size(value: &WorkDetail) -> Result<(), ServiceError> {
    (serde_json::to_vec(value)
        .map_err(|_| ServiceError::InvalidRequest)?
        .len()
        <= agbox_core::limits::MAX_CONTRACT_SERIALIZED_BYTES)
        .then_some(())
        .ok_or(ServiceError::InvalidRequest)
}

#[cfg(feature = "test-support")]
pub mod test_support {
    use super::*;
    pub fn scope(project_id: ProjectId, actor: RequestActor) -> RequestScope {
        RequestScope::verified(project_id, actor)
    }
}
