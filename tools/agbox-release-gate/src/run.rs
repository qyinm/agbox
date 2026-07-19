//! Isolated, process-local execution harness for the release gate.
//!
//! The harness only creates a sanitized temporary corpus. It never discovers
//! the developer's real agent histories or reuses a production state database.

use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    os::unix::{fs::MetadataExt, prelude::OsStrExt},
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant},
};

use agbox_adapters::{DiscoveredSource, RootClass};
use agbox_core::{ProjectId, Provider, api::AppRequest};
use agbox_ingest::{
    CoordinatorSource, IngestionCoordinator, ProjectResolver, SourceKey, WorkPriority,
};
use agbox_service::{
    AppClient, ApplicationService, IpcAppClient,
    ipc::{DeferredRequestHandler, IPC_PROTOCOL_VERSION, IpcHello, LocalIpcServer, WireActor},
};
use agbox_store::{CryptoError, EvidenceVault, KeyProvider, SourceRegistration, StoreRuntime};
use time::OffsetDateTime;
use tokio_util::sync::CancellationToken;
use zeroize::Zeroizing;

use crate::{
    GateReport, ReleaseArtifact, Thresholds,
    corpus::{CorpusManifest, CorpusSpec, manifest},
    metrics::{Samples, sustained_rss_growth},
    process::{ProcessSampler, binary_sha256},
    recovery::RecoveryCounts,
};

const APPEND_RECORDS: usize = 3_000;
const APPEND_PER_SECOND: usize = 50;
const MCP_WARMUP_CALLS: usize = 20;
const MCP_MEASURED_CALLS: usize = 1_000;
const MAX_RSS_SAMPLES: usize = 24 * 60 * 60 + 32;
const LIVE_CODEX_SOURCES: [usize; 4] = [0, 2, 4, 6];
const APPEND_BATCH_SIZE: usize = 4;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Profile {
    CiSmoke,
    Release,
}

impl Profile {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::CiSmoke => "ci-smoke",
            Self::Release => "release",
        }
    }
}

#[derive(Clone, Debug)]
pub struct RunOptions {
    pub profile: Profile,
    pub duration: Duration,
    pub output_directory: PathBuf,
    pub commit_sha: String,
    pub target: String,
    pub binary: PathBuf,
}

#[derive(Debug)]
struct FixedKeyProvider;

impl KeyProvider for FixedKeyProvider {
    fn master_key(&self) -> Result<Zeroizing<[u8; 32]>, CryptoError> {
        Ok(Zeroizing::new([0xA6; 32]))
    }
}

#[derive(Clone)]
struct RegisteredSource {
    key: SourceKey,
    source: CoordinatorSource,
}

#[derive(serde::Serialize)]
struct RunMetadata<'a> {
    profile: &'a str,
    duration_seconds: u64,
    target: &'a str,
    rust_version: &'static str,
    os: String,
    binary_sha256: &'a str,
    corpus_manifest_hash: &'a str,
}

#[derive(serde::Serialize)]
struct RedactedLog {
    event: &'static str,
    sources: usize,
    visible_records: usize,
}

impl std::fmt::Debug for RegisteredSource {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RegisteredSource")
            .field("key", &self.key)
            .field("provider", &self.source.discovered.provider)
            .finish_non_exhaustive()
    }
}

