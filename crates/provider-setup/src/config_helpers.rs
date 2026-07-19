//! Configuration merging, auto-detection, and directory helpers.

use std::{
    collections::{BTreeSet, HashMap},
    path::PathBuf,
};

use secrecy::{ExposeSecret, Secret};

use {
    chelix_config::schema::{ModelConfigMap, ProvidersConfig},
    chelix_oauth::TokenStore,
};

use crate::{
    key_store::{KeyStore, ProviderConfig},
    known_providers::{AuthType, known_providers},
    oauth::{codex_cli_auth_has_access_token, codex_cli_auth_path},
};

// ── Config directory helpers ───────────────────────────────────────────────

pub(crate) fn current_config_dir() -> PathBuf {
    chelix_config::config_dir().unwrap_or_else(|| PathBuf::from(".config/chelix"))
}

pub(crate) fn home_config_dir_if_different() -> Option<PathBuf> {
    chelix_config::user_global_config_dir_if_different()
}

pub(crate) fn home_key_store() -> Option<(KeyStore, PathBuf)> {
    let dir = home_config_dir_if_different()?;
    let path = dir.join("provider_keys.json");
    Some((KeyStore::with_path(path.clone()), path))
}

pub(crate) fn home_token_store() -> Option<(TokenStore, PathBuf)> {
    let dir = home_config_dir_if_different()?;
    let path = dir.join("oauth_tokens.json");
    Some((TokenStore::with_path(path.clone()), path))
}

pub(crate) fn home_provider_config() -> Option<(ProvidersConfig, PathBuf)> {
    let path = chelix_config::find_user_global_config_file()?;
    let home_dir = home_config_dir_if_different()?;
    if !path.starts_with(&home_dir) {
        return None;
    }
    let loaded = chelix_config::loader::load_config(&path).ok()?;
    Some((loaded.providers, path))
}

// ── Provider name helpers ──────────────────────────────────────────────────

pub(crate) fn normalize_provider_name(value: &str) -> String {
    chelix_config::normalize_provider_name(value).unwrap_or_default()
}

pub(crate) fn env_value_with_overrides(
    env_overrides: &HashMap<String, String>,
    key: &str,
) -> Option<String> {
    chelix_config::env_value_with_overrides(env_overrides, key)
}

