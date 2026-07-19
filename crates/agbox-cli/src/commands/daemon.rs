//! Foreground daemon composition.

use std::{os::unix::ffi::OsStrExt, path::Path, sync::Arc};

use agbox_adapters::{RootClass, adapters};
use agbox_ingest::{
    CoordinatorSource, DiscoveryWalker, HistoryDecision, HistoryPolicy, IngestionCoordinator,
    RecordScanner, ScanOutcome, VerifiedSourceOpener, WorkPriority, resolve_source_project,
};
use agbox_service::{ApplicationService, Components, Daemon, ipc::LocalIpcServer};
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
    let path = paths.logs.join("agbox.log");
    let mut emitted = 0_usize;
    loop {
        let contents = read_bounded_log(&path)?;
        let entries = contents.lines().collect::<Vec<_>>();
        let start = if emitted == 0 {
            entries.len().saturating_sub(200)
        } else if entries.len() < emitted {
            0
        } else {
            emitted
        };
        for entry in entries.iter().skip(start) {
            if entry.len() > agbox_service::logging::MAX_LOG_ENTRY_BYTES {
                return Err(CliError::Unavailable);
            }
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

fn read_bounded_log(path: &Path) -> Result<String, CliError> {
    let metadata = std::fs::symlink_metadata(path).map_err(|_| CliError::Unavailable)?;
    if metadata.file_type().is_symlink() || !metadata.is_file() || metadata.len() > 1_048_576 {
        return Err(CliError::Unavailable);
    }
    std::fs::read_to_string(path).map_err(|_| CliError::Unavailable)
}

/// Runs the single-writer foreground service until an interrupt signal arrives.
///
/// # Errors
///
/// Returns a stable CLI error when the store, IPC daemon, signal, or shutdown
/// lifecycle cannot complete.
pub async fn foreground(paths: &AgboxPaths) -> Result<(), CliError> {
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
    let application = ApplicationService::new(store.read().clone(), store.writer().clone(), vault);
    let server = LocalIpcServer::bind(paths.socket(), Arc::new(application))
        .await
        .map_err(|_| CliError::Unavailable)?;
    let daemon = Daemon::run(Components::new(server)).map_err(|_| CliError::Unavailable)?;
    tokio::signal::ctrl_c()
        .await
        .map_err(|_| CliError::Unavailable)?;
    daemon.shutdown().await.map_err(|_| CliError::Unavailable)?;
    store.shutdown().await.map_err(|_| CliError::Unavailable)
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