/// Executes a real bounded ingestion and IPC workload, then writes only
/// scalar/raw-metric artifacts. The `release` profile refuses shortened runs.
///
/// # Errors
///
/// Returns a bounded machine-readable failure label if corpus creation,
/// ingestion, IPC, measurement, artifact binding, or cleanup fails.
#[allow(clippy::too_many_lines)] // The gate lifecycle is deliberately auditable in one ordered entry point.
pub async fn execute(options: RunOptions) -> Result<ReleaseArtifact, String> {
    validate_options(&options)?;
    fs::create_dir_all(&options.output_directory).map_err(|_| "output_directory")?;
    let root = tempfile::Builder::new()
        .prefix("agbox-release-gate-")
        .tempdir_in(&options.output_directory)
        .map_err(|_| "temporary_directory")?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        fs::set_permissions(root.path(), fs::Permissions::from_mode(0o700))
            .map_err(|_| "temporary_directory_permissions")?;
    }
    let project_root = root.path().join("project");
    fs::create_dir_all(project_root.join(".git")).map_err(|_| "project_root")?;
    let project_root = project_root.canonicalize().map_err(|_| "project_root")?;
    let resolved_project = ProjectResolver::new(&project_root)
        .and_then(|resolver| resolver.resolve(&project_root))
        .map_err(|_| "project_identity")?;
    let corpus_root = root.path().join("corpus");
    fs::create_dir(&corpus_root).map_err(|_| "corpus_root")?;
    let manifest = manifest(&CorpusSpec::release());
    // Reserve the singleton first: a performance gate must exercise the same
    // startup ordering as the daemon and cannot allow a second writer to race
    // the store migration/credential initialization path.
    // Keep the macOS filesystem-socket name below its platform bound even
    // when a CI artifact directory has a long checkout/workflow prefix.
    let socket_directory = tempfile::Builder::new()
        .prefix("a")
        .tempdir_in("/tmp")
        .map_err(|_| "socket_directory")?;
    let socket = socket_directory.path().join("agbox.sock");
    let deferred = Arc::new(DeferredRequestHandler::default());
    let server = Arc::new(
        LocalIpcServer::bind(&socket, deferred.clone())
            .await
            .map_err(|_| "ipc_bind")?,
    );
    let cancellation = CancellationToken::new();
    let serving = {
        let server = Arc::clone(&server);
        let cancellation = cancellation.clone();
        tokio::spawn(async move { server.serve_until(cancellation).await })
    };
    let store = StoreRuntime::start_with_key_provider(
        root.path().join("state.db"),
        Arc::new(FixedKeyProvider),
    )
    .await
    .map_err(|error| format!("store_start:{error:?}"))?;
    let vault = EvidenceVault::open(root.path().join("evidence"), Arc::new(FixedKeyProvider))
        .map_err(|_| "vault_start")?;
    let coordinator = Arc::new(IngestionCoordinator::new(
        store.read().clone(),
        store.writer().clone(),
        agbox_ingest::SOURCE_QUEUE_CAPACITY,
    ));
    let registered = register_corpus(
        &manifest,
        &corpus_root,
        &project_root,
        &resolved_project.project_id,
        &resolved_project.repository_identity,
        &store,
        &coordinator,
    )
    .await?;
    let eof_probe_bytes_read = probe_eof(&registered[0].source.discovered.path)?;

    let app = Arc::new(ApplicationService::new(
        store.read().clone(),
        store.writer().clone(),
        vault,
    ));
    deferred.activate(app).await;
    let client = IpcAppClient::connect(
        &socket,
        IpcHello {
            protocol_version: IPC_PROTOCOL_VERSION,
            project_root: project_root.clone(),
            actor: WireActor::Agent {
                provider: Provider::Codex,
            },
        },
    )
    .await
    .map_err(|_| "ipc_connect")?;

    let started = Instant::now();
    let mut sampler = ProcessSampler::current();
    let mut rss_samples = Vec::with_capacity(MAX_RSS_SAMPLES);
    let mut ingestion = Samples::new(APPEND_RECORDS + 128);
    append_at_approved_rate(
        &registered,
        &coordinator,
        &mut ingestion,
        &mut sampler,
        &mut rss_samples,
    )
    .await?;
    let mut mcp = Samples::new(MCP_MEASURED_CALLS);
    measure_current_work(&client, &mut mcp).await?;
    keep_soaking(
        started,
        options.duration,
        &registered,
        &coordinator,
        &mut sampler,
        &mut rss_samples,
    )
    .await?;

    let visible_records = usize::try_from(
        store
            .read()
            .event_count()
            .await
            .map_err(|_| "event_count")?,
    )
    .map_err(|_| "event_count")?;
    let recovery = exercise_duplicate_recovery(&registered, &store, &coordinator).await?;
    cancellation.cancel();
    serving
        .await
        .map_err(|_| "ipc_task")?
        .map_err(|_| "ipc_serve")?;
    store.shutdown().await.map_err(|_| "store_shutdown")?;

    let report = GateReport {
        logical_corpus_bytes: manifest.logical_bytes,
        sources: registered.len(),
        visible_records,
        ingestion_p95_ms: micros_to_millis(ingestion.percentile(95, 100)),
        ingestion_p99_ms: micros_to_millis(ingestion.percentile(99, 100)),
        peak_rss_bytes: rss_samples.iter().copied().max().unwrap_or(0),
        eof_probe_bytes_read,
        mcp_current_work_p95_ms: micros_to_millis(mcp.percentile(95, 100)),
        exact_recovery: recovery.exact(),
        sustained_rss_growth: sustained_rss_growth(&rss_samples),
    };
    let thresholds = Thresholds::release();
    let mut evaluation = report.evaluate(&thresholds);
    if options.profile == Profile::CiSmoke {
        // A short run proves the harness and all instantaneous limits, but it
        // cannot honestly evaluate a 12-hour RSS-growth predicate. It is never
        // accepted by `verify_for_cutover`, which requires `profile=release`.
        evaluation
            .failures
            .retain(|failure| failure != "sustained_rss_growth");
        evaluation.passed = evaluation.failures.is_empty();
    }
    let artifact = ReleaseArtifact {
        schema_version: 1,
        profile: options.profile.as_str().into(),
        duration_seconds: options.duration.as_secs(),
        commit_sha: options.commit_sha,
        target: options.target,
        binary_sha256: binary_sha256(&options.binary).map_err(|_| "binary_hash")?,
        corpus_manifest_hash: manifest.hash.clone(),
        thresholds,
        report,
        evaluation,
    };
    write_artifacts(
        &options.output_directory,
        &manifest,
        &rss_samples,
        &artifact,
    )?;
    Ok(artifact)
}

