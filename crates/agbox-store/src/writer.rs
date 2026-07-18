use std::{collections::HashSet, fmt, sync::Arc};

use agbox_core::{
    ActivityEventV1, ContentRef, DisclosureClass, EventId, EvidenceId, PrivacyLabel, ProjectId,
    Provider, SourceObservation, WorkId,
};
use rusqlite::{OptionalExtension, Transaction, TransactionBehavior, params};
use time::{OffsetDateTime, format_description::well_known::Rfc3339};
use tokio::sync::{mpsc, oneshot};
use zeroize::Zeroizing;

use crate::{EvidenceContext, EvidenceOwnerRef, EvidenceVault, StoreError};

pub const MAX_BATCH_BYTES: usize = agbox_core::limits::MAX_BATCH_SEMANTIC_BYTES;
pub const MAX_BATCH_RECORDS: usize = agbox_core::limits::MAX_BATCH_RECORDS;
pub const WRITER_QUEUE_CAPACITY: usize = 32;

#[derive(Clone)]
pub struct SourceRegistration {
    pub project_id: ProjectId,
    pub repository_identity: String,
    pub project_root: Zeroizing<Vec<u8>>,
    pub source_id: String,
    pub provider: Provider,
    pub root_class: String,
    pub source_path: Zeroizing<Vec<u8>>,
    pub file_identity: String,
    pub generation: u64,
    pub size_bytes: u64,
    pub mtime: OffsetDateTime,
    pub session_time: Option<OffsetDateTime>,
    pub initial_cursor: u64,
}

impl fmt::Debug for SourceRegistration {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SourceRegistration")
            .field("project_id", &self.project_id)
            .field("source_id", &self.source_id)
            .field("provider", &self.provider)
            .field("generation", &self.generation)
            .field("size_bytes", &self.size_bytes)
            .field("initial_cursor", &self.initial_cursor)
            .finish_non_exhaustive()
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SourceRegistrationReceipt {
    pub source_id: String,
    pub generation: u64,
    pub initial_cursor: u64,
}

#[derive(Clone, Eq, PartialEq)]
pub struct CursorState {
    pub source_id: String,
    pub generation: u64,
    pub offset: u64,
    pub parser_state: Vec<u8>,
}

impl fmt::Debug for CursorState {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CursorState")
            .field("source_id", &self.source_id)
            .field("generation", &self.generation)
            .field("offset", &self.offset)
            .field("parser_state_bytes", &self.parser_state.len())
            .finish()
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EvidenceLink {
    pub event_id: String,
    pub observation_id: String,
    pub evidence_id: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum EvidenceOwner {
    Event(EventId),
    Work(WorkId),
}

#[derive(Clone)]
pub struct EvidenceWrite {
    pub evidence_id: EvidenceId,
    pub project_id: ProjectId,
    pub owner: EvidenceOwner,
    pub content_hash: String,
    pub media_type: String,
    pub privacy: PrivacyLabel,
    pub disclosure_class: DisclosureClass,
    pub redacted_excerpt: String,
    pub expires_at: Option<OffsetDateTime>,
    pub plaintext: Zeroizing<Vec<u8>>,
}

impl fmt::Debug for EvidenceWrite {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("EvidenceWrite")
            .field("evidence_id", &self.evidence_id)
            .field("project_id", &self.project_id)
            .field("owner", &self.owner)
            .field("privacy", &self.privacy)
            .field("disclosure_class", &self.disclosure_class)
            .field("redacted_excerpt_bytes", &self.redacted_excerpt.len())
            .field("plaintext_bytes", &self.plaintext.len())
            .finish_non_exhaustive()
    }
}

#[derive(Clone, Debug)]
pub struct ContentRefWrite {
    pub content_ref_id: String,
    pub project_id: ProjectId,
    pub content: ContentRef,
    pub privacy: PrivacyLabel,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SchemaFingerprintUpdate {
    pub provider: String,
    pub format: String,
    pub fingerprint: String,
    pub observed_at: OffsetDateTime,
}

#[derive(Clone, Eq, PartialEq)]
pub struct IngestionFault {
    pub fault_id: String,
    pub source_id: String,
    pub generation: u64,
    pub byte_start: u64,
    pub byte_end: u64,
    pub class: String,
    pub bounded_detail: String,
}

impl fmt::Debug for IngestionFault {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("IngestionFault")
            .field("fault_id", &self.fault_id)
            .field("source_id", &self.source_id)
            .field("generation", &self.generation)
            .field("byte_start", &self.byte_start)
            .field("byte_end", &self.byte_end)
            .field("class", &self.class)
            .field("bounded_detail_bytes", &self.bounded_detail.len())
            .finish()
    }
}

#[derive(Clone)]
pub struct IngestionChunk {
    pub expected_cursor: CursorState,
    pub next_cursor: CursorState,
    pub observations: Vec<SourceObservation>,
    pub events: Vec<ActivityEventV1>,
    pub evidence: Vec<EvidenceWrite>,
    pub evidence_links: Vec<EvidenceLink>,
    pub content_refs: Vec<ContentRefWrite>,
    pub fingerprints: Vec<SchemaFingerprintUpdate>,
    pub faults: Vec<IngestionFault>,
}

impl fmt::Debug for IngestionChunk {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("IngestionChunk")
            .field("source_id", &self.expected_cursor.source_id)
            .field("generation", &self.expected_cursor.generation)
            .field("expected_offset", &self.expected_cursor.offset)
            .field("next_offset", &self.next_cursor.offset)
            .field(
                "expected_parser_state_bytes",
                &self.expected_cursor.parser_state.len(),
            )
            .field(
                "next_parser_state_bytes",
                &self.next_cursor.parser_state.len(),
            )
            .field("observations", &self.observations.len())
            .field("events", &self.events.len())
            .field("evidence", &self.evidence.len())
            .field("evidence_links", &self.evidence_links.len())
            .field("content_refs", &self.content_refs.len())
            .field("fingerprints", &self.fingerprints.len())
            .field("faults", &self.faults.len())
            .finish()
    }
}

impl IngestionChunk {
    /// Revalidates all batch bounds and immutable normalized values.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::InvalidBatch`] when a cardinality, byte, cursor,
    /// identity, or normalized-value invariant is violated.
    #[allow(clippy::too_many_lines)]
    pub fn validate(&self) -> Result<(), StoreError> {
        let event_capacity = self
            .observations
            .len()
            .checked_mul(agbox_core::limits::MAX_EVENTS_PER_RECORD)
            .ok_or(StoreError::InvalidBatch)?;
        let evidence_capacity = self
            .observations
            .len()
            .checked_mul(agbox_core::limits::MAX_EVIDENCE_PER_RECORD)
            .ok_or(StoreError::InvalidBatch)?;
        let content_per_record = agbox_core::limits::MAX_EVENTS_PER_RECORD
            .checked_add(agbox_core::limits::MAX_EVIDENCE_PER_RECORD)
            .and_then(|value| value.checked_add(1))
            .ok_or(StoreError::InvalidBatch)?;
        let content_ref_capacity = self
            .observations
            .len()
            .checked_mul(content_per_record)
            .ok_or(StoreError::InvalidBatch)?;

        if self.observations.len() > MAX_BATCH_RECORDS
            || self.events.len() > event_capacity
            || self.evidence.len() > evidence_capacity
            || self.evidence_links.len() > evidence_capacity
            || self.content_refs.len() > content_ref_capacity
            || self.fingerprints.len() > self.observations.len()
            || self.faults.len() > self.observations.len()
            || self.expected_cursor.parser_state.len() > agbox_core::limits::MAX_DECODER_STATE_BYTES
            || self.next_cursor.parser_state.len() > agbox_core::limits::MAX_DECODER_STATE_BYTES
            || self.expected_cursor.source_id != self.next_cursor.source_id
            || self.expected_cursor.generation != self.next_cursor.generation
            || self.expected_cursor.generation == 0
            || self.next_cursor.offset < self.expected_cursor.offset
            || !bounded_identifier(&self.expected_cursor.source_id)
            || self.expected_cursor.offset > i64::MAX as u64
            || self.next_cursor.offset > i64::MAX as u64
            || self.expected_cursor.generation > i64::MAX as u64
        {
            return Err(StoreError::InvalidBatch);
        }

        for observation in &self.observations {
            observation
                .validate()
                .map_err(|_| StoreError::InvalidBatch)?;
            if observation.source().source_generation() != self.expected_cursor.generation
                || observation.range().end() < observation.range().start()
                || observation.range().end() > i64::MAX as u64
            {
                return Err(StoreError::InvalidBatch);
            }
        }
        for event in &self.events {
            event.validate().map_err(|_| StoreError::InvalidBatch)?;
            if event.source().source_generation() != self.expected_cursor.generation {
                return Err(StoreError::InvalidBatch);
            }
        }
        for item in &self.evidence {
            if item.plaintext.len() > agbox_core::limits::MAX_INLINE_BYTES
                || item.redacted_excerpt.len() > agbox_core::limits::MAX_PREVIEW_BYTES
                || !bounded_identifier(item.evidence_id.as_str())
                || !bounded_identifier(item.project_id.as_str())
                || !bounded_metadata(&item.content_hash)
                || !bounded_metadata(&item.media_type)
            {
                return Err(StoreError::InvalidBatch);
            }
        }
        for item in &self.evidence_links {
            if !bounded_identifier(&item.event_id)
                || !bounded_identifier(&item.observation_id)
                || !bounded_identifier(&item.evidence_id)
            {
                return Err(StoreError::InvalidBatch);
            }
        }
        for item in &self.content_refs {
            item.content
                .validate()
                .map_err(|_| StoreError::InvalidBatch)?;
            if item.content_ref_id != stable_content_ref_id(&item.project_id, &item.content)?
                || !bounded_identifier(&item.content_ref_id)
            {
                return Err(StoreError::InvalidContentRefId);
            }
        }
        for item in &self.fingerprints {
            if !bounded_metadata(&item.provider)
                || !bounded_metadata(&item.format)
                || !bounded_metadata(&item.fingerprint)
            {
                return Err(StoreError::InvalidBatch);
            }
        }
        for fault in &self.faults {
            if fault.source_id != self.expected_cursor.source_id
                || fault.generation != self.expected_cursor.generation
                || fault.byte_end < fault.byte_start
                || fault.byte_end > i64::MAX as u64
                || !bounded_identifier(&fault.fault_id)
                || !bounded_identifier(&fault.source_id)
                || !bounded_metadata(&fault.class)
                || fault.bounded_detail.len() > agbox_core::limits::MAX_PREVIEW_BYTES
            {
                return Err(StoreError::InvalidBatch);
            }
        }
        if self.measured_semantic_bytes()? > MAX_BATCH_BYTES {
            return Err(StoreError::InvalidBatch);
        }
        Ok(())
    }

