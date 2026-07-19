//! Allowlisted configuration commands.

use serde::{Deserialize, Serialize};

use crate::{
    CliError,
    args::{ConfigCommand, ConfigKey},
    platform::Platform,
};

const SETTINGS_FILE: &str = "settings.json";

/// The complete user-editable native configuration. New fields must be added
/// to the typed command enum rather than accepted as document paths.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeSettings {
    pub retention_days: u16,
}

impl Default for RuntimeSettings {
    fn default() -> Self {
        Self { retention_days: 90 }
    }
}

/// Runs one allowlisted configuration operation through the owner-private
/// platform file boundary.
///
/// # Errors
///
/// Returns a stable error when settings are malformed, out of range, or cannot
/// be read or atomically rewritten as a private file.
pub fn run(platform: &impl Platform, command: ConfigCommand) -> Result<RuntimeSettings, CliError> {
    let paths = platform.paths().map_err(|_| CliError::Unavailable)?;
    let path = paths.config.join(SETTINGS_FILE);
    let mut settings = match platform
        .read_file(&path)
        .map_err(|_| CliError::Unavailable)?
    {
        Some(bytes) => serde_json::from_slice(&bytes).map_err(|_| CliError::InvalidConfig)?,
        None => RuntimeSettings::default(),
    };
    if let ConfigCommand::Set { key, value } = command {
        match key {
            ConfigKey::RetentionDays => {
                let retention_days = value.parse::<u16>().map_err(|_| CliError::InvalidConfig)?;
                if !(1..=3_650).contains(&retention_days) {
                    return Err(CliError::InvalidConfig);
                }
                settings.retention_days = retention_days;
            }
        }
        let encoded = serde_json::to_vec_pretty(&settings).map_err(|_| CliError::InvalidConfig)?;
        let _ = platform
            .write_private_file(&path, &encoded)
            .map_err(|_| CliError::Unavailable)?;
    }
    Ok(settings)
}
