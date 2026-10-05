//! Configuration merging helpers.

use {chelix_config::schema::ProvidersConfig, secrecy::ExposeSecret};

// ── Provider name helpers ──────────────────────────────────────────────────

pub(crate) fn normalize_provider_name(value: &str) -> String {
    chelix_config::normalize_provider_name(value).unwrap_or_default()
}

// ── Offered provider ordering ──────────────────────────────────────────────

pub(crate) fn ui_offered_provider_order(config: &ProvidersConfig) -> Vec<String> {
    let mut ordered = Vec::new();
    for name in &config.offered {
        let normalized = normalize_provider_name(name);
        if normalized.is_empty()
            || ordered
                .iter()
                .any(|existing: &String| existing == &normalized)
        {
            continue;
        }
        ordered.push(normalized);
    }
    ordered
}

// ── Merge saved keys into config ───────────────────────────────────────────

// ── Explicit settings detection ────────────────────────────────────────────

pub fn has_explicit_provider_settings(config: &ProvidersConfig) -> bool {
    config.providers.values().any(|entry| {
        entry
            .api_key
            .as_ref()
            .is_some_and(|k| !k.expose_secret().trim().is_empty())
            || !entry.models.is_empty()
            || entry
                .base_url
                .as_deref()
                .is_some_and(|url| !url.trim().is_empty())
    })
}

#[allow(clippy::unwrap_used, clippy::expect_used)]
#[cfg(test)]
mod tests {
    use {
        super::*,
        chelix_config::schema::{
            ModelConfigMap, ModelModality, PartialModelMetadata, ProviderEntry,
        },
        secrecy::Secret,
    };

    fn model_metadata() -> PartialModelMetadata {
        PartialModelMetadata {
            context_length: Some(128_000),
            max_input_tokens: Some(96_000),
            max_output_tokens: Some(32_000),
            input_modalities: Some(vec![ModelModality::Text]),
            output_modalities: Some(vec![ModelModality::Text]),
            tool_calling: Some(true),
            zero_data_retention_enabled: Some(false),
            reasoning_supported_efforts: Some(vec!["low".into()]),
            reasoning_summary: None,
            reasoning_include: None,
            enabled: true,
        }
    }

    fn model_map(ids: &[&str]) -> ModelConfigMap {
        ids.iter()
            .map(|id| ((*id).to_string(), model_metadata()))
            .collect()
    }

    #[test]
    fn has_explicit_provider_settings_detects_populated_provider_entries() {
        let mut empty = ProvidersConfig::default();
        assert!(!has_explicit_provider_settings(&empty));

        empty.providers.insert("openai".into(), ProviderEntry {
            api_key: Some(Secret::new("sk-test".into())),
            ..Default::default()
        });
        assert!(has_explicit_provider_settings(&empty));

        let mut model_only = ProvidersConfig::default();
        model_only
            .providers
            .insert("openrouter".into(), ProviderEntry {
                models: model_map(&["z-ai/glm-4.6"]),
                ..Default::default()
            });
        assert!(has_explicit_provider_settings(&model_only));
    }
}
