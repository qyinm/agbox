//! Bounded, non-authoritative CLI rendering for application DTOs.

use std::io::{self, Write};

use agbox_core::api::{AppResponse, EvidenceView, WorkDetail, WorkSummary};

use crate::{CliError, args::Output};

const MAX_RENDERED_FIELD_BYTES: usize = 4 * 1024;

/// Writes one already-scoped application response without exposing transport
/// details, database paths, or raw evidence by default.
///
/// # Errors
///
/// Returns a stable CLI error when stdout cannot accept a bounded response.
pub fn response(output: Output, value: AppResponse) -> Result<(), CliError> {
    match output {
        Output::Json => {
            let encoded = serde_json::to_vec(&value).map_err(|_| CliError::Output)?;
            write_all(&encoded)?;
            write_all(b"\n")
        }
        Output::Text => text_response(value),
    }
}

fn text_response(value: AppResponse) -> Result<(), CliError> {
    match value {
        AppResponse::WorkList(page) => {
            for work in page.items {
                summary(&work)?;
            }
            if page.truncated {
                line("Results truncated.")?;
            }
            Ok(())
        }
        AppResponse::Search(page) => {
            for hit in page.items {
                summary(&hit.work)?;
            }
            if page.truncated {
                line("Results truncated.")?;
            }
            Ok(())
        }
        AppResponse::Work(work) => detail(&work),
        AppResponse::Evidence(view) => evidence(&view),
        AppResponse::Health(health) => line(if health.ready {
            "daemon: ready"
        } else {
            "daemon: unavailable"
        }),
        AppResponse::Accepted => line("accepted"),
        AppResponse::NotFound => line("not found"),
    }
}

fn summary(work: &WorkSummary) -> Result<(), CliError> {
    line(&format!(
        "{}  r{}  {:?}\n{}",
        work.work_id.as_str(),
        work.revision,
        work.status,
        safe(&work.summary)
    ))
}

fn detail(work: &WorkDetail) -> Result<(), CliError> {
    line(&format!(
        "Work: {}  Revision: {}  Status: {:?}",
        work.work_id.as_str(),
        work.revision,
        work.status
    ))?;
    line(&format!(
        "Objective: {}",
        work.objective
            .as_deref()
            .map_or_else(|| "unknown".to_owned(), safe)
    ))?;
    section("Completed", &work.completed_steps)?;
    section("Next actions", &work.next_actions)?;
    section("Blockers", &work.blockers)?;
    section("Artifacts", &work.artifacts)?;
    section("Verification", &work.verification)
}

fn section(label: &str, values: &[String]) -> Result<(), CliError> {
    line(&format!("{label}:"))?;
    if values.is_empty() {
        return line("- none");
    }
    for value in values {
        line(&format!("- {}", safe(value)))?;
    }
    Ok(())
}

fn evidence(value: &EvidenceView) -> Result<(), CliError> {
    line("----- BEGIN UNTRUSTED EVIDENCE DATA -----")?;
    if let Some(raw) = &value.raw {
        let bounded = &raw[..raw.len().min(MAX_RENDERED_FIELD_BYTES)];
        line(&safe(&String::from_utf8_lossy(bounded)))?;
        if raw.len() > bounded.len() {
            line("[truncated]")?;
        }
    } else {
        line(&safe(&value.redacted_preview))?;
    }
    line("----- END UNTRUSTED EVIDENCE DATA -----")
}

fn safe(value: &str) -> String {
    let end = value
        .char_indices()
        .nth(MAX_RENDERED_FIELD_BYTES)
        .map_or(value.len(), |(index, _)| index);
    let mut bounded = value[..end].to_owned();
    if end < value.len() {
        bounded.push('…');
    }
    // DTOs do not contain source-path fields. This guards the remaining
    // free-text rendering boundary against an accidental absolute path.
    for marker in ["/Users/", "/home/", "/private/", "/tmp/"] {
        while let Some(start) = bounded.find(marker) {
            let tail = &bounded[start..];
            let length = tail.find(char::is_whitespace).unwrap_or(tail.len());
            bounded.replace_range(start..start + length, "[redacted path]");
        }
    }
    bounded
}

fn line(value: &str) -> Result<(), CliError> {
    write_all(value.as_bytes())?;
    write_all(b"\n")
}

fn write_all(bytes: &[u8]) -> Result<(), CliError> {
    io::stdout()
        .lock()
        .write_all(bytes)
        .map_err(|_| CliError::Output)
}
