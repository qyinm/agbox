//! Privacy-bounded hook commands.

use std::path::Path;

use agbox_core::{Provider, WorkStatus, api::AppRequest};
use agbox_service::{AppClient, ipc::WireActor};

use crate::{CliError, commands::client::scoped_client, paths::AgboxPaths};

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