    /// Measures all retained semantic data with checked arithmetic.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::InvalidBatch`] on arithmetic overflow or a
    /// serialization failure.
    pub fn measured_semantic_bytes(&self) -> Result<usize, StoreError> {
        let mut total = 0_usize;
        add_len(&mut total, self.expected_cursor.source_id.len())?;
        add_len(&mut total, self.expected_cursor.parser_state.len())?;
        add_len(&mut total, self.next_cursor.source_id.len())?;
        add_len(&mut total, self.next_cursor.parser_state.len())?;
        add_len(
            &mut total,
            size_of::<u64>()
                .checked_mul(4)
                .ok_or(StoreError::InvalidBatch)?,
        )?;

        for observation in &self.observations {
            add_len(&mut total, serde_json::to_vec(observation)?.len())?;
        }
        for event in &self.events {
            add_len(&mut total, serde_json::to_vec(event)?.len())?;
        }
        for item in &self.evidence {
            add_len(&mut total, item.evidence_id.as_str().len())?;
            add_len(&mut total, item.project_id.as_str().len())?;
            add_len(&mut total, owner_kind(&item.owner).len())?;
            add_len(&mut total, owner_id(&item.owner).len())?;
            add_len(&mut total, item.content_hash.len())?;
            add_len(&mut total, item.media_type.len())?;
            add_len(&mut total, privacy(item.privacy).len())?;
            add_len(&mut total, disclosure(item.disclosure_class).len())?;
            add_len(&mut total, item.redacted_excerpt.len())?;
            add_len(&mut total, 1)?;
            if let Some(expires_at) = item.expires_at {
                add_len(&mut total, format_timestamp(expires_at)?.len())?;
            }
            add_len(&mut total, item.plaintext.len())?;
        }
        for item in &self.evidence_links {
            add_len(&mut total, item.event_id.len())?;
            add_len(&mut total, item.observation_id.len())?;
            add_len(&mut total, item.evidence_id.len())?;
        }
        for item in &self.content_refs {
            add_len(&mut total, item.content_ref_id.len())?;
            add_len(&mut total, item.project_id.as_str().len())?;
            add_len(&mut total, serde_json::to_vec(&item.content)?.len())?;
            add_len(&mut total, privacy(item.privacy).len())?;
        }
        for item in &self.fingerprints {
            add_len(&mut total, item.provider.len())?;
            add_len(&mut total, item.format.len())?;
            add_len(&mut total, item.fingerprint.len())?;
            add_len(&mut total, format_timestamp(item.observed_at)?.len())?;
        }
        for fault in &self.faults {
            add_len(&mut total, fault.fault_id.len())?;
            add_len(&mut total, fault.source_id.len())?;
            add_len(&mut total, fault.class.len())?;
            add_len(&mut total, fault.bounded_detail.len())?;
            add_len(
                &mut total,
                size_of::<u64>()
                    .checked_mul(3)
                    .ok_or(StoreError::InvalidBatch)?,
            )?;
        }
        Ok(total)
    }

    fn commit_digest(&self) -> Result<String, StoreError> {
        let mut manifest = ManifestHasher::new();
        manifest.bytes(
            "expected.source_id",
            self.expected_cursor.source_id.as_bytes(),
        )?;
        manifest.u64("expected.generation", self.expected_cursor.generation)?;
        manifest.u64("expected.offset", self.expected_cursor.offset)?;
        manifest.bytes("expected.parser_state", &self.expected_cursor.parser_state)?;
        manifest.bytes("next.source_id", self.next_cursor.source_id.as_bytes())?;
        manifest.u64("next.generation", self.next_cursor.generation)?;
        manifest.u64("next.offset", self.next_cursor.offset)?;
        manifest.bytes("next.parser_state", &self.next_cursor.parser_state)?;

        manifest.vector_len("observations", self.observations.len())?;
        for item in &self.observations {
            manifest.bytes("observation", &serde_json::to_vec(item)?)?;
        }
        manifest.vector_len("events", self.events.len())?;
        for item in &self.events {
            manifest.bytes("event", &serde_json::to_vec(item)?)?;
        }
        manifest.vector_len("evidence", self.evidence.len())?;
        for item in &self.evidence {
            manifest.bytes("evidence.id", item.evidence_id.as_str().as_bytes())?;
            manifest.bytes("evidence.project", item.project_id.as_str().as_bytes())?;
            manifest.bytes("evidence.owner_kind", owner_kind(&item.owner).as_bytes())?;
            manifest.bytes("evidence.owner_id", owner_id(&item.owner).as_bytes())?;
            manifest.bytes("evidence.content_hash", item.content_hash.as_bytes())?;
            manifest.bytes("evidence.media_type", item.media_type.as_bytes())?;
            manifest.bytes("evidence.privacy", privacy(item.privacy).as_bytes())?;
            manifest.bytes(
                "evidence.disclosure",
                disclosure(item.disclosure_class).as_bytes(),
            )?;
            manifest.bytes(
                "evidence.redacted_excerpt",
                item.redacted_excerpt.as_bytes(),
            )?;
            manifest.u64(
                "evidence.byte_length",
                u64::try_from(item.plaintext.len()).map_err(|_| StoreError::InvalidBatch)?,
            )?;
            match item.expires_at {
                Some(value) => {
                    manifest.u64("evidence.expires.present", 1)?;
                    manifest.bytes(
                        "evidence.expires.value",
                        format_timestamp(value)?.as_bytes(),
                    )?;
                }
                None => manifest.u64("evidence.expires.present", 0)?,
            }
        }
        manifest.vector_len("evidence_links", self.evidence_links.len())?;
        for item in &self.evidence_links {
            manifest.bytes("link.event", item.event_id.as_bytes())?;
            manifest.bytes("link.observation", item.observation_id.as_bytes())?;
            manifest.bytes("link.evidence", item.evidence_id.as_bytes())?;
        }
        manifest.vector_len("content_refs", self.content_refs.len())?;
        for item in &self.content_refs {
            manifest.bytes("content.id", item.content_ref_id.as_bytes())?;
            manifest.bytes("content.project", item.project_id.as_str().as_bytes())?;
            manifest.bytes("content.value", &serde_json::to_vec(&item.content)?)?;
            manifest.bytes("content.privacy", privacy(item.privacy).as_bytes())?;
        }
        manifest.vector_len("fingerprints", self.fingerprints.len())?;
        for item in &self.fingerprints {
            manifest.bytes("fingerprint.provider", item.provider.as_bytes())?;
            manifest.bytes("fingerprint.format", item.format.as_bytes())?;
            manifest.bytes("fingerprint.value", item.fingerprint.as_bytes())?;
            manifest.bytes(
                "fingerprint.observed_at",
                format_timestamp(item.observed_at)?.as_bytes(),
            )?;
        }
        manifest.vector_len("faults", self.faults.len())?;
        for item in &self.faults {
            manifest.bytes("fault.id", item.fault_id.as_bytes())?;
            manifest.bytes("fault.source", item.source_id.as_bytes())?;
            manifest.u64("fault.generation", item.generation)?;
            manifest.u64("fault.byte_start", item.byte_start)?;
            manifest.u64("fault.byte_end", item.byte_end)?;
            manifest.bytes("fault.class", item.class.as_bytes())?;
            manifest.bytes("fault.detail", item.bounded_detail.as_bytes())?;
        }
        Ok(manifest.finish())
    }
}

struct ManifestHasher {
    hasher: blake3::Hasher,
}

impl ManifestHasher {
    fn new() -> Self {
        let mut hasher = blake3::Hasher::new();
        hasher.update(b"agbox.ingestion.commit.v1");
        Self { hasher }
    }