pub(crate) fn set_provider_enabled_in_config(
    provider: &str,
    enabled: bool,
) -> chelix_service_traits::ServiceResult<()> {
    chelix_config::update_config(|cfg| {
        let entry = cfg
            .providers
            .providers
            .entry(provider.to_string())
            .or_default();
        entry.enabled = enabled;
    })
    .map_err(chelix_service_traits::ServiceError::message)?;
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

fn merge_model_maps(preferred: &mut ModelConfigMap, fallback: ModelConfigMap) {
    if preferred.is_empty() {
        *preferred = fallback;
        return;
    }

    preferred.iter_mut().for_each(|(model_id, metadata)| {
        if let Some(fallback_metadata) = fallback.get(model_id) {
            *metadata = std::mem::take(metadata).with_fallback(fallback_metadata.clone());
        }
    });
}

/// Merge persisted provider configs into a ProvidersConfig so the registry rebuild
/// picks them up without needing env vars.
pub fn config_with_saved_keys(base: &ProvidersConfig, key_store: &KeyStore) -> ProvidersConfig {
    let mut config = base.clone();
    if let Some((home_config, _)) = home_provider_config() {
        for (name, entry) in home_config.providers {
            let dst = config.providers.entry(name).or_default();
            if dst
                .api_key
                .as_ref()
                .is_none_or(|k| k.expose_secret().is_empty())
                && let Some(api_key) = entry.api_key
                && !api_key.expose_secret().is_empty()
            {
                dst.api_key = Some(api_key);
            }
            if dst.base_url.is_none()
                && let Some(base_url) = entry.base_url
                && !base_url.trim().is_empty()
            {
                dst.base_url = Some(base_url);
            }
            merge_model_maps(&mut dst.models, entry.models);
        }
    }

    // Merge home key store first, then current key store so current instance
    // values win when both have values.
    let mut saved_configs = HashMap::new();
    if let Some((home_store, _)) = home_key_store() {
        saved_configs.extend(home_store.load_all_configs());
    }
    for (name, saved) in key_store.load_all_configs() {
        let entry = saved_configs
            .entry(name)
            .or_insert_with(ProviderConfig::default);
        if saved.api_key.is_some() {
            entry.api_key = saved.api_key;
        }
        if saved.base_url.is_some() {
            entry.base_url = saved.base_url;
        }
        if !saved.models.is_empty() {
            let fallback = std::mem::take(&mut entry.models);
            entry.models = saved.models;
            merge_model_maps(&mut entry.models, fallback);
        }
    }

    for (name, saved) in saved_configs {
        let entry = config.providers.entry(name).or_default();

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

        merge_model_maps(&mut entry.models, saved.models);
    }

    config
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
) -> Vec<AutoDetectedProviderSource> {
    let is_cloud = deploy_platform.is_some();
    let key_store = KeyStore::new();
    let token_store = TokenStore::new();
    let home_key_store = home_key_store();
    let home_token_store = home_token_store();
    let home_provider_config = home_provider_config();
    let config_dir = current_config_dir();
    let provider_keys_path = config_dir.join("provider_keys.json");
    let oauth_tokens_path = config_dir.join("oauth_tokens.json");
    let codex_path = codex_cli_auth_path();

    let mut seen = BTreeSet::new();
    let mut detected = Vec::new();

    for provider in known_providers().into_iter().filter(|p| {
        if is_cloud {
            return !p.is_local_only();
        }
        true
    }) {
        let mut sources = Vec::new();

        if let Some(env_key) = provider.env_key
            && env_value_with_overrides(env_overrides, env_key).is_some()
        {
            sources.push(format!("env:{env_key}"));
        }
        if provider.auth_type == AuthType::ApiKey
            && let Some(source) = chelix_config::generic_provider_env_source_for_provider(
                provider.name,
                env_overrides,
            )
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

        if home_provider_config
            .as_ref()
            .and_then(|(cfg, _)| cfg.get(provider.name))
            .and_then(|entry| entry.api_key.as_ref())
            .is_some_and(|k| !k.expose_secret().trim().is_empty())
            && let Some((_, path)) = home_provider_config.as_ref()
        {
            sources.push(format!(
                "file:{}:[providers.{}].api_key",
                path.display(),
                provider.name
            ));
        }

        if key_store.load(provider.name).is_some() {
            sources.push(format!("file:{}", provider_keys_path.display()));
        }
        if home_key_store
            .as_ref()
            .is_some_and(|(store, _)| store.load(provider.name).is_some())
            && let Some((_, path)) = home_key_store.as_ref()
        {
            sources.push(format!("file:{}", path.display()));
        }

        if (provider.auth_type == AuthType::Oauth || provider.name == "kimi-code")
            && token_store.load(provider.name).is_some()
        {
            sources.push(format!("file:{}", oauth_tokens_path.display()));
        }
        if (provider.auth_type == AuthType::Oauth || provider.name == "kimi-code")
            && home_token_store
                .as_ref()
                .is_some_and(|(store, _)| store.load(provider.name).is_some())
            && let Some((_, path)) = home_token_store.as_ref()
        {
            sources.push(format!("file:{}", path.display()));
        }

        if provider.name == "openai-codex"
            && codex_path
                .as_deref()
                .is_some_and(codex_cli_auth_has_access_token)
            && let Some(path) = codex_path.as_ref()
        {
            sources.push(format!("file:{}", path.display()));
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

    detected
}

#[allow(clippy::unwrap_used, clippy::expect_used)]
#[cfg(test)]
mod tests {
    use {
        super::*,
        chelix_config::schema::{PartialModelMetadata, PartialReasoningMetadata, ProviderEntry},
    };

    fn model_metadata() -> PartialModelMetadata {
        PartialModelMetadata {
            context_length: Some(128_000),
            max_input_tokens: Some(96_000),
            max_output_tokens: Some(32_000),
            reasoning: Some(PartialReasoningMetadata {
                supported_efforts: Some(Vec::new()),
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    fn model_map(ids: &[&str]) -> ModelConfigMap {
        ids.iter()
            .map(|id| ((*id).to_string(), model_metadata()))
            .collect()
    }

    #[test]
    fn config_with_saved_keys_merges_base_url_and_models() {
        let dir = tempfile::tempdir().unwrap();
        let store = KeyStore::with_path(dir.path().join("keys.json"));
        store
            .save_config(
                "openai",
                Some("sk-saved".into()),
                Some("https://custom.api.com/v1".into()),
                Some(model_map(&["gpt-4o"])),
            )
            .unwrap();

        let base = ProvidersConfig::default();
        let merged = config_with_saved_keys(&base, &store);
        let entry = merged.get("openai").unwrap();
        assert_eq!(
            entry.api_key.as_ref().map(|s| s.expose_secret().as_str()),
            Some("sk-saved")
        );
        assert_eq!(entry.base_url.as_deref(), Some("https://custom.api.com/v1"));
        assert_eq!(
            entry.models.keys().map(String::as_str).collect::<Vec<_>>(),
            vec!["gpt-4o"]
        );
    }

    #[test]
    fn config_model_allowlist_and_fields_take_precedence_over_saved_metadata() {
        let dir = tempfile::tempdir().unwrap();
        let store = KeyStore::with_path(dir.path().join("keys.json"));
        let mut saved_models = model_map(&["gpt-4o", "gpt-5"]);
        saved_models.get_mut("gpt-4o").unwrap().tool_calling = Some(true);
        store
            .save_config("openai", Some("sk-saved".into()), None, Some(saved_models))
            .unwrap();

        let mut configured_metadata = PartialModelMetadata {
            context_length: Some(200_000),
            tool_calling: Some(false),
            ..Default::default()
        };
        configured_metadata.reasoning = Some(PartialReasoningMetadata::default());
        let mut configured_models = ModelConfigMap::new();
        configured_models.insert("gpt-4o".into(), configured_metadata);
        let mut base = ProvidersConfig::default();
        base.providers.insert("openai".into(), ProviderEntry {
            models: configured_models,
            ..Default::default()
        });

        let merged = config_with_saved_keys(&base, &store);
        let models = &merged.get("openai").unwrap().models;
        assert_eq!(models.keys().map(String::as_str).collect::<Vec<_>>(), vec![
            "gpt-4o"
        ]);
        let metadata = models.get("gpt-4o").unwrap();
        assert_eq!(metadata.context_length, Some(200_000));
        assert_eq!(metadata.max_input_tokens, Some(96_000));
        assert_eq!(metadata.max_output_tokens, Some(32_000));
        assert_eq!(metadata.tool_calling, Some(false));
        assert_eq!(
            metadata
                .reasoning
                .as_ref()
                .and_then(|reasoning| reasoning.supported_efforts.as_ref()),
            Some(&Vec::new())
        );
    }

    #[test]
    fn config_with_saved_keys_merges() {
        let dir = tempfile::tempdir().unwrap();
        let store = KeyStore::with_path(dir.path().join("keys.json"));
        store.save("anthropic", "sk-saved").unwrap();

        let base = ProvidersConfig::default();
        let merged = config_with_saved_keys(&base, &store);
        let entry = merged.get("anthropic").unwrap();
        assert_eq!(
            entry.api_key.as_ref().map(|s| s.expose_secret().as_str()),
            Some("sk-saved")
        );
    }

    #[test]
    fn config_with_saved_keys_does_not_override_existing() {
        let dir = tempfile::tempdir().unwrap();
        let store = KeyStore::with_path(dir.path().join("keys.json"));
        store.save("anthropic", "sk-saved").unwrap();

        let mut base = ProvidersConfig::default();
        base.providers.insert("anthropic".into(), ProviderEntry {
            api_key: Some(Secret::new("sk-config".into())),
            ..Default::default()
        });
        let merged = config_with_saved_keys(&base, &store);
        let entry = merged.get("anthropic").unwrap();
        // Config key takes precedence over saved key.
        assert_eq!(
            entry.api_key.as_ref().map(|s| s.expose_secret().as_str()),
            Some("sk-config")
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
        );

        assert!(detected.iter().any(|source| {
            source.provider == "openai"
                && source.source == "env:CHELIX_PROVIDER+CHELIX_API_KEY"
        }));
    }
}
