#![allow(clippy::missing_errors_doc)]

//! Narrow, preserving configuration edits for the managed MCP entries.

use std::path::Path;

use serde_json::{Map, Value, json};
use toml_edit::{Array, DocumentMut, Item, Table, Value as TomlValue, value};

use crate::platform::PlatformError;

const TOOLS: [&str; 5] = [
    "list_work",
    "get_current_work",
    "get_work",
    "get_evidence",
    "search_work",
];

/// Merges one managed Claude stdio entry while retaining every other JSON member.
pub fn merge_claude_user(
    existing: Option<&[u8]>,
    executable: &Path,
) -> Result<Vec<u8>, PlatformError> {
    let mut root = match existing {
        Some(bytes) => {
            serde_json::from_slice(bytes).map_err(|_| PlatformError::InvalidConfiguration)?
        }
        None => Value::Object(Map::new()),
    };
    let object = root
        .as_object_mut()
        .ok_or(PlatformError::InvalidConfiguration)?;
    let servers = object
        .entry("mcpServers")
        .or_insert_with(|| Value::Object(Map::new()))
        .as_object_mut()
        .ok_or(PlatformError::InvalidConfiguration)?;
    servers.insert(
        "agbox".into(),
        json!({
            "type": "stdio",
            "command": executable,
            "args": ["mcp", "--provider", "claude", "--project-root", "${CLAUDE_PROJECT_DIR:-.}"]
        }),
    );
    serde_json::to_vec_pretty(&root).map_err(|_| PlatformError::InvalidConfiguration)
}

/// Preserves Claude settings verbatim until a documented safe hook transport exists.
#[must_use]
pub fn merge_claude_settings(existing: Option<&[u8]>) -> Option<Vec<u8>> {
    existing.map(ToOwned::to_owned)
}

/// Merges the managed Codex MCP table without changing unrelated TOML, including `notify`.
pub fn merge_codex_config(
    existing: Option<&[u8]>,
    executable: &Path,
) -> Result<Vec<u8>, PlatformError> {
    let source = existing.map_or("", |bytes| std::str::from_utf8(bytes).unwrap_or(""));
    if existing.is_some() && source.is_empty() {
        return Err(PlatformError::InvalidConfiguration);
    }
    let mut document = source
        .parse::<DocumentMut>()
        .map_err(|_| PlatformError::InvalidConfiguration)?;
    let root = document.as_table_mut();
    if !root.contains_key("mcp_servers") {
        root.insert("mcp_servers", Item::Table(Table::new()));
    }
    let Some(servers) = root.get_mut("mcp_servers").and_then(Item::as_table_mut) else {
        return Err(PlatformError::InvalidConfiguration);
    };
    let mut agbox = Table::new();
    agbox.insert("command", value(executable.to_string_lossy().as_ref()));
    agbox.insert(
        "args",
        string_array(["mcp", "--provider", "codex", "--project-root", "."]),
    );
    agbox.insert("enabled", value(true));
    agbox.insert("required", value(false));
    agbox.insert("enabled_tools", string_array(TOOLS));
    agbox.insert("default_tools_approval_mode", value("auto"));
    servers.insert("agbox", Item::Table(agbox));
    Ok(document.to_string().into_bytes())
}

fn string_array<const N: usize>(values: [&str; N]) -> Item {
    let mut array = Array::new();
    for entry in values {
        array.push(TomlValue::from(entry));
    }
    Item::Value(TomlValue::Array(array))
}