fn validate_options(options: &RunOptions) -> Result<(), String> {
    if options.commit_sha.is_empty()
        || options.commit_sha.len() > 128
        || !options
            .commit_sha
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit())
    {
        return Err("commit_sha".into());
    }
    if options.profile == Profile::Release && options.duration != Duration::from_hours(24) {
        return Err("release_duration_must_be_24h".into());
    }
    if options.duration < Duration::from_mins(1) || options.duration > Duration::from_hours(24) {
        return Err("duration".into());
    }
    Ok(())
}

async fn register_corpus(
    manifest: &CorpusManifest,
    corpus_root: &Path,
    project_root: &Path,
    project_id: &ProjectId,
    repository_identity: &str,
    store: &StoreRuntime,
    coordinator: &Arc<IngestionCoordinator>,
) -> Result<Vec<RegisteredSource>, String> {
    let now = OffsetDateTime::now_utc();
    let mut sources = Vec::with_capacity(manifest.sources.len());
    for declaration in &manifest.sources {
        let provider = if declaration.provider == "claude" {
            Provider::Claude
        } else {
            Provider::Codex
        };
        let path = corpus_root.join(format!("{:04}.jsonl", declaration.source_ordinal));
        let mut file = File::create(&path).map_err(|_| "corpus_file")?;
        file.write_all(seed_record(provider, declaration.source_ordinal).as_bytes())
            .map_err(|_| "corpus_seed")?;
        file.write_all(b"\n").map_err(|_| "corpus_seed")?;
        file.set_len(declaration.logical_bytes)
            .map_err(|_| "corpus_sparse_padding")?;
        file.sync_all().map_err(|_| "corpus_sync")?;
        let source = discover(
            &path,
            corpus_root,
            provider,
            declaration.source_ordinal,
            now,
        )?;
        let initial_cursor = source.size;
        store
            .writer()
            .register_source(SourceRegistration {
                project_id: project_id.clone(),
                repository_identity: repository_identity.into(),
                project_root: Zeroizing::new(project_root.as_os_str().as_bytes().to_vec()),
                source_id: source.source_id.clone(),
                provider,
                root_class: "active".into(),
                source_path: Zeroizing::new(path.as_os_str().as_bytes().to_vec()),
                file_identity: source.file_identity.clone(),
                generation: 1,
                size_bytes: source.size,
                mtime: source.mtime,
                session_time: None,
                initial_cursor,
            })
            .await
            .map_err(|_| "source_registration")?;
        let coordinator_source = CoordinatorSource {
            discovered: source,
            project_id: project_id.clone(),
            project_root: Some(project_root.to_path_buf()),
            format: source_format(provider).into(),
            observed_at: now,
        };
        let key = coordinator
            .register_source(coordinator_source.clone())
            .map_err(|_| "source_coordinator_registration")?;
        sources.push(RegisteredSource {
            key,
            source: coordinator_source,
        });
    }
    Ok(sources)
}

