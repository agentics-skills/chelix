//! Exa request-timeout checks.

use chelix_config::{ChelixConfig, Diagnostic, DiagnosticCategory, Severity};

/// Reject a zero `tools.exa.request_timeout_secs`.
#[must_use]
pub fn validate_config(config: &ChelixConfig) -> Vec<Diagnostic> {
    if config.tools.exa.request_timeout_secs == 0 {
        return vec![Diagnostic {
            severity: Severity::Error,
            summary_message: None,
            category: DiagnosticCategory::InvalidValue,
            path: "tools.exa.request_timeout_secs".into(),
            message: "tools.exa.request_timeout_secs must be at least 1".into(),
        }];
    }
    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_timeout_is_accepted() {
        assert!(validate_config(&ChelixConfig::default()).is_empty());
    }

    #[test]
    fn zero_timeout_is_rejected() {
        let mut config = ChelixConfig::default();
        config.tools.exa.request_timeout_secs = 0;
        let diagnostics = validate_config(&config);
        assert!(diagnostics.iter().any(|diagnostic| {
            diagnostic.path == "tools.exa.request_timeout_secs"
                && diagnostic.severity == Severity::Error
                && diagnostic.category == DiagnosticCategory::InvalidValue
                && diagnostic.message == "tools.exa.request_timeout_secs must be at least 1"
        }));
    }
}
