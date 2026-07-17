PRAGMA foreign_keys = ON;

CREATE TABLE schema_migrations (
    version INTEGER PRIMARY KEY,
    applied_at TEXT NOT NULL
) STRICT;

CREATE TABLE projects (
    project_id TEXT PRIMARY KEY,
    repository_identity TEXT NOT NULL,
    encrypted_root_path BLOB NOT NULL,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
) STRICT;

CREATE TABLE sources (
    source_id TEXT PRIMARY KEY,
    project_id TEXT NOT NULL REFERENCES projects(project_id),
    provider TEXT NOT NULL CHECK (provider IN ('claude', 'codex')),
    root_class TEXT NOT NULL CHECK (root_class IN ('active', 'archive')),
    encrypted_path BLOB NOT NULL,
    file_identity TEXT NOT NULL,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
) STRICT;

CREATE TABLE source_generations (
    source_id TEXT NOT NULL REFERENCES sources(source_id),
    generation INTEGER NOT NULL CHECK (generation > 0),
    size_bytes INTEGER NOT NULL CHECK (size_bytes >= 0),
    mtime TEXT NOT NULL,
    session_time TEXT,
    schema_fingerprint TEXT,
    status TEXT NOT NULL,
    PRIMARY KEY (source_id, generation)
) STRICT;

CREATE TABLE source_cursors (
    source_id TEXT NOT NULL,
    generation INTEGER NOT NULL,
    cursor_offset INTEGER NOT NULL CHECK (cursor_offset >= 0),
    parser_state BLOB NOT NULL,
    updated_at TEXT NOT NULL,
    PRIMARY KEY (source_id, generation),
    FOREIGN KEY (source_id, generation)
        REFERENCES source_generations(source_id, generation)
) STRICT;

CREATE TABLE source_observations (
    observation_id TEXT PRIMARY KEY,
    source_id TEXT NOT NULL,
    generation INTEGER NOT NULL,
    byte_start INTEGER NOT NULL,
    byte_end INTEGER NOT NULL,
    record_hash TEXT NOT NULL,
    native_record_type TEXT NOT NULL,
    decode_status TEXT NOT NULL,
    schema_fingerprint TEXT NOT NULL,
    observed_at TEXT NOT NULL,
    UNIQUE (source_id, generation, byte_start, record_hash),
    FOREIGN KEY (source_id, generation)
        REFERENCES source_generations(source_id, generation)
) STRICT;

CREATE TABLE activity_events (
    event_seq INTEGER PRIMARY KEY AUTOINCREMENT,
    event_id TEXT NOT NULL UNIQUE,
    semantic_key TEXT NOT NULL,
    schema_version INTEGER NOT NULL CHECK (schema_version = 1),
    occurred_at TEXT NOT NULL,
    observed_at TEXT NOT NULL,
    project_id TEXT NOT NULL REFERENCES projects(project_id),
    session_id TEXT NOT NULL,
    turn_id TEXT,
    actor TEXT NOT NULL,
    correlation_id TEXT,
    causation_id TEXT,
    source_json TEXT NOT NULL,
    payload_json TEXT NOT NULL,
    privacy TEXT NOT NULL,
    UNIQUE (semantic_key, event_id)
) STRICT;

CREATE INDEX activity_events_project_time
    ON activity_events(project_id, occurred_at);
CREATE INDEX activity_events_semantic
    ON activity_events(semantic_key);

CREATE TABLE event_evidence (
    event_id TEXT NOT NULL REFERENCES activity_events(event_id),
    observation_id TEXT NOT NULL REFERENCES source_observations(observation_id),
    evidence_id TEXT NOT NULL REFERENCES evidence_objects(evidence_id),
    PRIMARY KEY (event_id, observation_id, evidence_id)
) STRICT;

CREATE TABLE content_refs (
    content_ref_id TEXT PRIMARY KEY,
    project_id TEXT NOT NULL REFERENCES projects(project_id),
    content_hash TEXT NOT NULL,
    byte_length INTEGER NOT NULL,
    media_type TEXT NOT NULL,
    local_locator BLOB,
    redacted_excerpt TEXT,
    truncated INTEGER NOT NULL CHECK (truncated IN (0, 1)),
    privacy TEXT NOT NULL,
    disclosure_class TEXT NOT NULL CHECK (disclosure_class IN (
        'human_intent',
        'agent_statement',
        'observed_state',
        'tool_result',
        'reasoning',
        'system_instruction',
        'developer_instruction',
        'derived_text'
    ))
) STRICT;