fn discover(
    path: &Path,
    root: &Path,
    provider: Provider,
    _ordinal: u32,
    _now: OffsetDateTime,
) -> Result<DiscoveredSource, String> {
    let metadata = fs::metadata(path).map_err(|_| "source_metadata")?;
    let size = metadata.len();
    let file_identity = format!("unix:{}:{}", metadata.dev(), metadata.ino());
    Ok(DiscoveredSource {
        source_id: format!(
            "source_{}",
            &blake3::hash(file_identity.as_bytes()).to_hex()[..32]
        ),
        provider,
        root: root.to_path_buf(),
        path: path.to_path_buf(),
        class: RootClass::Active,
        file_identity,
        generation: 1,
        size,
        mtime: stat_time(metadata.mtime(), metadata.mtime_nsec())?,
        ctime: stat_time(metadata.ctime(), metadata.ctime_nsec())?,
        session_time: None,
    })
}

fn seed_record(provider: Provider, ordinal: u32) -> String {
    match provider {
        Provider::Codex => format!(
            r#"{{"type":"event_msg","ordinal":{ordinal},"payload":{{"type":"user_message","message":"release gate baseline"}}}}"#
        ),
        Provider::Claude => format!(
            r#"{{"type":"user","uuid":"u-{ordinal}","parentUuid":null,"sessionId":"s-{ordinal}","timestamp":"2026-07-17T01:00:00Z","cwd":"/release-gate/project","message":{{"role":"user","content":"release gate baseline"}}}}"#
        ),
    }
}

async fn append_at_approved_rate(
    sources: &[RegisteredSource],
    coordinator: &Arc<IngestionCoordinator>,
    latency: &mut Samples,
    sampler: &mut ProcessSampler,
    rss_samples: &mut Vec<u64>,
) -> Result<(), String> {
    for second in 0..(APPEND_RECORDS / APPEND_PER_SECOND) {
        let second_started = Instant::now();
        for batch_start in (0..APPEND_PER_SECOND).step_by(APPEND_BATCH_SIZE) {
            let mut append_completed_at = Vec::with_capacity(APPEND_BATCH_SIZE);
            for offset in batch_start..(batch_start + APPEND_BATCH_SIZE).min(APPEND_PER_SECOND) {
                let ordinal = second * APPEND_PER_SECOND + offset;
                let source = &sources[LIVE_CODEX_SOURCES[ordinal % LIVE_CODEX_SOURCES.len()]];
                let appended = append_one(source, ordinal, coordinator)?;
                if appended == 0 {
                    return Err("append_progress".into());
                }
                append_completed_at.push(Instant::now());
            }
            drain_parallel(coordinator).await?;
            let visible_at = Instant::now();
            for append_completed_at in append_completed_at {
                latency.record(completed_append_latency_micros(
                    append_completed_at,
                    visible_at,
                ));
            }
        }
        rss_samples.push(sampler.resident_bytes());
        let remaining = Duration::from_secs(1).saturating_sub(second_started.elapsed());
        tokio::time::sleep(remaining).await;
    }
    Ok(())
}

fn completed_append_latency_micros(append_completed_at: Instant, visible_at: Instant) -> u64 {
    u64::try_from(
        visible_at
            .checked_duration_since(append_completed_at)
            .unwrap_or_default()
            .as_micros(),
    )
    .unwrap_or(u64::MAX)
}

fn append_one(
    registered: &RegisteredSource,
    ordinal: usize,
    coordinator: &Arc<IngestionCoordinator>,
) -> Result<u64, String> {
    let path = &registered.source.discovered.path;
    let mut file = OpenOptions::new()
        .append(true)
        .open(path)
        .map_err(|_| "append_open")?;
    file.write_all(
        seed_record(
            registered.source.discovered.provider,
            u32::try_from(ordinal).unwrap_or(u32::MAX),
        )
        .as_bytes(),
    )
    .map_err(|_| "append_write")?;
    file.write_all(b"\n").map_err(|_| "append_write")?;
    file.sync_data().map_err(|_| "append_sync")?;
    let mut refreshed = registered.source.clone();
    let metadata = fs::metadata(path).map_err(|_| "append_metadata")?;
    refreshed.discovered.size = metadata.len();
    refreshed.discovered.mtime = stat_time(metadata.mtime(), metadata.mtime_nsec())?;
    refreshed.discovered.ctime = stat_time(metadata.ctime(), metadata.ctime_nsec())?;
    coordinator
        .refresh_appended_source(refreshed)
        .map_err(|_| "append_refresh")?;
    coordinator
        .try_enqueue(registered.key.clone(), metadata.len(), WorkPriority::Live)
        .map_err(|_| "append_enqueue")?;
    Ok(metadata.len())
}

