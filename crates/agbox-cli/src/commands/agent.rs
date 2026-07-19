//! Managed agent configuration commands.

use crate::{
    CliError,
    config::{remove_claude_user, remove_codex_config},
    platform::Platform,
};

/// Removes only agbox-owned MCP blocks; unrelated user configuration remains
/// byte-for-byte under each native configuration format's serializer.
///
/// # Errors
///
/// Returns a stable error when owner-private configuration cannot be read or
/// atomically rewritten.
pub fn disconnect(platform: &impl Platform) -> Result<(), CliError> {
    let home = platform.home().map_err(|_| CliError::Unavailable)?;
    let claude = home.join(".claude.json");
    if let Some(existing) = platform
        .read_file(&claude)
        .map_err(|_| CliError::Unavailable)?
    {
        let updated = remove_claude_user(&existing).map_err(|_| CliError::Unavailable)?;
        let _ = platform
            .write_private_file(&claude, &updated)
            .map_err(|_| CliError::Unavailable)?;
    }
    let codex = home.join(".codex/config.toml");
    if let Some(existing) = platform
        .read_file(&codex)
        .map_err(|_| CliError::Unavailable)?
    {
        let updated = remove_codex_config(&existing).map_err(|_| CliError::Unavailable)?;
        let _ = platform
            .write_private_file(&codex, &updated)
            .map_err(|_| CliError::Unavailable)?;
    }
    Ok(())
}
