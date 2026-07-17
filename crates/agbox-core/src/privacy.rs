use std::{
    fmt,
    path::{Component, Path, PathBuf},
};

use aho_corasick::{AhoCorasick, AhoCorasickBuilder, Anchored, Input, StartKind};
use serde::{Deserialize, Serialize};

use crate::limits::{MAX_INLINE_BYTES, MAX_PREVIEW_BYTES};

const REDACTED_SECRET: &str = "[REDACTED_SECRET]";
const LOCAL_PATH: &str = "[LOCAL_PATH]";

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PrivacyLabel {
    RestrictedLocal,
    PrivateLocal,
    DerivedLocal,
    SyncEligible,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Authority {
    ModelInference,
    AgentStatement,
    ObservedState,
    ToolResult,
    HumanIntent,
}

impl Authority {
    #[must_use]
    pub fn may_define_instruction(self) -> bool {
        matches!(self, Self::HumanIntent)
    }
}

#[derive(Clone, Eq, PartialEq)]
pub struct RedactedText {
    value: String,
    redactions: u16,
}

impl fmt::Debug for RedactedText {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RedactedText")
            .field("byte_length", &self.value.len())
            .field("redactions", &self.redactions)
            .finish()
    }
}

impl RedactedText {
    #[must_use]
    pub fn value(&self) -> &str {
        &self.value
    }

    #[must_use]
    pub fn redactions(&self) -> u16 {
        self.redactions
    }

    pub(crate) fn into_value(self) -> String {
        self.value
    }
}

#[derive(Debug, thiserror::Error)]
pub enum RedactionError {
    #[error("redaction input exceeds the inline-content bound")]
    InputTooLarge,
    #[error("static redaction patterns are invalid")]
    InvalidPatterns(#[from] aho_corasick::BuildError),
}

#[derive(Debug)]
pub struct RedactionPolicy {
    markers: AhoCorasick,
}

impl RedactionPolicy {
    /// Builds the fixed credential-marker automaton.
    ///
    /// # Errors
    ///
    /// Returns [`RedactionError::InvalidPatterns`] if the static patterns
    /// cannot be compiled.
    pub fn new() -> Result<Self, RedactionError> {
        let markers = AhoCorasickBuilder::new()
            .ascii_case_insensitive(true)
            .start_kind(StartKind::Both)
            .build([
                "authorization",
                "bearer",
                "api_key",
                "apikey",
                "secret",
                "token",
                "password",
                "private_key",
                "sk-",
                "ghp_",
                "github_pat_",
                "AKIA",
                "-----BEGIN ",
            ])?;
        Ok(Self { markers })
    }

    /// Scans bounded text and returns only a transfer-safe preview.
    ///
    /// # Errors
    ///
    /// Returns [`RedactionError::InputTooLarge`] when `value` exceeds the
    /// inline-content bound, or a pattern-build error if policy setup failed.
    pub fn redact(
        &self,
        value: &str,
        project_root: Option<&Path>,
    ) -> Result<RedactedText, RedactionError> {
        redact_bounded(value, project_root, &self.markers)
    }
}

fn redact_bounded(
    value: &str,
    project_root: Option<&Path>,
    markers: &AhoCorasick,
) -> Result<RedactedText, RedactionError> {
    if value.len() > MAX_INLINE_BYTES {
        return Err(RedactionError::InputTooLarge);
    }

    let canonical_root = project_root.and_then(normalize_absolute);
    let mut output = String::with_capacity(value.len());
    let mut redactions = 0_u16;
    let mut position = 0;

    while position < value.len() {
        if is_path_start(value, position) {
            let end = scan_token_end(value, position);
            let token = &value[position..end];
            if let Some(replacement) = redact_path(token, canonical_root.as_deref()) {
                output.push_str(&replacement);
                redactions = redactions.saturating_add(1);
                position = end;
                continue;
            }
        }

        let remaining = &value[position..];
        if let Some(found) = markers.find(Input::new(remaining).anchored(Anchored::Yes)) {
            let marker_end = position + found.end();
            let marker = &value[position..marker_end];
            if marker.eq_ignore_ascii_case("-----BEGIN ") {
                let end = pem_block_end(value, position).unwrap_or(value.len());
                output.push_str(REDACTED_SECRET);
                redactions = redactions.saturating_add(1);
                position = end;
                continue;
            }
            if is_credential_prefix(marker) {
                let end = scan_token_end(value, marker_end);
                output.push_str(REDACTED_SECRET);
                redactions = redactions.saturating_add(1);
                position = end;
                continue;
            }
            if let Some((secret_start, secret_end)) =
                adjacent_secret_range(value, marker_end, marker)
            {
                output.push_str(&value[position..secret_start]);
                output.push_str(REDACTED_SECRET);
                redactions = redactions.saturating_add(1);
                position = secret_end;
                continue;
            }
        }

        let character = value[position..].chars().next();
        if let Some(character) = character {
            output.push(character);
            position += character.len_utf8();
        } else {
            break;
        }
    }

    let mut end = output.len().min(MAX_PREVIEW_BYTES);
    while !output.is_char_boundary(end) {
        end -= 1;
    }
    output.truncate(end);
    Ok(RedactedText {
        value: output,
        redactions,
    })
}

fn is_credential_prefix(marker: &str) -> bool {
    ["sk-", "ghp_", "github_pat_", "AKIA"]
        .iter()
        .any(|prefix| marker.eq_ignore_ascii_case(prefix))
}

fn adjacent_secret_range(value: &str, marker_end: usize, marker: &str) -> Option<(usize, usize)> {
    let bytes = value.as_bytes();
    let mut cursor = marker_end;

    if marker.eq_ignore_ascii_case("bearer") {
        cursor = skip_ascii_whitespace(bytes, cursor);
        return (cursor < value.len()).then(|| (cursor, scan_token_end(value, cursor)));
    }

    cursor = skip_ascii_whitespace(bytes, cursor);
    if bytes
        .get(cursor)
        .is_some_and(|byte| matches!(byte, b'"' | b'\''))
    {
        cursor += 1;
        cursor = skip_ascii_whitespace(bytes, cursor);
    }
    if marker.eq_ignore_ascii_case("authorization") {
        if bytes.get(cursor) == Some(&b':') || bytes.get(cursor) == Some(&b'=') {
            cursor += 1;
        }
        cursor = skip_ascii_whitespace(bytes, cursor);
        if value[cursor..]
            .get(..6)
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case("bearer"))
        {
            cursor += 6;
            cursor = skip_ascii_whitespace(bytes, cursor);
        }
        return (cursor < value.len()).then(|| (cursor, scan_token_end(value, cursor)));
    }