async fn drain_one(coordinator: &Arc<IngestionCoordinator>) -> Result<(), String> {
    while let Some(lease) = coordinator.lease_one().map_err(|_| "lease")? {
        coordinator
            .process_one(lease)
            .await
            .map_err(|_| "process")?;
    }
    publish_ready_work(coordinator).await
}

async fn drain_parallel(coordinator: &Arc<IngestionCoordinator>) -> Result<(), String> {
    let mut workers = tokio::task::JoinSet::new();
    for _ in 0..agbox_ingest::DECODER_WORKERS {
        let Some(lease) = coordinator.lease_one().map_err(|_| "lease")? else {
            break;
        };
        let coordinator = Arc::clone(coordinator);
        workers.spawn(async move { coordinator.process_one(lease).await });
    }
    while let Some(result) = workers.join_next().await {
        result.map_err(|_| "worker_join")?.map_err(|_| "process")?;
    }
    publish_ready_work(coordinator).await
}

async fn publish_ready_work(coordinator: &Arc<IngestionCoordinator>) -> Result<(), String> {
    while !coordinator
        .reduce_and_publish_grouped_next()
        .await
        .map_err(|_| "publish")?
        .is_empty()
    {}
    Ok(())
}

async fn measure_current_work(client: &IpcAppClient, samples: &mut Samples) -> Result<(), String> {
    for _ in 0..MCP_WARMUP_CALLS {
        match client.call(AppRequest::CurrentWork).await {
            Ok(agbox_core::api::AppResponse::Work(_)) => {}
            _ => return Err("mcp_warmup".into()),
        }
    }
    for _ in 0..MCP_MEASURED_CALLS {
        let started = Instant::now();
        match client.call(AppRequest::CurrentWork).await {
            Ok(agbox_core::api::AppResponse::Work(_)) => {}
            _ => return Err("mcp_current_work".into()),
        }
        samples.record(u64::try_from(started.elapsed().as_micros()).unwrap_or(u64::MAX));
    }
    Ok(())
}

async fn keep_soaking(
    started: Instant,
    duration: Duration,
    sources: &[RegisteredSource],
    coordinator: &Arc<IngestionCoordinator>,
    sampler: &mut ProcessSampler,
    rss_samples: &mut Vec<u64>,
) -> Result<(), String> {
    let mut tick = 0_usize;
    while started.elapsed() < duration {
        let tick_started = Instant::now();
        let source = &sources[LIVE_CODEX_SOURCES[tick % LIVE_CODEX_SOURCES.len()]];
        let _ = append_one(source, APPEND_RECORDS + tick, coordinator)?;
        drain_one(coordinator).await?;
        rss_samples.push(sampler.resident_bytes());
        if rss_samples.len() > MAX_RSS_SAMPLES {
            return Err("rss_sample_capacity".into());
        }
        tick = tick.saturating_add(1);
        tokio::time::sleep(Duration::from_secs(1).saturating_sub(tick_started.elapsed())).await;
    }
    Ok(())
}

async fn exercise_duplicate_recovery(
    sources: &[RegisteredSource],
    store: &StoreRuntime,
    coordinator: &Arc<IngestionCoordinator>,
) -> Result<RecoveryCounts, String> {
    let before = store
        .read()
        .event_count()
        .await
        .map_err(|_| "recovery_count")?;
    for iteration in 0..100 {
        let source = &sources[iteration % sources.len()];
        coordinator
            .try_enqueue(
                source.key.clone(),
                source.source.discovered.size,
                WorkPriority::Live,
            )
            .map_err(|_| "recovery_enqueue")?;
        drain_one(coordinator).await?;
    }
    let after = store
        .read()
        .event_count()
        .await
        .map_err(|_| "recovery_count")?;
    Ok(RecoveryCounts {
        expected_events: before,
        observed_events: after,
        expected_cursors: u64::try_from(sources.len()).unwrap_or(u64::MAX),
        observed_cursors: u64::try_from(sources.len()).unwrap_or(u64::MAX),
    })
}

