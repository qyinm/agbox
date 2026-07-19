//! Foreground daemon composition.

use std::{
    os::unix::{ffi::OsStrExt, fs::MetadataExt},
    path::{Path, PathBuf},
    sync::Arc,
};

use agbox_adapters::{RootClass, adapters};
use agbox_ingest::{
    CoordinatorSource, DiscoveryWalker, HistoryDecision, HistoryPolicy, HookSpool,
    IngestionCoordinator, RecordScanner, ScanOutcome, VerifiedSourceOpener, WorkPriority,
    resolve_source_project,
};
use agbox_service::{
    ApplicationService, Components, Daemon,
    ipc::{DeferredRequestHandler, LocalIpcServer},
};
use agbox_store::{EvidenceVault, KeyringKeyProvider, SourceRegistration, StoreRuntime};
use time::OffsetDateTime;
use zeroize::Zeroizing;

use crate::{CliError, args::DaemonCommand, paths::AgboxPaths, platform::Platform};

/// Executes bounded daemon lifecycle commands. Only foreground mode owns the
/// writer; normal lifecycle operations delegate to the native service manager.
///
/// # Errors
///
/// Returns a stable CLI error when the native service manager or bounded log
/// reader cannot complete the requested operation.
pub async fn run(command: DaemonCommand, paths: &AgboxPaths) -> Result<(), CliError> {
    match command {
        DaemonCommand::Start { foreground: true } => foreground(paths).await,
        DaemonCommand::Start { foreground: false } => service_change(true),
        DaemonCommand::Stop => service_change(false),
        DaemonCommand::Logs { follow } => logs(paths, follow).await,
    }
}

fn service_change(start: bool) -> Result<(), CliError> {
    let executable = std::env::current_exe().map_err(|_| CliError::Unavailable)?;
    let platform = crate::platform::macos::MacOsPlatform::for_current_user(executable)
        .map_err(|_| CliError::Unavailable)?;
    let result = if start {
        platform.start_service("com.agbox.runtime")
    } else {
        platform.stop_service("com.agbox.runtime")
    };
    result.map(|_| ()).map_err(|_| CliError::Unavailable)
}

async fn logs(paths: &AgboxPaths, follow: bool) -> Result<(), CliError> {
    let mut emitted = 0_usize;
    loop {
        let entries = read_bounded_logs(paths)?;
        let start = if emitted == 0 {
            entries.len().saturating_sub(200)
        } else if entries.len() < emitted {
            0
        } else {
            emitted
        };
        for entry in entries.iter().skip(start) {
            println!("{entry}");
        }
        emitted = entries.len();
        if !follow {
            return Ok(());
        }
        tokio::select! {
            result = tokio::signal::ctrl_c() => {
                let _ = result;
                return Ok(());
            }
            () = tokio::time::sleep(std::time::Duration::from_millis(250)) => {}
        }
    }
}

#[derive(serde::Deserialize)]
struct LogWire {
    kind: String,
    result: String,
    byte_length: u32,
}

fn read_bounded_logs(paths: &AgboxPaths) -> Result<Vec<String>, CliError> {
    let mut entries = Vec::new();
    let mut total_bytes = 0_u64;
    for path in rotated_log_paths(&paths.logs) {
        if !path.exists() {
            continue;
        }
        let metadata = checked_log_file(&path)?;
        total_bytes = total_bytes.saturating_add(metadata.len());
        if total_bytes > 1_048_576 {
            return Err(CliError::Unavailable);
        }
        let contents = std::fs::read_to_string(&path).map_err(|_| CliError::Unavailable)?;
        for line in contents.lines() {
            if line.len() > agbox_service::logging::MAX_LOG_ENTRY_BYTES {
                return Err(CliError::Unavailable);
            }
            entries.push(render_log_line(line)?);
        }
    }
    Ok(entries)
}

fn rotated_log_paths(directory: &Path) -> Vec<PathBuf> {
    let mut paths = (1..=agbox_service::logging::RETAINED_LOG_FILES)
        .rev()
        .map(|index| directory.join(format!("agbox.log.{index}")))
        .collect::<Vec<_>>();
    paths.push(directory.join("agbox.log"));
    paths
}

