//! Selects `chelix.toml` from the plugin environment.

use std::path::PathBuf;

pub fn resolve(config_dir: Option<&str>, home: Option<&str>) -> Result<PathBuf, String> {
    let config_dir = config_dir.filter(|value| !value.is_empty());
    let home = home.filter(|value| !value.is_empty());
    let dir = if let Some(dir) = config_dir {
        PathBuf::from(dir)
    } else if let Some(home) = home {
        PathBuf::from(home).join(".config").join("chelix")
    } else {
        return Err("HOME is empty".into());
    };
    Ok(dir.join("chelix.toml"))
}

pub fn from_env() -> Result<PathBuf, String> {
    let config_dir = std::env::var("CHELIX_CONFIG_DIR").ok();
    let home = std::env::var("HOME").ok();
    resolve(config_dir.as_deref(), home.as_deref())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uses_config_dir_when_set() {
        let path = resolve(Some("/cfg"), Some("/home/user")).expect("path");
        assert_eq!(path, PathBuf::from("/cfg/chelix.toml"));
    }

    #[test]
    fn empty_config_dir_uses_home() {
        let path = resolve(Some(""), Some("/home/user")).expect("path");
        assert_eq!(path, PathBuf::from("/home/user/.config/chelix/chelix.toml"));
    }

    #[test]
    fn missing_config_dir_uses_home() {
        let path = resolve(None, Some("/home/user")).expect("path");
        assert_eq!(path, PathBuf::from("/home/user/.config/chelix/chelix.toml"));
    }

    #[test]
    fn missing_home_names_the_variable() {
        let error = resolve(None, None).expect_err("home");
        assert!(error.contains("HOME"), "{error}");
        let error = resolve(Some(""), Some("")).expect_err("home");
        assert!(error.contains("HOME"), "{error}");
    }
}
