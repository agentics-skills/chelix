//! Hook eligibility checks.
//!
//! Before loading a hook, validate its requirements against the current
//! system environment.

use crate::hook_metadata::HookMetadata;

/// Result of eligibility checks.
#[derive(Debug, Clone)]
pub struct EligibilityResult {
    pub eligible: bool,
    pub missing_os: bool,
    pub missing_bins: Vec<String>,
    pub missing_env: Vec<String>,
    pub missing_config: Vec<String>,
}

/// Check whether a hook's requirements are met on the current system.
pub fn check_hook_eligibility(metadata: &HookMetadata) -> EligibilityResult {
    let mut result = EligibilityResult {
        eligible: true,
        missing_os: false,
        missing_bins: Vec::new(),
        missing_env: Vec::new(),
        missing_config: Vec::new(),
    };

    // OS check
    if !metadata.requires.os.is_empty()
        && !metadata
            .requires
            .os
            .iter()
            .any(|os| os == std::env::consts::OS)
    {
        result.missing_os = true;
        result.eligible = false;
    }

    // Binary check
    for bin in &metadata.requires.bins {
        if !bin_exists(bin) {
            result.missing_bins.push(bin.clone());
            result.eligible = false;
        }
    }

    // Environment variable check
    for var in &metadata.requires.env {
        if std::env::var(var).is_err() {
            result.missing_env.push(var.clone());
            result.eligible = false;
        }
    }

    // Config check is skipped for now (requires ChelixConfig access).
    // We leave missing_config empty; gateway wiring can add this later.

    result
}

/// Check if a binary exists on PATH.
fn bin_exists(name: &str) -> bool {
    std::process::Command::new("which")
        .arg(name)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

#[cfg(test)]
mod tests {
    use {
        super::*,
        crate::hook_metadata::{HookMetadata, HookRequirements},
    };

    fn minimal_metadata() -> HookMetadata {
        HookMetadata {
            name: "test".into(),
            description: String::new(),
            emoji: None,
            events: vec![],
            command: None,
            timeout: 10,
            priority: 0,
            env: Default::default(),
            requires: HookRequirements::default(),
        }
    }

    #[test]
    fn no_requirements_is_eligible() {
        let result = check_hook_eligibility(&minimal_metadata());
        assert!(result.eligible);
    }

    #[test]
    fn wrong_os_is_ineligible() {
        let mut meta = minimal_metadata();
        meta.requires.os = vec!["nonexistent-os".into()];
        let result = check_hook_eligibility(&meta);
        assert!(!result.eligible);
        assert!(result.missing_os);
    }

    #[test]
    fn current_os_is_eligible() {
        let mut meta = minimal_metadata();
        meta.requires.os = vec![std::env::consts::OS.into()];
        let result = check_hook_eligibility(&meta);
        assert!(!result.missing_os);
    }

    #[test]
    fn missing_bin_is_ineligible() {
        let mut meta = minimal_metadata();
        meta.requires.bins = vec!["nonexistent_binary_xyz_12345".into()];
        let result = check_hook_eligibility(&meta);
        assert!(!result.eligible);
        assert_eq!(result.missing_bins, vec!["nonexistent_binary_xyz_12345"]);
    }

    #[test]
    fn missing_env_is_ineligible() {
        let mut meta = minimal_metadata();
        meta.requires.env = vec!["CHELIX_TEST_NONEXISTENT_VAR_XYZ".into()];
        let result = check_hook_eligibility(&meta);
        assert!(!result.eligible);
        assert_eq!(result.missing_env, vec!["CHELIX_TEST_NONEXISTENT_VAR_XYZ"]);
    }

    #[test]
    fn present_env_is_eligible() {
        // PATH always exists.
        let mut meta = minimal_metadata();
        meta.requires.env = vec!["PATH".into()];
        let result = check_hook_eligibility(&meta);
        assert!(result.missing_env.is_empty());
    }
}