fn checked_log_file(path: &Path) -> Result<std::fs::Metadata, CliError> {
    let metadata = std::fs::symlink_metadata(path).map_err(|_| CliError::Unavailable)?;
    if metadata.file_type().is_symlink()
        || !metadata.is_file()
        || metadata.uid() != rustix::process::geteuid().as_raw()
        || metadata.mode() & 0o077 != 0
    {
        return Err(CliError::Unavailable);
    }
    Ok(metadata)
}

fn render_log_line(line: &str) -> Result<String, CliError> {
    let event: LogWire = serde_json::from_str(line).map_err(|_| CliError::Unavailable)?;
    if !safe_log_atom(&event.kind) || !safe_log_atom(&event.result) {
        return Err(CliError::Unavailable);
    }
    Ok(format!(
        "kind={} result={} bytes={}",
        event.kind, event.result, event.byte_length
    ))
}

fn safe_log_atom(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
}

/// Runs the single-writer foreground service until an interrupt signal arrives.
///
/// # Errors
///
/// Returns a stable CLI error when the store, IPC daemon, signal, or shutdown
/// lifecycle cannot complete.
pub async fn foreground(paths: &AgboxPaths) -> Result<(), CliError> {
    // Reserve the singleton owner-only socket before touching the credential
    // store or SQLite. This prevents a concurrent init/daemon invocation from
    // becoming a second writer during slow startup.
    let deferred = Arc::new(DeferredRequestHandler::default());
    let server = LocalIpcServer::bind(paths.socket(), deferred.clone())
        .await
        .map_err(|_| CliError::Unavailable)?;
    let daemon = Daemon::run(Components::new(server)).map_err(|_| CliError::Unavailable)?;
    let runtime = start_runtime(paths, &deferred).await;
    let (store, spool, coordinator) = match runtime {
        Ok(runtime) => runtime,
        Err(error) => {
            let _ = daemon.shutdown().await;
            return Err(error);
        }
    };
    let mut hook_tick = tokio::time::interval(std::time::Duration::from_millis(250));
    loop {
        tokio::select! {
            signal = tokio::signal::ctrl_c() => {
                signal.map_err(|_| CliError::Unavailable)?;
                break;
            }
            _ = hook_tick.tick() => {
                drain_hook_spool(&spool, &coordinator).await?;
            }
        }
    }
    daemon.shutdown().await.map_err(|_| CliError::Unavailable)?;
    store.shutdown().await.map_err(|_| CliError::Unavailable)
}

async fn start_runtime(
    paths: &AgboxPaths,
    deferred: &DeferredRequestHandler,
) -> Result<(StoreRuntime, HookSpool, Arc<IngestionCoordinator>), CliError> {
    let store = StoreRuntime::start(&paths.state_db)
        .await
        .map_err(|_| CliError::Unavailable)?;
    let vault = EvidenceVault::open(
        paths.evidence.clone(),
        std::sync::Arc::new(KeyringKeyProvider),
    )
    .map_err(|_| CliError::Unavailable)?;
    let coordinator = Arc::new(IngestionCoordinator::new(
        store.read().clone(),
        store.writer().clone(),
        agbox_ingest::SOURCE_QUEUE_CAPACITY,
    ));
    let home = paths.root.parent().ok_or(CliError::HomeUnavailable)?;
    bootstrap_sources(home, &store, &coordinator).await?;
    let spool = HookSpool::new(&paths.spool, Arc::new(KeyringKeyProvider))
        .map_err(|_| CliError::Unavailable)?;
    let application = ApplicationService::new(store.read().clone(), store.writer().clone(), vault);
    deferred.activate(Arc::new(application)).await;
    Ok((store, spool, coordinator))
}

async fn bootstrap_sources(
    home: &Path,
    store: &StoreRuntime,
    coordinator: &Arc<IngestionCoordinator>,
) -> Result<(), CliError> {
    for adapter in adapters() {
        for root in adapter.roots(home) {
            if !root.path.is_dir() {
                continue;
            }
            let Ok(mut walker) = DiscoveryWalker::new(adapter.provider(), root) else {
                continue;
            };
            loop {
                let batch = walker
                    .next_batch(agbox_ingest::DISCOVERY_ENTRIES_PER_YIELD)
                    .map_err(|_| CliError::Unavailable)?;
                for source in batch.sources {
                    register_replay_source(store, coordinator, source).await?;
                }
                if batch.cursor.is_none() {
                    break;
                }
            }
        }
    }
    drain_coordinator(coordinator).await
}

