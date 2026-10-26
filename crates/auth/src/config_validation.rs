//! Authentication exposure checks for a parsed Chelix config.

use chelix_config::{ChelixConfig, Diagnostic, DiagnosticCategory, Severity};

/// Warn when authentication is disabled on a non-loopback bind address.
#[must_use]
pub fn validate_config(config: &ChelixConfig) -> Vec<Diagnostic> {
    if config.auth.disabled && !config.server.binds_localhost() {
        return vec![Diagnostic {
            severity: Severity::Warning,
            summary_message: None,
            category: DiagnosticCategory::Security,
            path: "auth".into(),
            message: format!(
                "authentication is disabled while binding to {}",
                config.server.bind
            ),
        }];
    }
    Vec::new()
}

/// Editor warning when gateway authentication is disabled.
#[must_use]
pub fn configuration_editor_warning(config: &ChelixConfig) -> Option<&'static str> {
    if config.auth.disabled {
        Some("Authentication is disabled. Anyone with network access can use the gateway.")
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn check(bind: &str) -> Vec<Diagnostic> {
        let mut config = ChelixConfig::default();
        config.server.bind = bind.into();
        config.auth.disabled = true;
        validate_config(&config)
    }

    #[test]
    fn editor_warning_tracks_auth_disabled() {
        assert_eq!(configuration_editor_warning(&ChelixConfig::default()), None);
        let mut config = ChelixConfig::default();
        config.auth.disabled = true;
        assert_eq!(
            configuration_editor_warning(&config),
            Some("Authentication is disabled. Anyone with network access can use the gateway.")
        );
    }

    #[test]
    fn auth_disabled_non_localhost_warned() {
        assert!(check("0.0.0.0").iter().any(|diagnostic| {
            diagnostic.category == DiagnosticCategory::Security
                && diagnostic.path == "auth"
                && diagnostic.severity == Severity::Warning
        }));
    }

    #[test]
    fn auth_disabled_localhost_not_warned() {
        for bind in ["127.0.0.1", "localhost", "::1"] {
            assert!(
                check(bind).is_empty(),
                "should not warn about auth disabled on {bind}"
            );
        }
    }
}
