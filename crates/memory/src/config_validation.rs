//! Memory embedding checks for a parsed Chelix config.

use chelix_config::{ChelixConfig, Diagnostic, DiagnosticCategory, Severity};

/// Report whether the memory embedding endpoint is complete.
#[must_use]
pub fn validate_config(config: &ChelixConfig) -> Vec<Diagnostic> {
    match config.memory.embedding_fields_complete() {
        Ok(()) => Vec::new(),
        Err(message) => vec![Diagnostic {
            severity: Severity::Error,
            summary_message: None,
            category: DiagnosticCategory::InvalidValue,
            path: "memory".into(),
            message,
        }],
    }
}

#[cfg(test)]
mod tests {
    use {super::*, secrecy::Secret};

    fn check(config: &ChelixConfig) -> Vec<Diagnostic> {
        validate_config(config)
    }

    #[test]
    fn unset_embedding_endpoint_is_accepted() {
        assert!(check(&ChelixConfig::default()).is_empty());
    }

    #[test]
    fn partial_memory_embedding_fields_are_invalid() {
        let mut config = ChelixConfig::default();
        config.memory.url = Some("http://127.0.0.1:8080".into());
        let diagnostics = check(&config);
        assert!(diagnostics.iter().any(|diagnostic| {
            diagnostic.category == DiagnosticCategory::InvalidValue
                && diagnostic.severity == Severity::Error
                && diagnostic.path == "memory"
        }));
    }

    #[test]
    fn complete_embedding_endpoint_is_accepted() {
        let mut config = ChelixConfig::default();
        config.memory.url = Some("http://127.0.0.1:8080".into());
        config.memory.api_key = Some(Secret::new("secret".into()));
        config.memory.dimensions = Some(3);
        assert!(check(&config).is_empty());
    }
}