CREATE TABLE schema_fingerprints (
    provider TEXT NOT NULL,
    format TEXT NOT NULL,
    fingerprint TEXT NOT NULL,
    first_seen_at TEXT NOT NULL,
    last_seen_at TEXT NOT NULL,
    count INTEGER NOT NULL,
    PRIMARY KEY (provider, format, fingerprint)
) STRICT;

CREATE TABLE ingestion_faults (
    fault_id TEXT PRIMARY KEY,
    source_id TEXT NOT NULL,
    generation INTEGER NOT NULL,
    byte_start INTEGER NOT NULL,
    byte_end INTEGER NOT NULL,
    class TEXT NOT NULL,
    bounded_detail TEXT NOT NULL,
    created_at TEXT NOT NULL
) STRICT;

CREATE TABLE agent_runs (
    run_id TEXT PRIMARY KEY,
    project_id TEXT NOT NULL REFERENCES projects(project_id),
    provider TEXT NOT NULL,
    native_session_id TEXT NOT NULL,
    branch_hash TEXT,
    started_at TEXT NOT NULL,
    finished_at TEXT,
    status TEXT NOT NULL
) STRICT;

CREATE TABLE work_items (
    work_id TEXT PRIMARY KEY,
    project_id TEXT NOT NULL REFERENCES projects(project_id),
    status TEXT NOT NULL,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
) STRICT;

CREATE INDEX work_items_project_status_recent
    ON work_items(project_id, status, updated_at DESC);

CREATE TABLE evidence_objects (
    evidence_id TEXT PRIMARY KEY,
    project_id TEXT NOT NULL REFERENCES projects(project_id),
    owner_kind TEXT NOT NULL CHECK (owner_kind IN ('event', 'work')),
    owner_id TEXT NOT NULL,
    content_hash TEXT NOT NULL,
    media_type TEXT NOT NULL,
    privacy TEXT NOT NULL,
    byte_length INTEGER NOT NULL CHECK (byte_length >= 0),
    redacted_excerpt TEXT NOT NULL
        CHECK (length(CAST(redacted_excerpt AS BLOB)) <= 2048),
    disclosure_class TEXT NOT NULL CHECK (disclosure_class IN (
        'human_intent',
        'agent_statement',
        'observed_state',
        'tool_result',
        'reasoning',
        'system_instruction',
        'developer_instruction',
        'derived_text'
    )),
    blob_state TEXT NOT NULL
        CHECK (blob_state IN ('available', 'expired', 'delete_pending')),
    created_at TEXT NOT NULL,
    expires_at TEXT,
    retired_at TEXT
) STRICT;

CREATE INDEX evidence_objects_project_owner
    ON evidence_objects(project_id, owner_kind, owner_id);

CREATE TABLE work_assertions (
    assertion_id TEXT PRIMARY KEY,
    work_id TEXT NOT NULL REFERENCES work_items(work_id),
    field TEXT NOT NULL,
    value TEXT NOT NULL,
    authority TEXT NOT NULL,
    privacy TEXT NOT NULL,
    disclosure_class TEXT NOT NULL CHECK (disclosure_class IN (
        'human_intent',
        'agent_statement',
        'observed_state',
        'tool_result',
        'reasoning',
        'system_instruction',
        'developer_instruction',
        'derived_text'
    )),
    confidence_basis_points INTEGER NOT NULL,
    created_at TEXT NOT NULL,
    supersedes_assertion_id TEXT
) STRICT;

CREATE TABLE work_edges (
    from_work_id TEXT NOT NULL REFERENCES work_items(work_id),
    to_work_id TEXT NOT NULL REFERENCES work_items(work_id),
    kind TEXT NOT NULL,
    created_at TEXT NOT NULL,
    PRIMARY KEY (from_work_id, to_work_id, kind)
) STRICT;

CREATE TABLE artifacts (
    artifact_id TEXT PRIMARY KEY,
    work_id TEXT NOT NULL REFERENCES work_items(work_id),
    path_hash TEXT NOT NULL,
    encrypted_path BLOB NOT NULL,
    content_hash TEXT,
    operation TEXT NOT NULL,
    observed_at TEXT NOT NULL
) STRICT;