    fn bytes(&mut self, label: &str, value: &[u8]) -> Result<(), StoreError> {
        hash_part(&mut self.hasher, label.as_bytes())?;
        hash_part(&mut self.hasher, value)
    }

    fn u64(&mut self, label: &str, value: u64) -> Result<(), StoreError> {
        self.bytes(label, &value.to_le_bytes())
    }

    fn vector_len(&mut self, label: &str, value: usize) -> Result<(), StoreError> {
        self.u64(
            label,
            u64::try_from(value).map_err(|_| StoreError::InvalidBatch)?,
        )
    }

    fn finish(self) -> String {
        self.hasher.finalize().to_hex().to_string()
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CommitReceipt {
    pub source_id: String,
    pub generation: u64,
    pub cursor_offset: u64,
    pub inserted_events: usize,
}

pub(crate) enum WriteCommand {
    RegisterSource {
        registration: Box<SourceRegistration>,
        reply: oneshot::Sender<Result<SourceRegistrationReceipt, StoreError>>,
    },
    Commit {
        chunk: Box<IngestionChunk>,
        reply: oneshot::Sender<Result<CommitReceipt, StoreError>>,
    },
    Shutdown {
        reply: oneshot::Sender<()>,
    },
}

#[derive(Clone)]
pub struct WriterHandle {
    pub(crate) sender: mpsc::Sender<WriteCommand>,
}

impl fmt::Debug for WriterHandle {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("WriterHandle")
            .finish_non_exhaustive()
    }
}

impl WriterHandle {
    /// Atomically registers one project, source, generation, and initial cursor.
    ///
    /// # Errors
    ///
    /// Returns an error for invalid, conflicting, regressing, partially
    /// associated, unencryptable, or unpersistable registration state.
    pub async fn register_source(
        &self,
        registration: SourceRegistration,
    ) -> Result<SourceRegistrationReceipt, StoreError> {
        validate_registration(&registration)?;
        let (reply, receive) = oneshot::channel();
        self.sender
            .send(WriteCommand::RegisterSource {
                registration: Box::new(registration),
                reply,
            })
            .await
            .map_err(|_| StoreError::WriterStopped)?;
        receive.await.map_err(|_| StoreError::WriterStopped)?
    }

    /// Atomically commits a validated bounded ingestion chunk.
    ///
    /// # Errors
    ///
    /// Returns a validation, cursor, immutable-row, evidence, writer, or
    /// database error without advancing the cursor.
    pub async fn commit_ingestion(
        &self,
        chunk: IngestionChunk,
    ) -> Result<CommitReceipt, StoreError> {
        chunk.validate()?;
        let (reply, receive) = oneshot::channel();
        self.sender
            .send(WriteCommand::Commit {
                chunk: Box::new(chunk),
                reply,
            })
            .await
            .map_err(|_| StoreError::WriterStopped)?;
        receive.await.map_err(|_| StoreError::WriterStopped)?
    }

