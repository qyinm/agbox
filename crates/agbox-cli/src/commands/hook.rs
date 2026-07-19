//! Privacy-bounded hook commands.

use std::{io::Read, path::Path, sync::Arc};

use agbox_adapters::{RootSpec, adapters};
use agbox_core::{Provider, WorkStatus, api::AppRequest};
use agbox_ingest::{
    DiscoveryWalker, HookSignal, HookSourceVerifier, HookSpool, MAX_HOOK_PAYLOAD_BYTES, SourceKey,
};
use agbox_service::{AppClient, ipc::WireActor};
use agbox_store::{KeyProvider, KeyringKeyProvider};
use time::OffsetDateTime;

use crate::{CliError, commands::client::scoped_client, paths::AgboxPaths};

const MAX_HOOK_DISCOVERY_ENTRIES: usize = 8_192;

/// Prints an intentionally non-authoritative active-work hint for a supported
/// agent hook. It never reads stdin, a transcript, evidence, or a contract.
///
/// # Errors
///
/// Returns a stable CLI failure when the verified local daemon is unavailable.
pub async fn active_index(
    paths: &AgboxPaths,
    project_root: &Path,
    provider: Provider,
    max_items: u16,
) -> Result<(), CliError> {
    if max_items == 0 || max_items > 10 {
        return Err(CliError::InvalidConfig);
    }
    let client = scoped_client(paths, project_root, WireActor::Agent { provider }).await?;
    let response = client
        .call(AppRequest::ListWork {
            status: Some(WorkStatus::Active),
            limit: max_items,
        })
        .await
        .map_err(|_| CliError::Unavailable)?;
    let agbox_core::api::AppResponse::WorkList(page) = response else {
        return Err(CliError::Unavailable);
    };
    println!(
        "agbox found {} active work items. Use get_current_work or list_work for evidence-backed handoff context.",
        page.items.len()
    );
    Ok(())
}

/// Reads a provider hook payload, verifies its transcript against the locally
/// discoverable provider roots, and stores only its encrypted normalized
/// signal. The daemon remains the only process that may turn a signal into an
/// ingestion action.
///
/// # Errors
///
/// Returns a stable CLI error without exposing hook contents, paths, or keyring
/// failures.
pub fn ingest_stdin(
    paths: &AgboxPaths,
    home: &Path,
    provider: Provider,
    max_bytes: usize,
) -> Result<(), CliError> {
    ingest(
        paths,
        home,
        provider,
        max_bytes,
        std::io::stdin().lock(),
        Arc::new(KeyringKeyProvider),
    )
}

/// Testable hook ingestion boundary. It deliberately accepts a key provider
/// rather than reading a database or starting a daemon.
///
/// # Errors
///
/// Returns a stable CLI error for every malformed, untrusted, oversized, or
/// persistence failure.
pub fn ingest<R: Read>(
    paths: &AgboxPaths,
    home: &Path,
    provider: Provider,
    max_bytes: usize,
    reader: R,
    keys: Arc<dyn KeyProvider>,
) -> Result<(), CliError> {
    if max_bytes == 0 || max_bytes > MAX_HOOK_PAYLOAD_BYTES {
        return Err(CliError::InvalidConfig);
    }
    let take = u64::try_from(max_bytes)
        .ok()
        .and_then(|value| value.checked_add(1))
        .ok_or(CliError::InvalidConfig)?;
    let verifier = ProviderSourceVerifier::new(home, provider);
    let signal = HookSignal::from_reader(reader.take(take), &verifier, OffsetDateTime::now_utc())
        .map_err(|_| CliError::InvalidHook)?;
    let spool = HookSpool::new(&paths.spool, keys).map_err(|_| CliError::Unavailable)?;
    spool.enqueue(&signal).map_err(|_| CliError::Unavailable)
}

/// Metadata-only verifier for a bounded scan of the native provider roots.
/// It never follows a hook-supplied path and derives the source identity from
/// the same descriptor-safe discovery code the daemon uses.
#[derive(Debug)]
struct ProviderSourceVerifier {
    provider: Provider,
    roots: Vec<RootSpec>,
}

impl ProviderSourceVerifier {
    fn new(home: &Path, provider: Provider) -> Self {
        let roots = adapters()
            .iter()
            .copied()
            .find(|adapter| adapter.provider() == provider)
            .map(|adapter| adapter.roots(home))
            .unwrap_or_default();
        Self { provider, roots }
    }
}

impl HookSourceVerifier for ProviderSourceVerifier {
    fn verify(
        &self,
        provider: Provider,
        path: &Path,
        target_size: u64,
    ) -> Option<(SourceKey, u64)> {
        if provider != self.provider || !path.is_absolute() {
            return None;
        }
        let mut remaining = MAX_HOOK_DISCOVERY_ENTRIES;
        for root in &self.roots {
            if !root.path.is_dir() {
                continue;
            }
            let mut walker = DiscoveryWalker::new(self.provider, root.clone()).ok()?;
            loop {
                let batch = walker.next_batch(remaining.min(256)).ok()?;
                if batch.visited_entries > remaining {
                    return None;
                }
                remaining -= batch.visited_entries;
                if let Some(source) = batch.sources.into_iter().find(|source| source.path == path) {
                    if target_size > source.size {
                        return None;
                    }
                    return SourceKey::new(source.source_id, source.generation)
                        .ok()
                        .map(|key| (key, source.size));
                }
                if batch.cursor.is_none() || remaining == 0 {
                    break;
                }
            }
        }
        None
    }
}
