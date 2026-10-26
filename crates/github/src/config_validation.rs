//! GitHub request-timeout checks.

use chelix_config::{ChelixConfig, Diagnostic, DiagnosticCategory, Severity};

/// Reject a zero `tools.github.request_timeout_secs`.
#[must_use]
pub fn validate_config(config: &ChelixConfig) -> Vec<Diagnostic> {
    if config.tools.github.request_timeout_secs == 0 {
        return vec![Diagnostic {
            severity: Severity::Error,
            summary_message: None,
            category: DiagnosticCategory::InvalidValue,
            path: "tools.github.request_timeout_secs".into(),
            message: "tools.github.request_timeout_secs must be at least 1".into(),
        }];
    }
    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn github_request_timeout_is_accepted() {
        assert!(validate_config(&ChelixConfig::default()).is_empty());
    }

    #[test]
    fn github_request_timeout_must_be_positive() {
        let mut config = ChelixConfig::default();
        config.tools.github.request_timeout_secs = 0;
        let diagnostics = validate_config(&config);
        assert!(diagnostics.iter().any(|diagnostic| {
            diagnostic.path == "tools.github.request_timeout_secs"
                && diagnostic.severity == Severity::Error
                && diagnostic.category == DiagnosticCategory::InvalidValue
        }));
    }
}