    #[cfg(feature = "test-support")]
    #[doc(hidden)]
    #[must_use]
    pub fn available_capacity_for_test(&self) -> usize {
        self.sender.capacity()
    }
}

#[allow(clippy::needless_pass_by_value)]
pub(crate) fn run_writer(
    mut connection: rusqlite::Connection,
    vault: Arc<EvidenceVault>,
    mut commands: mpsc::Receiver<WriteCommand>,
) {
    while let Some(command) = commands.blocking_recv() {
        match command {
            WriteCommand::RegisterSource {
                registration,
                reply,
            } => {
                let _ = reply.send(register_source(&mut connection, &vault, &registration));
            }
            WriteCommand::Commit { chunk, reply } => {
                let _ = reply.send(commit(&mut connection, &vault, &chunk));
            }
            WriteCommand::Shutdown { reply } => {
                let _ = reply.send(());
                break;
            }
        }
    }
}

fn validate_registration(registration: &SourceRegistration) -> Result<(), StoreError> {
    if !bounded_identifier(registration.project_id.as_str())
        || !bounded_identifier(&registration.source_id)
        || !bounded_metadata(&registration.repository_identity)
        || !bounded_metadata(&registration.file_identity)
        || !matches!(registration.root_class.as_str(), "active" | "archive")
        || registration.generation == 0
        || registration.generation > i64::MAX as u64
        || registration.size_bytes > i64::MAX as u64
        || registration.initial_cursor > i64::MAX as u64
        || (registration.initial_cursor != 0
            && registration.initial_cursor != registration.size_bytes)
        || registration.project_root.is_empty()
        || registration.source_path.is_empty()
        || registration.project_root.len() > 32 * 1024
        || registration.source_path.len() > 32 * 1024
    {
        return Err(StoreError::InvalidBatch);
    }
    let _ = format_timestamp(registration.mtime)?;
    if let Some(session_time) = registration.session_time {
        let _ = format_timestamp(session_time)?;
    }
    Ok(())
}

fn register_source(
    connection: &mut rusqlite::Connection,
    vault: &EvidenceVault,
    registration: &SourceRegistration,
) -> Result<SourceRegistrationReceipt, StoreError> {
    validate_registration(registration)?;
    let project_aad = registration_aad(
        b"agbox.db.project-root.v1",
        &[
            registration.project_id.as_str().as_bytes(),
            registration.repository_identity.as_bytes(),
        ],
    )?;
    let source_aad = registration_aad(
        b"agbox.db.source-path.v1",
        &[
            registration.project_id.as_str().as_bytes(),
            registration.source_id.as_bytes(),
            &registration.generation.to_le_bytes(),
        ],
    )?;
    // Both sensitive fields are sealed before SQLite sees any value from the
    // registration. Plaintext remains in its original Zeroizing allocation.
    let encrypted_root =
        vault.seal_database_field(&project_aad, registration.project_root.as_slice())?;
    let encrypted_source =
        vault.seal_database_field(&source_aad, registration.source_path.as_slice())?;

    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    validate_registration_associations(&transaction, registration)?;
    let mtime = format_timestamp(registration.mtime)?;
    let session_time = registration
        .session_time
        .map(format_timestamp)
        .transpose()?;

    transaction.execute(
        "INSERT INTO projects(
             project_id, repository_identity, encrypted_root_path, created_at, updated_at
         ) VALUES (?1, ?2, ?3,
             strftime('%Y-%m-%dT%H:%M:%fZ', 'now'),
             strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
         ON CONFLICT(project_id) DO UPDATE SET
             encrypted_root_path = excluded.encrypted_root_path,
             updated_at = excluded.updated_at",
        params![
            registration.project_id.as_str(),
            registration.repository_identity,
            encrypted_root,
        ],
    )?;
    transaction.execute(
        "INSERT INTO sources(
             source_id, project_id, provider, root_class, encrypted_path,
             file_identity, created_at, updated_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6,
             strftime('%Y-%m-%dT%H:%M:%fZ', 'now'),
             strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
         ON CONFLICT(source_id) DO UPDATE SET
             encrypted_path = excluded.encrypted_path,
             file_identity = excluded.file_identity,
             updated_at = excluded.updated_at",
        params![
            registration.source_id,
            registration.project_id.as_str(),
            registration.provider.as_str(),
            registration.root_class,
            encrypted_source,
            registration.file_identity,
        ],
    )?;
    transaction.execute(
        "INSERT INTO source_generations(
             source_id, generation, size_bytes, mtime, session_time,
             schema_fingerprint, status
         ) VALUES (?1, ?2, ?3, ?4, ?5, NULL, 'active')
         ON CONFLICT(source_id, generation) DO NOTHING",
        params![
            registration.source_id,
            to_i64(registration.generation)?,
            to_i64(registration.size_bytes)?,
            mtime,
            session_time,
        ],
    )?;
    let registration_digest = registration_digest(registration)?;
    transaction.execute(
        "INSERT INTO source_cursors(
             source_id, generation, cursor_offset, parser_state,
             last_commit_digest, updated_at
         ) VALUES (?1, ?2, ?3, X'', ?4,
             strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
         ON CONFLICT(source_id, generation) DO NOTHING",
        params![
            registration.source_id,
            to_i64(registration.generation)?,
            to_i64(registration.initial_cursor)?,
            registration_digest,
        ],
    )?;
    transaction.commit()?;
    Ok(SourceRegistrationReceipt {
        source_id: registration.source_id.clone(),
        generation: registration.generation,
        initial_cursor: registration.initial_cursor,
    })
}

#[allow(clippy::too_many_lines)]
fn validate_registration_associations(
    transaction: &Transaction<'_>,
    registration: &SourceRegistration,
) -> Result<(), StoreError> {
    let project: Option<String> = transaction
        .query_row(
            "SELECT repository_identity FROM projects WHERE project_id = ?1",
            [registration.project_id.as_str()],
            |row| row.get(0),
        )
        .optional()?;
    if project
        .as_deref()
        .is_some_and(|identity| identity != registration.repository_identity)
    {
        return Err(StoreError::ImmutableConflict);
    }
    let repository_owner: Option<String> = transaction
        .query_row(
            "SELECT project_id FROM projects WHERE repository_identity = ?1 LIMIT 1",
            [registration.repository_identity.as_str()],
            |row| row.get(0),
        )
        .optional()?;
    if repository_owner
        .as_deref()
        .is_some_and(|project_id| project_id != registration.project_id.as_str())
    {
        return Err(StoreError::ImmutableConflict);
    }

    let generation = to_i64(registration.generation)?;
    let maximum: Option<i64> = transaction.query_row(
        "SELECT max(generation) FROM source_generations WHERE source_id = ?1",
        [registration.source_id.as_str()],
        |row| row.get(0),
    )?;
    if maximum.is_some_and(|maximum| generation < maximum) {
        return Err(StoreError::ImmutableConflict);
    }

    let source: Option<(String, String, String, String)> = transaction
        .query_row(
            "SELECT project_id, provider, root_class, file_identity
             FROM sources WHERE source_id = ?1",
            [registration.source_id.as_str()],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .optional()?;
    let source_exists = source.is_some();
    if let Some((project, provider, root_class, file_identity)) = source {
        let is_next_replacement = maximum
            .and_then(|value| value.checked_add(1))
            .is_some_and(|next| next == generation);
        if project != registration.project_id.as_str()
            || provider != registration.provider.as_str()
            || root_class != registration.root_class
            || (file_identity != registration.file_identity && !is_next_replacement)
        {
            return Err(StoreError::ImmutableConflict);
        }
    }
    let file_owner: Option<String> = transaction
        .query_row(
            "SELECT source_id FROM sources WHERE file_identity = ?1 LIMIT 1",
            [registration.file_identity.as_str()],
            |row| row.get(0),
        )
        .optional()?;
    if file_owner
        .as_deref()
        .is_some_and(|source_id| source_id != registration.source_id)
    {
        return Err(StoreError::ImmutableConflict);
    }

    if source_exists && maximum.is_none() {
        return Err(StoreError::ImmutableConflict);
    }
    let existing: Option<(i64, String, Option<String>, i64)> = transaction
        .query_row(
            "SELECT source_generations.size_bytes, source_generations.mtime,
                    source_generations.session_time, source_cursors.cursor_offset
             FROM source_generations
             INNER JOIN source_cursors USING (source_id, generation)
             WHERE source_id = ?1 AND generation = ?2",
            params![registration.source_id, generation],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .optional()?;
    let expected_mtime = format_timestamp(registration.mtime)?;
    let expected_session = registration
        .session_time
        .map(format_timestamp)
        .transpose()?;
    if let Some((size, mtime, session_time, cursor)) = existing {
        if size != to_i64(registration.size_bytes)?
            || mtime != expected_mtime
            || session_time != expected_session
            || cursor != to_i64(registration.initial_cursor)?
        {
            return Err(StoreError::ImmutableConflict);
        }
        return Ok(());
    }

    match maximum {
        None if registration.generation == 1 => Ok(()),
        Some(previous) if previous.checked_add(1) == Some(generation) => Ok(()),
        _ => Err(StoreError::ImmutableConflict),
    }
}

fn registration_aad(domain: &[u8], parts: &[&[u8]]) -> Result<Vec<u8>, StoreError> {
    let mut aad = Vec::with_capacity(256);
    append_aad_part(&mut aad, domain)?;
    for part in parts {
        append_aad_part(&mut aad, part)?;
    }
    if aad.len() > 32 * 1024 {
        return Err(StoreError::InvalidBatch);
    }
    Ok(aad)
}

fn append_aad_part(aad: &mut Vec<u8>, part: &[u8]) -> Result<(), StoreError> {
    aad.extend_from_slice(
        &u64::try_from(part.len())
            .map_err(|_| StoreError::InvalidBatch)?
            .to_le_bytes(),
    );
    aad.extend_from_slice(part);
    Ok(())
}

fn registration_digest(registration: &SourceRegistration) -> Result<String, StoreError> {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"agbox.source.registration.v1");
    for part in [
        registration.source_id.as_bytes(),
        &registration.generation.to_le_bytes(),
        &registration.initial_cursor.to_le_bytes(),
    ] {
        hasher.update(
            &u64::try_from(part.len())
                .map_err(|_| StoreError::InvalidBatch)?
                .to_le_bytes(),
        );
        hasher.update(part);
    }
    Ok(hasher.finalize().to_hex().to_string())
}

fn commit(
    connection: &mut rusqlite::Connection,
    vault: &EvidenceVault,
    chunk: &IngestionChunk,
) -> Result<CommitReceipt, StoreError> {
    let commit_digest = chunk.commit_digest()?;
    let preflight_cursor = load_cursor(connection, &chunk.expected_cursor)?;
    let registered = load_registered_source(connection, chunk)?;
    validate_project_and_provider(chunk, &registered)?;
    validate_evidence_relations(connection, chunk, &registered)?;

    if cursor_matches(preflight_cursor.as_ref(), &chunk.next_cursor) {
        let transaction = connection.transaction()?;
        let current = load_cursor(&transaction, &chunk.expected_cursor)?;
        if !cursor_matches(current.as_ref(), &chunk.next_cursor) {
            return Err(StoreError::CursorConflict);
        }
        if current
            .as_ref()
            .is_none_or(|cursor| cursor.last_commit_digest != commit_digest)
        {
            return Err(StoreError::ImmutableConflict);
        }
        let registered = load_registered_source(&transaction, chunk)?;
        validate_project_and_provider(chunk, &registered)?;
        validate_evidence_relations(&transaction, chunk, &registered)?;
        verify_retry(&transaction, chunk)?;
        transaction.commit()?;
        persist_evidence_blobs(vault, &chunk.evidence)?;
        return Ok(receipt(chunk, 0));
    }

    if !cursor_matches_expected(preflight_cursor.as_ref(), &chunk.expected_cursor) {
        return Err(StoreError::CursorConflict);
    }

    persist_evidence_blobs(vault, &chunk.evidence)?;
    let transaction = connection.transaction()?;
    let current = load_cursor(&transaction, &chunk.expected_cursor)?;
    if !cursor_matches_expected(current.as_ref(), &chunk.expected_cursor) {
        return Err(StoreError::CursorConflict);
    }
    let registered = load_registered_source(&transaction, chunk)?;
    validate_project_and_provider(chunk, &registered)?;
    validate_evidence_relations(&transaction, chunk, &registered)?;

    insert_observations(&transaction, chunk)?;
    let inserted_events = insert_events(&transaction, &chunk.events)?;
    insert_evidence_objects(&transaction, &chunk.evidence)?;
    insert_evidence_links(&transaction, &chunk.evidence_links)?;
    insert_content_refs(&transaction, &chunk.content_refs)?;
    upsert_schema_fingerprints(&transaction, &chunk.fingerprints)?;
    insert_faults(&transaction, &chunk.faults)?;
    transaction.execute(
        "INSERT INTO source_cursors(
             source_id, generation, cursor_offset, parser_state,
             last_commit_digest, updated_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
         ON CONFLICT(source_id, generation) DO UPDATE SET
             cursor_offset = excluded.cursor_offset,
             parser_state = excluded.parser_state,
             last_commit_digest = excluded.last_commit_digest,
             updated_at = excluded.updated_at",
        params![
            chunk.next_cursor.source_id,
            to_i64(chunk.next_cursor.generation)?,
            to_i64(chunk.next_cursor.offset)?,
            chunk.next_cursor.parser_state,
            commit_digest,
        ],
    )?;
    transaction.commit()?;
    Ok(receipt(chunk, inserted_events))
}

fn receipt(chunk: &IngestionChunk, inserted_events: usize) -> CommitReceipt {
    CommitReceipt {
        source_id: chunk.next_cursor.source_id.clone(),
        generation: chunk.next_cursor.generation,
        cursor_offset: chunk.next_cursor.offset,
        inserted_events,
    }
}

struct RegisteredSource {
    project_id: ProjectId,
    provider: Provider,
}

fn load_registered_source(
    connection: &rusqlite::Connection,
    chunk: &IngestionChunk,
) -> Result<RegisteredSource, StoreError> {
    let value: Option<(String, String)> = connection
        .query_row(
            "SELECT sources.project_id, sources.provider
             FROM sources
             INNER JOIN source_generations USING (source_id)
             WHERE sources.source_id = ?1 AND source_generations.generation = ?2",
            params![
                chunk.expected_cursor.source_id,
                to_i64(chunk.expected_cursor.generation)?
            ],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    let (project, provider) = value.ok_or(StoreError::SourceNotFound)?;
    let project_id = ProjectId::parse_wire(&project).ok_or(StoreError::InvalidBatch)?;
    let provider = match provider.as_str() {
        "claude" => Provider::Claude,
        "codex" => Provider::Codex,
        _ => return Err(StoreError::InvalidBatch),
    };
    Ok(RegisteredSource {
        project_id,
        provider,
    })
}

fn validate_project_and_provider(
    chunk: &IngestionChunk,
    registered: &RegisteredSource,
) -> Result<(), StoreError> {
    if chunk
        .events
        .iter()
        .any(|event| event.project_id() != &registered.project_id)
        || chunk
            .evidence
            .iter()
            .any(|item| item.project_id != registered.project_id)
        || chunk
            .content_refs
            .iter()
            .any(|item| item.project_id != registered.project_id)
        || chunk
            .events
            .iter()
            .any(|event| event.source().provider() != registered.provider)
        || chunk
            .observations
            .iter()
            .any(|item| item.source().provider() != registered.provider)
        || chunk
            .fingerprints
            .iter()
            .any(|item| item.provider != registered.provider.as_str())
    {
        return Err(StoreError::ProjectMismatch);
    }
    Ok(())
}

fn validate_evidence_relations(
    connection: &rusqlite::Connection,
    chunk: &IngestionChunk,
    registered: &RegisteredSource,
) -> Result<(), StoreError> {
    let chunk_events: HashSet<&str> = chunk
        .events
        .iter()
        .map(|event| event.event_id().as_str())
        .collect();
    let chunk_evidence: HashSet<&str> = chunk
        .evidence
        .iter()
        .map(|evidence| evidence.evidence_id.as_str())
        .collect();
    let chunk_observations: HashSet<&str> = chunk
        .observations
        .iter()
        .map(SourceObservation::observation_id)
        .collect();

    for evidence in &chunk.evidence {
        if !evidence_reference_is_valid(
            connection,
            evidence.evidence_id.as_str(),
            &registered.project_id,
            true,
        )? {
            return Err(StoreError::InvalidReference);
        }
        let owner_is_valid = match &evidence.owner {
            EvidenceOwner::Event(event_id) => {
                let created_in_chunk = chunk_events.contains(event_id.as_str());
                event_reference_is_valid(
                    connection,
                    event_id.as_str(),
                    &registered.project_id,
                    created_in_chunk,
                )?
            }
            EvidenceOwner::Work(work_id) => {
                work_exists_in_project(connection, work_id.as_str(), &registered.project_id)?
            }
        };
        if !owner_is_valid {
            return Err(StoreError::InvalidReference);
        }
    }

    for link in &chunk.evidence_links {
        let event_in_chunk = chunk_events.contains(link.event_id.as_str());
        let event_is_valid = event_reference_is_valid(
            connection,
            &link.event_id,
            &registered.project_id,
            event_in_chunk,
        )?;
        let evidence_in_chunk = chunk_evidence.contains(link.evidence_id.as_str());
        let evidence_is_valid = evidence_reference_is_valid(
            connection,
            &link.evidence_id,
            &registered.project_id,
            evidence_in_chunk,
        )?;
        let observation_in_chunk = chunk_observations.contains(link.observation_id.as_str());
        let observation_is_valid = observation_reference_is_valid(
            connection,
            &link.observation_id,
            &chunk.expected_cursor,
            &registered.project_id,
            observation_in_chunk,
        )?;
        if !event_is_valid || !evidence_is_valid || !observation_is_valid {
            return Err(StoreError::InvalidReference);
        }
    }
    Ok(())
}

fn event_reference_is_valid(
    connection: &rusqlite::Connection,
    event_id: &str,
    project_id: &ProjectId,
    created_in_chunk: bool,
) -> Result<bool, StoreError> {
    let mut statement =
        connection.prepare_cached("SELECT project_id FROM activity_events WHERE event_id = ?1")?;
    let stored_project: Option<String> = statement
        .query_row([event_id], |row| row.get(0))
        .optional()?;
    Ok(stored_project
        .as_deref()
        .map_or(created_in_chunk, |stored| stored == project_id.as_str()))
}

fn work_exists_in_project(
    connection: &rusqlite::Connection,
    work_id: &str,
    project_id: &ProjectId,
) -> Result<bool, StoreError> {
    let mut statement = connection.prepare_cached(
        "SELECT EXISTS(
             SELECT 1 FROM work_items
             WHERE work_id = ?1 AND project_id = ?2
         )",
    )?;
    Ok(statement.query_row(params![work_id, project_id.as_str()], |row| row.get(0))?)
}

fn evidence_reference_is_valid(
    connection: &rusqlite::Connection,
    evidence_id: &str,
    project_id: &ProjectId,
    created_in_chunk: bool,
) -> Result<bool, StoreError> {
    let mut statement = connection
        .prepare_cached("SELECT project_id FROM evidence_objects WHERE evidence_id = ?1")?;
    let stored_project: Option<String> = statement
        .query_row([evidence_id], |row| row.get(0))
        .optional()?;
    Ok(stored_project
        .as_deref()
        .map_or(created_in_chunk, |stored| stored == project_id.as_str()))
}

fn observation_reference_is_valid(
    connection: &rusqlite::Connection,
    observation_id: &str,
    cursor: &CursorState,
    project_id: &ProjectId,
    created_in_chunk: bool,
) -> Result<bool, StoreError> {
    let mut statement = connection.prepare_cached(
        "SELECT source_observations.source_id,
                source_observations.generation,
                sources.project_id
             FROM source_observations
             INNER JOIN sources USING (source_id)
             WHERE observation_id = ?1
         ",
    )?;
    let stored: Option<(String, i64, String)> = statement
        .query_row([observation_id], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?))
        })
        .optional()?;
    stored.map_or_else(
        || Ok(created_in_chunk),
        |(source_id, generation, stored_project)| {
            Ok(source_id == cursor.source_id
                && generation == to_i64(cursor.generation)?
                && stored_project == project_id.as_str())
        },
    )
}

