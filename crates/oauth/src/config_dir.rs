use std::path::PathBuf;

/// Returns the configured Chelix config directory.
///
/// Resolution order comes from `chelix_config::config_dir()`:
/// 1. programmatic override (`set_config_dir`)
/// 2. `CHELIX_CONFIG_DIR`
/// 3. `~/.config/chelix`
pub fn chelix_config_dir() -> PathBuf {
    chelix_config::config_dir().unwrap_or_else(|| PathBuf::from(".config/chelix"))
}
