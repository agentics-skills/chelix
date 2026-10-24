use std::path::PathBuf;

/// Returns the configured Chelix config directory.
///
/// Resolution order comes from `chelix_config::config_dir()`:
/// 1. programmatic override (`set_config_dir`)
/// 2. absolute `CHELIX_CONFIG_DIR`
/// 3. `~/.config/chelix`
///
/// A non-empty `CHELIX_CONFIG_DIR` that is not absolute is an error.
/// A missing home directory is an error.
pub fn chelix_config_dir() -> crate::Result<PathBuf> {
    chelix_config::config_dir().map_err(|error| crate::Error::message(error.to_string()))
}