struct StoredCursor {
    offset: u64,
    parser_state: Vec<u8>,
    last_commit_digest: String,
}

fn load_cursor(
    connection: &rusqlite::Connection,
    cursor: &CursorState,
) -> Result<Option<StoredCursor>, StoreError> {
    connection
        .query_row(
            "SELECT cursor_offset, parser_state, last_commit_digest
             FROM source_cursors
             WHERE source_id = ?1 AND generation = ?2",
            params![cursor.source_id, to_i64(cursor.generation)?],
            |row| {
                let offset: i64 = row.get(0)?;
                let parser_state = row.get(1)?;
                let last_commit_digest = row.get(2)?;
                Ok((offset, parser_state, last_commit_digest))
            },
        )
        .optional()?
        .map(|(offset, parser_state, last_commit_digest)| {
            let offset = u64::try_from(offset).map_err(|_| StoreError::InvalidBatch)?;
            Ok(StoredCursor {
                offset,
                parser_state,
                last_commit_digest,
            })
        })
        .transpose()
}

fn cursor_matches(current: Option<&StoredCursor>, wanted: &CursorState) -> bool {
    current.is_some_and(|cursor| {
        cursor.offset == wanted.offset && cursor.parser_state == wanted.parser_state
    })
}

fn cursor_matches_expected(current: Option<&StoredCursor>, wanted: &CursorState) -> bool {
    current.map_or_else(
        || wanted.offset == 0 && wanted.parser_state.is_empty(),
        |cursor| cursor.offset == wanted.offset && cursor.parser_state == wanted.parser_state,
    )
}