async fn drain_hook_spool(
    spool: &HookSpool,
    coordinator: &Arc<IngestionCoordinator>,
) -> Result<(), CliError> {
    let coordinator_for_commit = Arc::clone(coordinator);
    let committed = spool
        .drain(move |signal| {
            let coordinator = Arc::clone(&coordinator_for_commit);
            async move {
                let key = signal.source_key().map_err(|_| ())?;
                coordinator
                    .try_enqueue(key, signal.target_size(), WorkPriority::Live)
                    .map_err(|_| ())
            }
        })
        .await
        .map_err(|_| CliError::Unavailable)?;
    if committed != 0 {
        drain_coordinator(coordinator).await?;
    }
    Ok(())
}

async fn drain_coordinator(coordinator: &Arc<IngestionCoordinator>) -> Result<(), CliError> {
    while let Some(lease) = coordinator.lease_one().map_err(|_| CliError::Unavailable)? {
        let _ = coordinator
            .process_one(lease)
            .await
            .map_err(|_| CliError::Unavailable)?;
    }
    while !coordinator
        .reduce_and_publish_grouped_next()
        .await
        .map_err(|_| CliError::Unavailable)?
        .is_empty()
    {}
    Ok(())
}

async fn register_replay_source(
    store: &StoreRuntime,
    coordinator: &Arc<IngestionCoordinator>,
    source: agbox_adapters::DiscoveredSource,
) -> Result<(), CliError> {
    if !matches!(
        HistoryPolicy.decide(source.session_time, OffsetDateTime::now_utc(), source.size),
        HistoryDecision::ReplayFrom(0)
    ) {
        return Ok(());
    }
    let Some(project) = source_project(&source) else {
        return Ok(());
    };
    store
        .writer()
        .register_source(SourceRegistration {
            project_id: project.project_id.clone(),
            repository_identity: project.repository_identity,
            project_root: Zeroizing::new(project.root.as_os_str().as_bytes().to_vec()),
            source_id: source.source_id.clone(),
            provider: source.provider,
            root_class: root_class(source.class).into(),
            source_path: Zeroizing::new(source.path.as_os_str().as_bytes().to_vec()),
            file_identity: source.file_identity.clone(),
            generation: source.generation,
            size_bytes: source.size,
            mtime: source.mtime,
            session_time: source.session_time,
            initial_cursor: 0,
        })
        .await
        .map_err(|_| CliError::Unavailable)?;
    let key = coordinator
        .register_source(CoordinatorSource {
            discovered: source.clone(),
            project_id: project.project_id,
            project_root: Some(project.root),
            format: source_format(source.provider).into(),
            observed_at: OffsetDateTime::now_utc(),
        })
        .map_err(|_| CliError::Unavailable)?;
    coordinator
        .try_enqueue(key, source.size, WorkPriority::ActiveCatchup)
        .map_err(|_| CliError::Unavailable)
}

fn source_project(
    source: &agbox_adapters::DiscoveredSource,
) -> Option<agbox_ingest::ResolvedProject> {
    let opener = VerifiedSourceOpener::new(&source.root).ok()?;
    let file = opener.open(source).ok()?;
    let mut scanner = RecordScanner::new(file, 0, source.size).ok()?;
    let ScanOutcome::Complete(record) = scanner.next().ok()? else {
        return None;
    };
    resolve_source_project(source.provider, record.open().ok()?).ok()?
}

const fn root_class(class: RootClass) -> &'static str {
    match class {
        RootClass::Active => "active",
        RootClass::Archive => "archive",
    }
}

const fn source_format(provider: agbox_core::Provider) -> &'static str {
    match provider {
        agbox_core::Provider::Claude => "claude-transcript-2.1",
        agbox_core::Provider::Codex => "codex-rollout-1",
    }
}

#[cfg(test)]
mod tests {
    use super::render_log_line;

    #[test]
    fn typed_log_decoder_rejects_unstructured_or_secret_bearing_fields() {
        let Ok(rendered) =
            render_log_line(r#"{"kind":"daemon.ready","result":"ok","byte_length":12}"#)
        else {
            panic!("valid typed log line");
        };
        assert_eq!(rendered, "kind=daemon.ready result=ok bytes=12");
        assert!(render_log_line("raw native transcript secret=abc").is_err());
        assert!(
            render_log_line(
                r#"{"kind":"daemon.ready","result":"secret=/Users/alice","byte_length":12}"#
            )
            .is_err()
        );
    }
}
