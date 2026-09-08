//! Configuration merging, auto-detection, and directory helpers.

use std::{
    collections::{BTreeSet, HashMap},
    path::PathBuf,
};

use secrecy::{ExposeSecret, Secret};

use {
    chelix_config::schema::ProvidersConfig,
    chelix_service_traits::{ServiceError, ServiceResult},
};

use crate::{key_store::KeyStore, known_providers::known_providers};

// ── Config directory helpers ───────────────────────────────────────────────

pub(crate) fn current_config_dir() -> PathBuf {
    chelix_config::config_dir().unwrap_or_else(|| PathBuf::from(".config/chelix"))
}

// ── Provider name helpers ──────────────────────────────────────────────────

pub(crate) fn is_custom_provider(name: &str) -> bool {
    name.starts_with("custom-")
}

pub(crate) fn normalize_provider_name(value: &str) -> String {
    chelix_config::normalize_provider_name(value).unwrap_or_default()
}

pub(crate) fn env_value_with_overrides(
    env_overrides: &HashMap<String, String>,
    key: &str,
) -> Option<String> {
    chelix_config::env_value_with_overrides(env_overrides, key)
}

pub(crate) fn set_provider_enabled_in_config(provider: &str, enabled: bool) -> ServiceResult<()> {
    chelix_config::update_config(|cfg| {
        let entry = cfg
            .providers
            .providers
            .entry(provider.to_string())
            .or_default();
        entry.enabled = enabled;
    })
    .map_err(ServiceError::message)?;
    Ok(())
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

pub(crate) fn ui_offered_provider_set(offered_order: &[String]) -> Option<BTreeSet<String>> {
    let offered: BTreeSet<String> = offered_order.iter().cloned().collect();
    (!offered.is_empty()).then_some(offered)
}

// ── Merge saved keys into config ───────────────────────────────────────────

/// Merge persisted LLM provider credentials into provider entries declared in
/// the service configuration. Other credential-store namespaces remain available
/// to their owning subsystems and do not create LLM provider entries.
pub fn config_with_saved_keys(
    base: &ProvidersConfig,
    key_store: &KeyStore,
) -> ServiceResult<ProvidersConfig> {
    let mut config = base.clone();

    for (name, saved) in key_store
        .load_all_configs()
        .map_err(ServiceError::message)?
    {
        let Some(entry) = config.providers.get_mut(&name) else {
            continue;
        };

        // Only override API key if config doesn't already have one.
        if let Some(key) = saved.api_key
            && entry
                .api_key
                .as_ref()
                .is_none_or(|k| k.expose_secret().is_empty())
        {
            entry.api_key = Some(Secret::new(key));
        }

        // Only override base_url if config doesn't already have one.
        if let Some(url) = saved.base_url
            && entry.base_url.is_none()
        {
            entry.base_url = Some(url);
        }
    }

    Ok(config)
}

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

// ── Auto-detected provider source ──────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AutoDetectedProviderSource {
    pub provider: String,
    pub source: String,
}

pub fn detect_auto_provider_sources_with_overrides(
    config: &ProvidersConfig,
    deploy_platform: Option<&str>,
    env_overrides: &HashMap<String, String>,
) -> ServiceResult<Vec<AutoDetectedProviderSource>> {
    let is_cloud = deploy_platform.is_some();
    let key_store = KeyStore::new();
    let config_dir = current_config_dir();
    let provider_keys_path = config_dir.join("provider_keys.json");

    let mut seen = BTreeSet::new();
    let mut detected = Vec::new();

    for provider in known_providers().into_iter().filter(|p| {
        if is_cloud {
            return !p.is_local_only();
        }
        true
    }) {
        let mut sources = Vec::new();

        let env_key = provider.env_key;
        if env_value_with_overrides(env_overrides, env_key).is_some() {
            sources.push(format!("env:{env_key}"));
        }
        if let Some(source) =
            chelix_config::generic_provider_env_source_for_provider(provider.name, env_overrides)
        {
            sources.push(source);
        }

        if config
            .get(provider.name)
            .and_then(|entry| entry.api_key.as_ref())
            .is_some_and(|k| !k.expose_secret().trim().is_empty())
        {
            sources.push(format!("config:[providers.{}].api_key", provider.name));
        }

        if key_store
            .load(provider.name)
            .map_err(ServiceError::message)?
            .is_some()
        {
            sources.push(format!("file:{}", provider_keys_path.display()));
        }

        for source in sources {
            if seen.insert((provider.name.to_string(), source.clone())) {
                detected.push(AutoDetectedProviderSource {
                    provider: provider.name.to_string(),
                    source,
                });
            }
        }
    }

    Ok(detected)
}

