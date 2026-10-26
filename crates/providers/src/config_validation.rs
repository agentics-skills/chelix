//! Provider name and model-metadata checks for a parsed Chelix config.

use chelix_config::{
    ChelixConfig, Diagnostic, DiagnosticCategory, Severity,
    schema::{PartialModelMetadata, openai_compatible_provider_name_error},
};

/// Report unknown provider section names and `providers.offered` entries.
///
/// Names use the OpenAI-compatible slug rules.
#[must_use]
pub fn validate_load_config(config: &ChelixConfig) -> Vec<Diagnostic> {
    config
        .providers
        .invalid_provider_names()
        .into_iter()
        .map(|(path, name)| {
            let message = openai_compatible_provider_name_error(&name)
                .unwrap_or("unknown provider name")
                .to_string();
            Diagnostic {
                severity: Severity::Error,
                summary_message: None,
                category: DiagnosticCategory::UnknownProvider,
                path,
                message,
            }
        })
        .collect()
}

/// Report unknown provider names and incomplete model metadata.
///
/// Section names and `providers.offered` entries use the OpenAI-compatible
/// slug rules. Each configured model must resolve to canonical metadata.
#[must_use]
pub fn validate_config(config: &ChelixConfig) -> Vec<Diagnostic> {
    let mut diagnostics = validate_load_config(config);
    for (provider_name, provider_entry) in &config.providers.providers {
        for (model_id, metadata) in &provider_entry.models {
            validate_model_metadata(
                metadata,
                &format!("providers.{provider_name}.models.{model_id}"),
                &mut diagnostics,
            );
        }
    }
    diagnostics
}

fn validate_model_metadata(
    metadata: &PartialModelMetadata,
    path: &str,
    diagnostics: &mut Vec<Diagnostic>,
) {
    if let Err(error) = metadata.clone().resolve() {
        diagnostics.push(Diagnostic {
            severity: Severity::Error,
            summary_message: None,
            category: DiagnosticCategory::InvalidValue,
            path: path.into(),
            message: error.to_string(),
        });
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use chelix_config::schema::{ModelModality, ProviderEntry, ReasoningEffort};

    use super::*;

    fn check(config: &ChelixConfig) -> Vec<Diagnostic> {
        validate_config(config)
    }

    fn unknown_provider<'a>(diagnostics: &'a [Diagnostic], path: &str) -> Option<&'a Diagnostic> {
        diagnostics.iter().find(|diagnostic| {
            diagnostic.category == DiagnosticCategory::UnknownProvider && diagnostic.path == path
        })
    }

    #[test]
    fn reserved_voice_provider_name_is_rejected() {
        let mut config = ChelixConfig::default();
        config
            .providers
            .providers
            .insert("voice-openai".into(), ProviderEntry::default());
        let diagnostics = check(&config);
        let diagnostic = unknown_provider(&diagnostics, "providers.voice-openai")
            .expect("voice-openai should be rejected");
        assert_eq!(diagnostic.severity, Severity::Error);
        assert!(diagnostic.message.contains("voice- prefix"));
    }

    #[test]
    fn providers_offered_key_not_treated_as_provider_name() {
        let mut config = ChelixConfig::default();
        config.providers.offered = vec!["openai".into(), "openrouter".into()];
        let diagnostics = check(&config);
        assert!(unknown_provider(&diagnostics, "providers.offered").is_none());
        assert!(
            diagnostics
                .iter()
                .all(|diagnostic| diagnostic.category != DiagnosticCategory::UnknownProvider)
        );
    }

    #[test]
    fn unknown_offered_provider_is_rejected() {
        let mut config = ChelixConfig::default();
        config.providers.offered = vec!["openai".into(), "voice-unsupported".into()];
        let diagnostics = check(&config);
        let diagnostic = unknown_provider(&diagnostics, "providers.offered[1]")
            .expect("unsupported offered provider should be rejected");
        assert_eq!(diagnostic.severity, Severity::Error);
    }

    #[test]
    fn noncanonical_offered_provider_names_are_rejected() {
        for name in ["voice-openai", "offered", "bad_name"] {
            let mut config = ChelixConfig::default();
            config.providers.offered = vec![name.into()];
            let diagnostics = check(&config);
            let diagnostic = unknown_provider(&diagnostics, "providers.offered[0]")
                .unwrap_or_else(|| panic!("noncanonical provider {name:?} should be rejected"));
            assert_eq!(diagnostic.severity, Severity::Error);
        }
    }

    #[test]
    fn provider_section_name_with_underscore_is_rejected() {
        let mut config = ChelixConfig::default();
        config
            .providers
            .providers
            .insert("my_custom_llm".into(), ProviderEntry::default());
        let diagnostic = check(&config)
            .into_iter()
            .find(|diagnostic| diagnostic.category == DiagnosticCategory::UnknownProvider)
            .expect("underscore provider name should be rejected");
        assert_eq!(diagnostic.severity, Severity::Error);
        assert!(
            diagnostic
                .message
                .contains("lowercase letters, digits, and hyphens")
        );
    }

    #[test]
    fn openai_compatible_provider_name_is_accepted() {
        let mut config = ChelixConfig::default();
        config
            .providers
            .providers
            .insert("together-ai".into(), ProviderEntry::default());
        assert!(
            check(&config)
                .iter()
                .all(|diagnostic| diagnostic.category != DiagnosticCategory::UnknownProvider)
        );
    }

    #[test]
    fn invalid_provider_name_is_rejected() {
        let mut config = ChelixConfig::default();
        config
            .providers
            .providers
            .insert("voice-typo".into(), ProviderEntry::default());
        let diagnostic = check(&config)
            .into_iter()
            .find(|diagnostic| diagnostic.category == DiagnosticCategory::UnknownProvider)
            .expect("misspelled provider should trigger unknown-provider error");
        assert_eq!(diagnostic.severity, Severity::Error);
    }

    #[test]
    fn incomplete_model_metadata_is_rejected() {
        let mut entry = ProviderEntry::default();
        entry
            .models
            .insert("gpt-5.2".into(), PartialModelMetadata::default());
        let mut config = ChelixConfig::default();
        config.providers.providers.insert("openai".into(), entry);
        let diagnostic = check(&config)
            .into_iter()
            .find(|diagnostic| diagnostic.path == "providers.openai.models.gpt-5.2")
            .expect("incomplete model metadata should be rejected");
        assert_eq!(diagnostic.severity, Severity::Error);
        assert_eq!(diagnostic.category, DiagnosticCategory::InvalidValue);
        assert!(diagnostic.message.contains("missing mandatory"));
    }

    #[test]
    fn complete_model_metadata_is_accepted() {
        let metadata = PartialModelMetadata {
            context_length: Some(8),
            max_input_tokens: Some(4),
            max_output_tokens: Some(4),
            input_modalities: Some(vec![ModelModality::Text]),
            output_modalities: Some(vec![ModelModality::Text]),
            tool_calling: Some(true),
            zero_data_retention_enabled: Some(false),
            reasoning_supported_efforts: Some(vec![ReasoningEffort::from("low")]),
            ..PartialModelMetadata::default()
        };
        let mut entry = ProviderEntry::default();
        entry.models.insert("gpt-5.2".into(), metadata);
        let mut config = ChelixConfig::default();
        config.providers.providers.insert("openai".into(), entry);
        assert!(check(&config).is_empty());
    }
}