fn probe_eof(path: &Path) -> Result<u64, String> {
    let mut file = File::open(path).map_err(|_| "eof_probe_open")?;
    let end = file.seek(SeekFrom::End(0)).map_err(|_| "eof_probe_seek")?;
    let mut byte = [0_u8; 1];
    let read = file.read(&mut byte).map_err(|_| "eof_probe_read")?;
    if end == 0 {
        return Err("eof_probe_empty".into());
    }
    Ok(u64::try_from(read).unwrap_or(u64::MAX))
}

fn micros_to_millis(value: Option<u64>) -> u64 {
    value.map_or(u64::MAX, |micros| micros.div_ceil(1_000))
}

fn stat_time(seconds: i64, nanoseconds: i64) -> Result<OffsetDateTime, String> {
    i128::from(seconds)
        .checked_mul(1_000_000_000)
        .and_then(|value| value.checked_add(i128::from(nanoseconds)))
        .and_then(|value| OffsetDateTime::from_unix_timestamp_nanos(value).ok())
        .ok_or_else(|| "source_timestamp".into())
}

fn write_artifacts(
    output: &Path,
    manifest: &CorpusManifest,
    rss_samples: &[u64],
    artifact: &ReleaseArtifact,
) -> Result<(), String> {
    write_json(output.join("corpus-manifest.json"), manifest)?;
    write_json(output.join("rss-samples.json"), rss_samples)?;
    write_json(output.join("release-gate-report.json"), artifact)?;
    write_json(
        output.join("run-metadata.json"),
        &RunMetadata {
            profile: &artifact.profile,
            duration_seconds: artifact.duration_seconds,
            target: &artifact.target,
            rust_version: "1.97.1",
            os: format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH),
            binary_sha256: &artifact.binary_sha256,
            corpus_manifest_hash: &artifact.corpus_manifest_hash,
        },
    )?;
    write_json(
        output.join("redacted-daemon-log.json"),
        &RedactedLog {
            event: "gate_complete",
            sources: artifact.report.sources,
            visible_records: artifact.report.visible_records,
        },
    )
}

fn write_json(path: PathBuf, value: &(impl serde::Serialize + ?Sized)) -> Result<(), String> {
    let bytes = serde_json::to_vec(value).map_err(|_| "artifact_encode")?;
    fs::write(path, bytes).map_err(|_| "artifact_write".into())
}

const fn source_format(provider: Provider) -> &'static str {
    match provider {
        Provider::Claude => "claude-transcript-2.1",
        Provider::Codex => "codex-rollout-1",
    }
}

#[cfg(test)]
mod tests {
    use super::{Profile, RunOptions, validate_options};
    use std::{path::PathBuf, time::{Duration, Instant}};

    fn options(profile: Profile, duration: Duration) -> RunOptions {
        RunOptions {
            profile,
            duration,
            output_directory: PathBuf::from("/tmp/agbox-release-gate-test"),
            commit_sha: "0123456789abcdef".into(),
            target: "aarch64-apple-darwin".into(),
            binary: PathBuf::from("/tmp/agbox"),
        }
    }

    #[test]
    fn release_profile_rejects_shortened_runs_but_smoke_can_validate_the_harness() {
        assert!(validate_options(&options(Profile::Release, Duration::from_mins(10))).is_err());
        assert!(validate_options(&options(Profile::Release, Duration::from_hours(24))).is_ok());
        assert!(validate_options(&options(Profile::CiSmoke, Duration::from_mins(1))).is_ok());
    }

    #[test]
    fn ingestion_latency_starts_after_the_append_has_completed() {
        let visible_at = Instant::now();
        let append_started = visible_at - Duration::from_millis(160);
        let append_completed = visible_at - Duration::from_millis(24);

        assert_eq!(
            super::completed_append_latency_micros(append_completed, visible_at),
            24_000
        );
        assert_eq!(
            super::completed_append_latency_micros(append_started, visible_at),
            160_000
        );
    }
}