#[allow(clippy::unwrap_used, clippy::expect_used)]
#[cfg(test)]
mod tests {
    use {
        super::*,
        chelix_config::schema::{
            ModelConfigMap, ModelModality, PartialModelMetadata, ProviderEntry,
        },
    };

    #[test]
    fn custom_provider_prefix_is_explicit() {
        assert!(is_custom_provider("custom-example"));
        assert!(!is_custom_provider("openai"));
    }

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
        }
    }

    fn model_map(ids: &[&str]) -> ModelConfigMap {
        ids.iter()
            .map(|id| ((*id).to_string(), model_metadata()))
            .collect()
    }

    #[test]
    fn config_with_saved_keys_merges_credentials_without_changing_models() {
        let dir = tempfile::tempdir().unwrap();
        let store = KeyStore::with_path(dir.path().join("keys.json"));
        store
            .save_config(
                "openai",
                Some("sk-saved".into()),
                Some("https://custom.api.com/v1".into()),
            )
            .unwrap();

        let mut base = ProvidersConfig::default();
        base.providers.insert("openai".into(), ProviderEntry {
            models: model_map(&["gpt-4o"]),
            ..Default::default()
        });
        let merged = config_with_saved_keys(&base, &store).expect("merge saved keys");
        let entry = merged.get("openai").unwrap();
        assert_eq!(
            entry
                .api_key
                .as_ref()
                .map(|key| key.expose_secret().as_str()),
            Some("sk-saved")
        );
        assert_eq!(entry.base_url.as_deref(), Some("https://custom.api.com/v1"));
        assert_eq!(
            entry.models.keys().map(String::as_str).collect::<Vec<_>>(),
            vec!["gpt-4o"]
        );
    }

    #[test]
    fn config_with_saved_keys_does_not_create_provider_entries() {
        let dir = tempfile::tempdir().unwrap();
        let store = KeyStore::with_path(dir.path().join("keys.json"));
        store.save("openrouter", "saved-key").unwrap();
        store.save("voice-elevenlabs", "voice-key").unwrap();
        store.save("phone_twilio", "phone-key").unwrap();

        let base = ProvidersConfig::default();
        let merged = config_with_saved_keys(&base, &store).expect("merge saved keys");
        assert!(merged.providers.is_empty());
    }

    #[test]
    fn config_with_saved_keys_does_not_override_existing() {
        let dir = tempfile::tempdir().unwrap();
        let store = KeyStore::with_path(dir.path().join("keys.json"));
        store.save("openrouter", "saved-key").unwrap();

        let mut base = ProvidersConfig::default();
        base.providers.insert("openrouter".into(), ProviderEntry {
            api_key: Some(Secret::new("config-key".into())),
            ..Default::default()
        });
        let merged = config_with_saved_keys(&base, &store).expect("merge saved keys");
        let entry = merged.get("openrouter").unwrap();
        // Config key takes precedence over saved key.
        assert_eq!(
            entry.api_key.as_ref().map(|s| s.expose_secret().as_str()),
            Some("config-key")
        );
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

    #[test]
    fn detect_auto_provider_sources_includes_generic_provider_env() {
        let detected = detect_auto_provider_sources_with_overrides(
            &ProvidersConfig::default(),
            None,
            &HashMap::from([
                ("CHELIX_PROVIDER".to_string(), "openai".to_string()),
                (
                    "CHELIX_API_KEY".to_string(),
                    "sk-test-openai-generic".to_string(),
                ),
            ]),
        )
        .expect("detect provider sources");

        assert!(detected.iter().any(|source| {
            source.provider == "openai" && source.source == "env:CHELIX_PROVIDER+CHELIX_API_KEY"
        }));
    }
}