CREATE INDEX artifacts_work_path
    ON artifacts(path_hash, work_id);

CREATE TABLE work_evidence (
    work_id TEXT NOT NULL REFERENCES work_items(work_id),
    assertion_id TEXT,
    event_id TEXT NOT NULL REFERENCES activity_events(event_id),
    evidence_id TEXT NOT NULL REFERENCES evidence_objects(evidence_id),
    PRIMARY KEY (work_id, event_id, evidence_id)
) STRICT;

CREATE TABLE work_contract_revisions (
    contract_id TEXT NOT NULL,
    work_id TEXT NOT NULL REFERENCES work_items(work_id),
    revision INTEGER NOT NULL CHECK (revision > 0),
    contract_json TEXT NOT NULL,
    extractor_version TEXT NOT NULL,
    created_at TEXT NOT NULL,
    PRIMARY KEY (contract_id, revision),
    UNIQUE (work_id, revision)
) STRICT;

CREATE TABLE extractor_runs (
    extractor_run_id TEXT PRIMARY KEY,
    work_id TEXT NOT NULL REFERENCES work_items(work_id),
    extractor_version TEXT NOT NULL,
    input_event_watermark TEXT NOT NULL,
    status TEXT NOT NULL,
    bounded_error TEXT,
    created_at TEXT NOT NULL,
    finished_at TEXT
) STRICT;

CREATE TABLE handoff_reads (
    handoff_read_id TEXT PRIMARY KEY,
    work_id TEXT NOT NULL REFERENCES work_items(work_id),
    contract_id TEXT NOT NULL,
    revision INTEGER NOT NULL,
    provider TEXT NOT NULL,
    project_id TEXT NOT NULL,
    read_at TEXT NOT NULL
) STRICT;

CREATE TABLE audit_events (
    audit_id TEXT PRIMARY KEY,
    kind TEXT NOT NULL,
    project_id TEXT,
    work_id TEXT,
    actor TEXT NOT NULL,
    detail_json TEXT NOT NULL,
    created_at TEXT NOT NULL
) STRICT;

CREATE TABLE evidence_delete_queue (
    deletion_job_id TEXT NOT NULL,
    evidence_id TEXT NOT NULL,
    project_hash TEXT NOT NULL,
    attempts INTEGER NOT NULL DEFAULT 0 CHECK (attempts >= 0),
    state TEXT NOT NULL CHECK (state IN ('pending', 'failed')),
    created_at TEXT NOT NULL,
    last_error_code TEXT,
    PRIMARY KEY (deletion_job_id, evidence_id)
) STRICT;

CREATE TABLE reducer_watermarks (
    reducer_name TEXT PRIMARY KEY,
    through_event_seq INTEGER NOT NULL CHECK (through_event_seq >= 0),
    through_event_id TEXT NOT NULL,
    updated_at TEXT NOT NULL
) STRICT;

CREATE TABLE action_facts (
    project_id TEXT NOT NULL REFERENCES projects(project_id),
    session_id TEXT NOT NULL,
    native_action_id TEXT NOT NULL,
    request_event_id TEXT NOT NULL REFERENCES activity_events(event_id),
    finish_event_id TEXT REFERENCES activity_events(event_id),
    tool_name TEXT NOT NULL,
    input_hash TEXT NOT NULL,
    redacted_command TEXT,
    succeeded INTEGER CHECK (succeeded IN (0, 1)),
    PRIMARY KEY (project_id, session_id, native_action_id, request_event_id)
) STRICT;

CREATE INDEX action_facts_project_input
    ON action_facts(project_id, input_hash);

CREATE TABLE verification_facts (
    verification_id TEXT PRIMARY KEY,
    project_id TEXT NOT NULL REFERENCES projects(project_id),
    work_id TEXT,
    session_id TEXT NOT NULL,
    native_action_id TEXT NOT NULL,
    succeeded INTEGER NOT NULL CHECK (succeeded IN (0, 1)),
    basis TEXT NOT NULL,
    event_id TEXT NOT NULL REFERENCES activity_events(event_id),
    observed_at TEXT NOT NULL
) STRICT;

CREATE VIRTUAL TABLE work_search USING fts5(
    work_id UNINDEXED,
    project_id UNINDEXED,
    objective,
    summary,
    completed_steps,
    next_actions,
    blockers,
    artifacts,
    verification,
    tokenize = 'unicode61'
);