    if bytes.get(cursor) != Some(&b'=') && bytes.get(cursor) != Some(&b':') {
        return None;
    }
    cursor += 1;
    cursor = skip_ascii_whitespace(bytes, cursor);
    let quote = bytes
        .get(cursor)
        .copied()
        .filter(|byte| matches!(byte, b'"' | b'\''));
    if quote.is_some() {
        cursor += 1;
    }
    if cursor >= value.len() {
        return Some((cursor, cursor));
    }
    let end = quote.map_or_else(
        || scan_token_end(value, cursor),
        |quote| scan_quoted_value_end(value, cursor, quote),
    );
    Some((cursor, end))
}

fn scan_quoted_value_end(value: &str, start: usize, quote: u8) -> usize {
    let mut escaped = false;
    for (relative, character) in value[start..].char_indices() {
        if escaped {
            escaped = false;
        } else if character == '\\' {
            escaped = true;
        } else if character.is_ascii() && character as u8 == quote {
            return start + relative;
        }
    }
    value.len()
}

fn skip_ascii_whitespace(bytes: &[u8], mut position: usize) -> usize {
    while bytes.get(position).is_some_and(u8::is_ascii_whitespace) {
        position += 1;
    }
    position
}

fn scan_token_end(value: &str, start: usize) -> usize {
    value[start..]
        .char_indices()
        .find_map(|(relative, character)| {
            (relative > 0
                && character.is_ascii()
                && (character.is_ascii_whitespace()
                    || matches!(
                        character,
                        '"' | '\'' | ',' | ';' | '(' | ')' | '[' | ']' | '{' | '}'
                    )))
            .then_some(start + relative)
        })
        .unwrap_or(value.len())
}

fn is_path_start(value: &str, position: usize) -> bool {
    if value.as_bytes().get(position) != Some(&b'/') {
        return false;
    }
    position == 0
        || value[..position]
            .chars()
            .next_back()
            .is_some_and(|previous| {
                previous.is_ascii_whitespace()
                    || matches!(previous, '"' | '\'' | '(' | '[' | '{' | '=' | ':')
            })
}

fn redact_path(token: &str, canonical_root: Option<&Path>) -> Option<String> {
    let path = Path::new(token);
    if !path.is_absolute() {
        return None;
    }
    let Some(normalized) = normalize_absolute(path) else {
        return Some(LOCAL_PATH.to_owned());
    };
    if let Some(root) = canonical_root
        && let Ok(relative) = normalized.strip_prefix(root)
    {
        let relative = relative.to_string_lossy();
        return Some(if relative.is_empty() {
            "$PROJECT".to_owned()
        } else {
            format!("$PROJECT/{relative}")
        });
    }
    Some(LOCAL_PATH.to_owned())
}

fn normalize_absolute(path: &Path) -> Option<PathBuf> {
    if !path.is_absolute() {
        return None;
    }
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::RootDir | Component::Prefix(_) => normalized.push(component.as_os_str()),
            Component::CurDir => {}
            Component::ParentDir => {
                if !normalized.pop() {
                    return None;
                }
            }
            Component::Normal(part) => normalized.push(part),
        }
    }
    Some(normalized)
}

fn pem_block_end(value: &str, start: usize) -> Option<usize> {
    const BEGIN: &str = "-----BEGIN ";
    let label_start = start + BEGIN.len();
    let label_end = value[label_start..]
        .find("-----")
        .map(|relative| label_start + relative)?;
    let label = &value[label_start..label_end];
    let end_pattern = format!("-----END {label}-----");
    let end_marker = value[label_end + 5..]
        .find(&end_pattern)
        .map(|relative| label_end + 5 + relative)?;
    let line_end = end_marker + end_pattern.len();
    Some(
        value[line_end..]
            .find('\n')
            .map_or(line_end, |relative| line_end + relative + 1),
    )
}