fn persist_evidence_blobs(
    vault: &EvidenceVault,
    evidence: &[EvidenceWrite],
) -> Result<(), StoreError> {
    for item in evidence {
        let owner = match &item.owner {
            EvidenceOwner::Event(id) => EvidenceOwnerRef::Event(id),
            EvidenceOwner::Work(id) => EvidenceOwnerRef::Work(id),
        };
        vault.put(
            &item.evidence_id,
            EvidenceContext {
                project_id: &item.project_id,
                owner,
            },
            &item.plaintext,
        )?;
    }
    Ok(())
}

fn insert_observations(
    transaction: &Transaction<'_>,
    chunk: &IngestionChunk,
) -> Result<(), StoreError> {
    for item in &chunk.observations {
        let values = ObservationValues::new(chunk, item)?;
        let changed = transaction.execute(
            "INSERT OR IGNORE INTO source_observations(
                 observation_id, source_id, generation, byte_start, byte_end,
                 record_hash, native_record_type, decode_status,
                 schema_fingerprint, observed_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                values.observation_id,
                values.source_id,
                values.generation,
                values.byte_start,
                values.byte_end,
                values.record_hash,
                values.native_record_type,
                values.decode_status,
                values.schema_fingerprint,
                values.observed_at,
            ],
        )?;
        if changed == 0 && !observation_exact(transaction, &values)? {
            return Err(StoreError::ImmutableConflict);
        }
    }
    Ok(())
}

struct ObservationValues<'a> {
    observation_id: &'a str,
    source_id: &'a str,
    generation: i64,
    byte_start: i64,
    byte_end: i64,
    record_hash: &'a str,
    native_record_type: &'a str,
    decode_status: &'static str,
    schema_fingerprint: &'a str,
    observed_at: String,
}

impl<'a> ObservationValues<'a> {
    fn new(chunk: &'a IngestionChunk, item: &'a SourceObservation) -> Result<Self, StoreError> {
        Ok(Self {
            observation_id: item.observation_id(),
            source_id: &chunk.expected_cursor.source_id,
            generation: to_i64(chunk.expected_cursor.generation)?,
            byte_start: to_i64(item.range().start())?,
            byte_end: to_i64(item.range().end())?,
            record_hash: item.source().record_hash(),
            native_record_type: item.source().native_record_type(),
            decode_status: decode_status(item.status()),
            schema_fingerprint: item.schema_fingerprint(),
            observed_at: format_timestamp(item.observed_at())?,
        })
    }
}

fn observation_exact(
    transaction: &Transaction<'_>,
    value: &ObservationValues<'_>,
) -> Result<bool, StoreError> {
    exists(
        transaction,
        "SELECT EXISTS(
             SELECT 1 FROM source_observations
             WHERE observation_id = ?1 AND source_id = ?2 AND generation = ?3
               AND byte_start = ?4 AND byte_end = ?5 AND record_hash = ?6
               AND native_record_type = ?7 AND decode_status = ?8
               AND schema_fingerprint = ?9 AND observed_at = ?10
         )",
        params![
            value.observation_id,
            value.source_id,
            value.generation,
            value.byte_start,
            value.byte_end,
            value.record_hash,
            value.native_record_type,
            value.decode_status,
            value.schema_fingerprint,
            value.observed_at,
        ],
    )
}

fn insert_events(
    transaction: &Transaction<'_>,
    events: &[ActivityEventV1],
) -> Result<usize, StoreError> {
    let mut inserted = 0_usize;
    for item in events {
        let values = EventValues::new(item)?;
        let changed = transaction.execute(
            "INSERT OR IGNORE INTO activity_events(
                 event_id, semantic_key, schema_version, occurred_at, observed_at,
                 project_id, session_id, turn_id, actor, correlation_id,
                 causation_id, source_json, payload_json, privacy
             ) VALUES (
                 ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14
             )",
            params![
                values.event_id,
                values.semantic_key,
                values.schema_version,
                values.occurred_at,
                values.observed_at,
                values.project_id,
                values.session_id,
                values.turn_id,
                values.actor,
                values.correlation_id,
                values.causation_id,
                values.source_json,
                values.payload_json,
                values.privacy,
            ],
        )?;
        if changed == 0 && !event_exact(transaction, &values)? {
            return Err(StoreError::ImmutableConflict);
        }
        inserted = inserted
            .checked_add(changed)
            .ok_or(StoreError::InvalidBatch)?;
    }
    Ok(inserted)
}

struct EventValues<'a> {
    event_id: &'a str,
    semantic_key: &'a str,
    schema_version: i64,
    occurred_at: String,
    observed_at: String,
    project_id: &'a str,
    session_id: &'a str,
    turn_id: Option<&'a str>,
    actor: &'static str,
    correlation_id: Option<&'a str>,
    causation_id: Option<&'a str>,
    source_json: String,
    payload_json: String,
    privacy: &'static str,
}

impl<'a> EventValues<'a> {
    fn new(item: &'a ActivityEventV1) -> Result<Self, StoreError> {
        Ok(Self {
            event_id: item.event_id().as_str(),
            semantic_key: item.semantic_key().as_str(),
            schema_version: i64::from(item.schema_version()),
            occurred_at: format_timestamp(item.occurred_at())?,
            observed_at: format_timestamp(item.observed_at())?,
            project_id: item.project_id().as_str(),
            session_id: item.session_id().as_str(),
            turn_id: item.turn_id(),
            actor: actor(item.actor()),
            correlation_id: item.correlation_id(),
            causation_id: item.causation_id(),
            source_json: serde_json::to_string(item.source())?,
            payload_json: serde_json::to_string(item.payload())?,
            privacy: privacy(item.privacy()),
        })
    }
}

fn event_exact(transaction: &Transaction<'_>, value: &EventValues<'_>) -> Result<bool, StoreError> {
    exists(
        transaction,
        "SELECT EXISTS(
             SELECT 1 FROM activity_events
             WHERE event_id = ?1 AND semantic_key = ?2 AND schema_version = ?3
               AND occurred_at = ?4 AND observed_at = ?5 AND project_id = ?6
               AND session_id = ?7 AND turn_id IS ?8 AND actor = ?9
               AND correlation_id IS ?10 AND causation_id IS ?11
               AND source_json = ?12 AND payload_json = ?13 AND privacy = ?14
         )",
        params![
            value.event_id,
            value.semantic_key,
            value.schema_version,
            value.occurred_at,
            value.observed_at,
            value.project_id,
            value.session_id,
            value.turn_id,
            value.actor,
            value.correlation_id,
            value.causation_id,
            value.source_json,
            value.payload_json,
            value.privacy,
        ],
    )
}

fn insert_evidence_objects(
    transaction: &Transaction<'_>,
    evidence: &[EvidenceWrite],
) -> Result<(), StoreError> {
    for item in evidence {
        let values = EvidenceValues::new(item)?;
        let changed = transaction.execute(
            "INSERT OR IGNORE INTO evidence_objects(
                 evidence_id, project_id, owner_kind, owner_id, content_hash,
                 media_type, privacy, byte_length, redacted_excerpt,
                 disclosure_class, blob_state, created_at, expires_at, retired_at
             ) VALUES (
                 ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, 'available',
                 strftime('%Y-%m-%dT%H:%M:%fZ', 'now'), ?11, NULL
             )",
            params![
                values.evidence_id,
                values.project_id,
                values.owner_kind,
                values.owner_id,
                values.content_hash,
                values.media_type,
                values.privacy,
                values.byte_length,
                values.redacted_excerpt,
                values.disclosure_class,
                values.expires_at,
            ],
        )?;
        if changed == 0 && !evidence_exact(transaction, &values)? {
            return Err(StoreError::ImmutableConflict);
        }
    }
    Ok(())
}

