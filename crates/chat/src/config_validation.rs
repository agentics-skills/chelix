//! Chat prompt-size checks for a parsed Chelix config.

use chelix_config::{ChelixConfig, Diagnostic, DiagnosticCategory, Severity};

/// Warn when workspace prompt files would be truncated to nothing.
#[must_use]
pub fn validate_config(config: &ChelixConfig) -> Vec<Diagnostic> {
    if config.chat.workspace_file_max_chars == 0 {
        return vec![Diagnostic {
            severity: Severity::Warning,
            summary_message: None,
            category: DiagnosticCategory::InvalidValue,
            path: "chat.workspace_file_max_chars".into(),
            message: "chat.workspace_file_max_chars is 0 — AGENTS.md and TOOLS.md will be empty in the prompt".into(),
        }];
    }
    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zero_workspace_file_limit_is_warned() {
        let mut config = ChelixConfig::default();
        config.chat.workspace_file_max_chars = 0;
        let diagnostics = validate_config(&config);
        assert!(diagnostics.iter().any(|diagnostic| {
            diagnostic.severity == Severity::Warning
                && diagnostic.path == "chat.workspace_file_max_chars"
                && diagnostic.message.contains("AGENTS.md")
        }));
    }

    #[test]
    fn positive_workspace_file_limit_is_accepted() {
        assert!(validate_config(&ChelixConfig::default()).is_empty());
    }
}
