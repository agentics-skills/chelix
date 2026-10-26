//! Channel type checks for a parsed Chelix config.

use chelix_config::{
    ChelixConfig, Diagnostic, DiagnosticCategory, Severity, schema::KNOWN_CHANNEL_TYPES,
};

/// Report channel types outside the supported set.
#[must_use]
pub fn validate_config(config: &ChelixConfig) -> Vec<Diagnostic> {
    config
        .channels
        .invalid_channel_types()
        .into_iter()
        .map(|(path, channel_type)| Diagnostic {
            severity: Severity::Error,
            summary_message: None,
            category: DiagnosticCategory::UnknownField,
            path,
            message: format!(
                "unknown channel type \"{channel_type}\"; expected one of: {}",
                KNOWN_CHANNEL_TYPES.join(", ")
            ),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn check(offered: &[&str]) -> Vec<Diagnostic> {
        let mut config = ChelixConfig::default();
        config.channels.offered = offered.iter().map(|name| (*name).to_string()).collect();
        validate_config(&config)
    }

    #[test]
    fn channels_offered_accepted_without_warning() {
        assert!(check(&["telegram"]).is_empty());
    }

    #[test]
    fn channels_offered_telephony_accepted() {
        assert!(check(&["telephony"]).is_empty());
        assert!(check(&["telegram", "telephony"]).is_empty());
    }

    #[test]
    fn channels_offered_unknown_type_rejected() {
        let diagnostics = check(&["telegram", "foobar"]);
        let diagnostic = diagnostics
            .iter()
            .find(|diagnostic| diagnostic.path == "channels.offered[1]")
            .unwrap_or_else(|| panic!("unknown channel type should be rejected"));
        assert_eq!(diagnostic.severity, Severity::Error);
        assert_eq!(diagnostic.category, DiagnosticCategory::UnknownField);
        assert!(diagnostic.message.contains("foobar"));
    }
}