struct EvidenceValues<'a> {
    evidence_id: &'a str,
    project_id: &'a str,
    owner_kind: &'static str,
    owner_id: &'a str,
    content_hash: &'a str,
    media_type: &'a str,
    privacy: &'static str,
    byte_length: i64,
    redacted_excerpt: &'a str,
    disclosure_class: &'static str,
    expires_at: Option<String>,
}

impl<'a> EvidenceValues<'a> {
    fn new(item: &'a EvidenceWrite) -> Result<Self, StoreError> {
        Ok(Self {
            evidence_id: item.evidence_id.as_str(),
            project_id: item.project_id.as_str(),
            owner_kind: owner_kind(&item.owner),
            owner_id: owner_id(&item.owner),
            content_hash: &item.content_hash,
            media_type: &item.media_type,
            privacy: privacy(item.privacy),
            byte_length: i64::try_from(item.plaintext.len())
                .map_err(|_| StoreError::InvalidBatch)?,
            redacted_excerpt: &item.redacted_excerpt,
            disclosure_class: disclosure(item.disclosure_class),
            expires_at: item.expires_at.map(format_timestamp).transpose()?,
        })
    }
}

fn evidence_exact(
    transaction: &Transaction<'_>,
    value: &EvidenceValues<'_>,
) -> Result<bool, StoreError> {
    exists(
        transaction,
        "SELECT EXISTS(
             SELECT 1 FROM evidence_objects
             WHERE evidence_id = ?1 AND project_id = ?2 AND owner_kind = ?3
               AND owner_id = ?4 AND content_hash = ?5 AND media_type = ?6
               AND privacy = ?7 AND byte_length = ?8 AND redacted_excerpt = ?9
               AND disclosure_class = ?10 AND blob_state = 'available'
               AND expires_at IS ?11 AND retired_at IS NULL
         )",
        params![
            value.evidence_id,
            value.project_id,
            value.owner_kind,
            value.owner_id,
            value.content_hash,
            value.media_type,
            value.privacy,
            value.byte_length,
            value.redacted_excerpt,
            value.disclosure_class,
            value.expires_at,
        ],
    )
}

fn insert_evidence_links(
    transaction: &Transaction<'_>,
    links: &[EvidenceLink],
) -> Result<(), StoreError> {
    for item in links {
        transaction.execute(
            "INSERT OR IGNORE INTO event_evidence(event_id, observation_id, evidence_id)
             VALUES (?1, ?2, ?3)",
            params![item.event_id, item.observation_id, item.evidence_id],
        )?;
    }
    Ok(())
}

fn insert_content_refs(
    transaction: &Transaction<'_>,
    content_refs: &[ContentRefWrite],
) -> Result<(), StoreError> {
    for item in content_refs {
        let values = ContentValues::new(item)?;
        let changed = transaction.execute(
            "INSERT OR IGNORE INTO content_refs(
                 content_ref_id, project_id, content_hash, byte_length, media_type,
                 local_locator, redacted_excerpt, truncated, privacy, disclosure_class
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                values.content_ref_id,
                values.project_id,
                values.content_hash,
                values.byte_length,
                values.media_type,
                values.local_locator,
                values.redacted_excerpt,
                values.truncated,
                values.privacy,
                values.disclosure_class,
            ],
        )?;
        if changed == 0 && !content_exact(transaction, &values)? {
            return Err(StoreError::ImmutableConflict);
        }
    }
    Ok(())
}

struct ContentValues<'a> {
    content_ref_id: &'a str,
    project_id: &'a str,
    content_hash: &'a str,
    byte_length: i64,
    media_type: &'a str,
    local_locator: Option<Vec<u8>>,
    redacted_excerpt: Option<&'a str>,
    truncated: i64,
    privacy: &'static str,
    disclosure_class: &'static str,
}

impl<'a> ContentValues<'a> {
    fn new(item: &'a ContentRefWrite) -> Result<Self, StoreError> {
        Ok(Self {
            content_ref_id: &item.content_ref_id,
            project_id: item.project_id.as_str(),
            content_hash: item.content.hash(),
            byte_length: to_i64(item.content.byte_length())?,
            media_type: item.content.media_type(),
            local_locator: item
                .content
                .local_locator()
                .map(serde_json::to_vec)
                .transpose()?,
            redacted_excerpt: item.content.redacted_excerpt(),
            truncated: i64::from(item.content.is_truncated()),
            privacy: privacy(item.privacy),
            disclosure_class: disclosure(item.content.disclosure_class()),
        })
    }
}

fn content_exact(
    transaction: &Transaction<'_>,
    value: &ContentValues<'_>,
) -> Result<bool, StoreError> {
    exists(
        transaction,
        "SELECT EXISTS(
             SELECT 1 FROM content_refs
             WHERE content_ref_id = ?1 AND project_id = ?2 AND content_hash = ?3
               AND byte_length = ?4 AND media_type = ?5 AND local_locator IS ?6
               AND redacted_excerpt IS ?7 AND truncated = ?8 AND privacy = ?9
               AND disclosure_class = ?10
         )",
        params![
            value.content_ref_id,
            value.project_id,
            value.content_hash,
            value.byte_length,
            value.media_type,
            value.local_locator,
            value.redacted_excerpt,
            value.truncated,
            value.privacy,
            value.disclosure_class,
        ],
    )
}

fn upsert_schema_fingerprints(
    transaction: &Transaction<'_>,
    fingerprints: &[SchemaFingerprintUpdate],
) -> Result<(), StoreError> {
    for item in fingerprints {
        let observed_at = format_timestamp(item.observed_at)?;
        let current: Option<i64> = transaction
            .query_row(
                "SELECT count FROM schema_fingerprints
                 WHERE provider = ?1 AND format = ?2 AND fingerprint = ?3",
                params![item.provider, item.format, item.fingerprint],
                |row| row.get(0),
            )
            .optional()?;
        if let Some(current) = current {
            if current < 0 {
                return Err(StoreError::InvalidBatch);
            }
            let next = current.checked_add(1).ok_or(StoreError::InvalidBatch)?;
            transaction.execute(
                "UPDATE schema_fingerprints
                 SET last_seen_at = ?4, count = ?5
                 WHERE provider = ?1 AND format = ?2 AND fingerprint = ?3",
                params![
                    item.provider,
                    item.format,
                    item.fingerprint,
                    observed_at,
                    next
                ],
            )?;
        } else {
            transaction.execute(
                "INSERT INTO schema_fingerprints(
                     provider, format, fingerprint, first_seen_at, last_seen_at, count
                 ) VALUES (?1, ?2, ?3, ?4, ?4, 1)",
                params![item.provider, item.format, item.fingerprint, observed_at],
            )?;
        }
    }
    Ok(())
}

fn insert_faults(
    transaction: &Transaction<'_>,
    faults: &[IngestionFault],
) -> Result<(), StoreError> {
    for item in faults {
        let changed = transaction.execute(
            "INSERT OR IGNORE INTO ingestion_faults(
                 fault_id, source_id, generation, byte_start, byte_end,
                 class, bounded_detail, created_at
             ) VALUES (
                 ?1, ?2, ?3, ?4, ?5, ?6, ?7,
                 strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
             )",
            params![
                item.fault_id,
                item.source_id,
                to_i64(item.generation)?,
                to_i64(item.byte_start)?,
                to_i64(item.byte_end)?,
                item.class,
                item.bounded_detail,
            ],
        )?;
        if changed == 0 && !fault_exact(transaction, item)? {
            return Err(StoreError::ImmutableConflict);
        }
    }
    Ok(())
}

fn fault_exact(transaction: &Transaction<'_>, item: &IngestionFault) -> Result<bool, StoreError> {
    exists(
        transaction,
        "SELECT EXISTS(
             SELECT 1 FROM ingestion_faults
             WHERE fault_id = ?1 AND source_id = ?2 AND generation = ?3
               AND byte_start = ?4 AND byte_end = ?5 AND class = ?6
               AND bounded_detail = ?7
         )",
        params![
            item.fault_id,
            item.source_id,
            to_i64(item.generation)?,
            to_i64(item.byte_start)?,
            to_i64(item.byte_end)?,
            item.class,
            item.bounded_detail,
        ],
    )
}

