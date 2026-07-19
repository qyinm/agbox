//! Safe conversion of untrusted adapter workspace hints into project identity.

use std::io::Read;

use agbox_adapters::project_hint_from_reader;
use agbox_core::Provider;

use crate::{ProjectError, ResolvedProject};

/// A source cannot be enrolled until its untrusted workspace hint survives the
/// same no-symlink Git-root resolution used by IPC clients.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum SourceAttributionError {
    #[error("source project hint is invalid")]
    Hint,
    #[error("source project is unavailable")]
    Project,
}

/// Resolves one streamed native record to a canonical Git project, if it
/// contains a supported absolute workspace hint. The raw path is never
/// returned or logged on failure.
///
/// # Errors
///
/// Returns a bounded error if the selected hint cannot be decoded or cannot
/// independently establish a valid project identity.
pub fn resolve_source_project(
    provider: Provider,
    record: impl Read,
) -> Result<Option<ResolvedProject>, SourceAttributionError> {
    let Some(hint) =
        project_hint_from_reader(provider, record).map_err(|_| SourceAttributionError::Hint)?
    else {
        return Ok(None);
    };
    let resolver = crate::ProjectResolver::new(hint.as_path()).map_err(map_project_error)?;
    resolver
        .resolve(hint.as_path())
        .map(Some)
        .map_err(map_project_error)
}

fn map_project_error(_error: ProjectError) -> SourceAttributionError {
    SourceAttributionError::Project
}