fn verify_retry(transaction: &Transaction<'_>, chunk: &IngestionChunk) -> Result<(), StoreError> {
    for item in &chunk.observations {
        if !observation_exact(transaction, &ObservationValues::new(chunk, item)?)? {
            return Err(StoreError::ImmutableConflict);
        }
    }
    for item in &chunk.events {
        if !event_exact(transaction, &EventValues::new(item)?)? {
            return Err(StoreError::ImmutableConflict);
        }
    }
    for item in &chunk.evidence {
        if !evidence_exact(transaction, &EvidenceValues::new(item)?)? {
            return Err(StoreError::ImmutableConflict);
        }
    }
    for item in &chunk.evidence_links {
        if !exists(
            transaction,
            "SELECT EXISTS(
                 SELECT 1 FROM event_evidence
                 WHERE event_id = ?1 AND observation_id = ?2 AND evidence_id = ?3
             )",
            params![item.event_id, item.observation_id, item.evidence_id],
        )? {
            return Err(StoreError::ImmutableConflict);
        }
    }
    for item in &chunk.content_refs {
        if !content_exact(transaction, &ContentValues::new(item)?)? {
            return Err(StoreError::ImmutableConflict);
        }
    }
    for (index, item) in chunk.fingerprints.iter().enumerate() {
        if chunk.fingerprints[index + 1..].iter().any(|later| {
            later.provider == item.provider
                && later.format == item.format
                && later.fingerprint == item.fingerprint
        }) {
            continue;
        }
        if !exists(
            transaction,
            "SELECT EXISTS(
                 SELECT 1 FROM schema_fingerprints
                 WHERE provider = ?1 AND format = ?2 AND fingerprint = ?3
                   AND count > 0
             )",
            params![item.provider, item.format, item.fingerprint],
        )? {
            return Err(StoreError::ImmutableConflict);
        }
    }
    for item in &chunk.faults {
        if !fault_exact(transaction, item)? {
            return Err(StoreError::ImmutableConflict);
        }
    }
    Ok(())
}

fn exists<P: rusqlite::Params>(
    connection: &rusqlite::Connection,
    sql: &str,
    parameters: P,
) -> Result<bool, StoreError> {
    Ok(connection.query_row(sql, parameters, |row| row.get(0))?)
}

/// Computes the project-scoped stable ID for a retained content reference.
///
/// # Errors
///
/// Returns [`StoreError::InvalidBatch`] if length conversion or locator
/// serialization fails.
pub fn stable_content_ref_id(
    project_id: &ProjectId,
    content: &ContentRef,
) -> Result<String, StoreError> {
    let locator = serde_json::to_vec(&content.local_locator())?;
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"content_ref");
    hash_part(&mut hasher, project_id.as_str().as_bytes())?;
    hash_part(&mut hasher, content.hash().as_bytes())?;
    hash_part(&mut hasher, &locator)?;
    Ok(format!("cref_{}", &hasher.finalize().to_hex()[..24]))
}

fn hash_part(hasher: &mut blake3::Hasher, value: &[u8]) -> Result<(), StoreError> {
    let length = u64::try_from(value.len()).map_err(|_| StoreError::InvalidBatch)?;
    hasher.update(&length.to_le_bytes());
    hasher.update(value);
    Ok(())
}

fn add_len(total: &mut usize, value: usize) -> Result<(), StoreError> {
    *total = total.checked_add(value).ok_or(StoreError::InvalidBatch)?;
    Ok(())
}

fn bounded_metadata(value: &str) -> bool {
    !value.is_empty() && value.len() <= 128
}

fn bounded_identifier(value: &str) -> bool {
    bounded_metadata(value) && value.bytes().all(|byte| byte.is_ascii_graphic())
}

fn to_i64(value: u64) -> Result<i64, StoreError> {
    i64::try_from(value).map_err(|_| StoreError::InvalidBatch)
}

fn format_timestamp(value: OffsetDateTime) -> Result<String, StoreError> {
    value.format(&Rfc3339).map_err(|_| StoreError::InvalidBatch)
}

fn owner_kind(owner: &EvidenceOwner) -> &'static str {
    match owner {
        EvidenceOwner::Event(_) => "event",
        EvidenceOwner::Work(_) => "work",
    }
}

fn owner_id(owner: &EvidenceOwner) -> &str {
    match owner {
        EvidenceOwner::Event(id) => id.as_str(),
        EvidenceOwner::Work(id) => id.as_str(),
    }
}

fn privacy(value: PrivacyLabel) -> &'static str {
    match value {
        PrivacyLabel::RestrictedLocal => "restricted_local",
        PrivacyLabel::PrivateLocal => "private_local",
        PrivacyLabel::DerivedLocal => "derived_local",
        PrivacyLabel::SyncEligible => "sync_eligible",
    }
}

fn disclosure(value: DisclosureClass) -> &'static str {
    match value {
        DisclosureClass::HumanIntent => "human_intent",
        DisclosureClass::AgentStatement => "agent_statement",
        DisclosureClass::ObservedState => "observed_state",
        DisclosureClass::ToolResult => "tool_result",
        DisclosureClass::Reasoning => "reasoning",
        DisclosureClass::SystemInstruction => "system_instruction",
        DisclosureClass::DeveloperInstruction => "developer_instruction",
        DisclosureClass::DerivedText => "derived_text",
    }
}

fn actor(value: agbox_core::Actor) -> &'static str {
    match value {
        agbox_core::Actor::Human => "human",
        agbox_core::Actor::Agent => "agent",
        agbox_core::Actor::Tool => "tool",
        agbox_core::Actor::System => "system",
    }
}

fn decode_status(value: agbox_core::DecodeStatus) -> &'static str {
    match value {
        agbox_core::DecodeStatus::Known => "known",
        agbox_core::DecodeStatus::UnknownType => "unknown_type",
        agbox_core::DecodeStatus::Malformed => "malformed",
        agbox_core::DecodeStatus::Oversized => "oversized",
    }
}

#[cfg(feature = "test-support")]
impl IngestionChunk {
    #[must_use]
    pub fn fixture(
        source_id: &str,
        generation: u64,
        expected_offset: u64,
        next_offset: u64,
        event_count: usize,
    ) -> Self {
        use agbox_core::{
            ActivityEventDraft, ByteRange, DecodeStatus, EventId, SemanticKey, SourceIdentity,
            SourceObservationDraft, SourceRef, SourceRefDraft,
        };

        let source = SourceRef::new(SourceRefDraft {
            provider: Provider::Codex,
            format: "jsonl".into(),
            native_session_id: "native-session-fixture".into(),
            native_record_type: "message".into(),
            native_record_id: Some("message-fixture".into()),
            source_generation: generation,
            byte_offset: expected_offset,
            ordinal: Some(1),
            record_hash: format!("b3:fixture-record-{expected_offset}-{next_offset}"),
            decoder_version: "fixture-v1".into(),
        })
        .unwrap_or_else(|_| unreachable!("fixed fixture source is valid"));
        let observation = SourceObservation::new(SourceObservationDraft {
            observation_id: format!("obs_{source_id}_{generation}_{expected_offset}_{next_offset}"),
            source: source.clone(),
            range: ByteRange::new(expected_offset, next_offset)
                .unwrap_or_else(|_| unreachable!("fixed fixture range is valid")),
            observed_at: OffsetDateTime::UNIX_EPOCH,
            status: DecodeStatus::Known,
            bounded_record: None,
            schema_fingerprint: "fixture-fingerprint".into(),
        })
        .unwrap_or_else(|_| unreachable!("fixed fixture observation is valid"));

        let events = (0..event_count)
            .map(|index| {
                let identity = SourceIdentity {
                    provider: Provider::Codex,
                    source_id: source_id.into(),
                    generation,
                    byte_offset: expected_offset,
                    record_hash: source.record_hash().into(),
                };
                let mut draft: ActivityEventDraft = ActivityEventV1::fixture_message_draft();
                draft.event_id = EventId::from_source(
                    &identity,
                    u32::try_from(index)
                        .unwrap_or_else(|_| unreachable!("fixture count is bounded")),
                );
                draft.semantic_key = SemanticKey::from_native(
                    Provider::Codex,
                    "native-session-fixture",
                    "message",
                    &format!("{expected_offset}-{next_offset}-{index}"),
                );
                draft.source = source.clone();
                ActivityEventV1::new(draft)
                    .unwrap_or_else(|_| unreachable!("fixed fixture event is valid"))
            })
            .collect();

        Self {
            expected_cursor: CursorState {
                source_id: source_id.into(),
                generation,
                offset: expected_offset,
                parser_state: Vec::new(),
            },
            next_cursor: CursorState {
                source_id: source_id.into(),
                generation,
                offset: next_offset,
                parser_state: Vec::new(),
            },
            observations: vec![observation],
            events,
            evidence: Vec::new(),
            evidence_links: Vec::new(),
            content_refs: Vec::new(),
            fingerprints: Vec::new(),
            faults: Vec::new(),
        }
    }
}
