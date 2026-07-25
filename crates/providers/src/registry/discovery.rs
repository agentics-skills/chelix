//! Concurrent provider model discovery.

use std::{collections::HashMap, future::Future, pin::Pin};

use {chelix_config::schema::ProvidersConfig, futures::future::join_all, secrecy::ExposeSecret};

use crate::{
    DiscoveredModel, anthropic,
    config_helpers::{
        configured_models_for_provider, env_value, oauth_discovery_enabled, resolve_api_key,
        should_fetch_models,
    },
    model_catalogs::OPENAI_COMPAT_PROVIDERS,
    openai,
};

type DiscoveryFuture = Pin<Box<dyn Future<Output = anyhow::Result<Vec<DiscoveredModel>>> + Send>>;

/// Model records returned by one concurrent discovery pass.
#[derive(Debug, Default)]
pub struct DiscoveryResult {
    pub(crate) models: HashMap<String, Vec<DiscoveredModel>>,
}

impl DiscoveryResult {
    #[must_use]
    pub fn empty() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.models.values().all(Vec::is_empty)
    }

    #[must_use]
    pub(crate) fn models_for(&self, provider: &str) -> Vec<DiscoveredModel> {
        self.models.get(provider).cloned().unwrap_or_default()
    }
}

/// Fetch partial model records from every eligible provider concurrently.
pub async fn discover_models(
    config: &ProvidersConfig,
    env_overrides: &HashMap<String, String>,
    provider_filter: Option<&str>,
) -> DiscoveryResult {
    let filter_matches =
        |name: &str| provider_filter.is_none_or(|filter| filter.eq_ignore_ascii_case(name));
    let mut tasks: Vec<(String, DiscoveryFuture)> = Vec::new();

    push_openai_discovery(&mut tasks, config, env_overrides, &filter_matches);
    push_anthropic_discovery(&mut tasks, config, env_overrides, &filter_matches);
    push_openai_compatible_discoveries(&mut tasks, config, env_overrides, &filter_matches);
    push_custom_discoveries(&mut tasks, config, &filter_matches);
    push_oauth_discoveries(&mut tasks, config, &filter_matches);

    let names: Vec<String> = tasks.iter().map(|(name, _)| name.clone()).collect();
    let futures = tasks.into_iter().map(|(_, future)| future);
    let results = join_all(futures).await;
    let models = names
        .into_iter()
        .zip(results)
        .filter_map(|(provider, result)| match result {
            Ok(models) => {
                tracing::debug!(
                    provider = %provider,
                    model_count = models.len(),
                    "model discovery succeeded"
                );
                Some((provider, models))
            },
            Err(error) => {
                tracing::debug!(
                    provider = %provider,
                    error = %error,
                    "model discovery failed"
                );
                None
            },
        })
        .collect();

    DiscoveryResult { models }
}

fn push_openai_discovery(
    tasks: &mut Vec<(String, DiscoveryFuture)>,
    config: &ProvidersConfig,
    env_overrides: &HashMap<String, String>,
    filter_matches: &impl Fn(&str) -> bool,
) {
    if !filter_matches("openai")
        || !config.is_enabled("openai")
        || cfg!(test)
        || !should_fetch_models(config, "openai")
    {
        return;
    }
    let Some(key) = resolve_api_key(config, "openai", "OPENAI_API_KEY", env_overrides) else {
        return;
    };
    let base_url = config
        .get("openai")
        .and_then(|entry| entry.base_url.clone())
        .or_else(|| env_value(env_overrides, "OPENAI_BASE_URL"))
        .unwrap_or_else(|| "https://api.openai.com/v1".into());
    tasks.push((
        "openai".into(),
        Box::pin(openai::fetch_models_from_api(key, base_url)),
    ));
}

fn push_anthropic_discovery(
    tasks: &mut Vec<(String, DiscoveryFuture)>,
    config: &ProvidersConfig,
    env_overrides: &HashMap<String, String>,
    filter_matches: &impl Fn(&str) -> bool,
) {
    if !filter_matches("anthropic")
        || !config.is_enabled("anthropic")
        || cfg!(test)
        || !should_fetch_models(config, "anthropic")
    {
        return;
    }
    let Some(key) = resolve_api_key(config, "anthropic", "ANTHROPIC_API_KEY", env_overrides) else {
        return;
    };
    let base_url = config
        .get("anthropic")
        .and_then(|entry| entry.base_url.clone())
        .or_else(|| env_value(env_overrides, "ANTHROPIC_BASE_URL"))
        .unwrap_or_else(|| "https://api.anthropic.com".into());
    tasks.push((
        "anthropic".into(),
        Box::pin(anthropic::fetch_models_from_api(key, base_url)),
    ));
}

fn push_openai_compatible_discoveries(
    tasks: &mut Vec<(String, DiscoveryFuture)>,
    config: &ProvidersConfig,
    env_overrides: &HashMap<String, String>,
    filter_matches: &impl Fn(&str) -> bool,
) {
    for definition in OPENAI_COMPAT_PROVIDERS {
        if !filter_matches(definition.config_name)
            || !config.is_enabled(definition.config_name)
            || !should_fetch_models(config, definition.config_name)
        {
            continue;
        }

        let key = resolve_compatible_api_key(config, definition, env_overrides);
        let Some(key) = key else {
            continue;
        };
        let base_url = config
            .get(definition.config_name)
            .and_then(|entry| entry.base_url.clone())
            .or_else(|| env_value(env_overrides, definition.env_base_url_key))
            .unwrap_or_else(|| definition.default_base_url.into());

        if definition.local_only {
            let has_explicit_entry = config.get(definition.config_name).is_some();
            let has_env_base_url = env_value(env_overrides, definition.env_base_url_key).is_some();
            let has_configured_models =
                !configured_models_for_provider(config, definition.config_name).is_empty();
            if !has_explicit_entry && !has_env_base_url && !has_configured_models {
                continue;
            }
        }

        tasks.push((
            definition.config_name.into(),
            Box::pin(openai::fetch_models_from_api(key, base_url)),
        ));
    }
}

fn push_custom_discoveries(
    tasks: &mut Vec<(String, DiscoveryFuture)>,
    config: &ProvidersConfig,
    filter_matches: &impl Fn(&str) -> bool,
) {
    for (name, entry) in &config.providers {
        if !name.starts_with("custom-")
            || !entry.enabled
            || !filter_matches(name)
            || !should_fetch_models(config, name)
        {
            continue;
        }
        let Some(api_key) = entry
            .api_key
            .as_ref()
            .filter(|key| !key.expose_secret().is_empty())
        else {
            continue;
        };
        let Some(base_url) = entry.base_url.as_ref().filter(|url| !url.trim().is_empty()) else {
            continue;
        };
        tasks.push((
            name.clone(),
            Box::pin(openai::fetch_models_from_api(
                api_key.clone(),
                base_url.clone(),
            )),
        ));
    }
}

fn push_oauth_discoveries(
    tasks: &mut Vec<(String, DiscoveryFuture)>,
    config: &ProvidersConfig,
    filter_matches: &impl Fn(&str) -> bool,
) {
    #[cfg(feature = "provider-openai-codex")]
    if filter_matches("openai-codex")
        && oauth_discovery_enabled(config, "openai-codex")
        && should_fetch_models(config, "openai-codex")
        && crate::openai_codex::has_stored_tokens()
    {
        tasks.push((
            "openai-codex".into(),
            Box::pin(crate::openai_codex::fetch_models()),
        ));
    }

    #[cfg(feature = "provider-github-copilot")]
    if filter_matches("github-copilot")
        && oauth_discovery_enabled(config, "github-copilot")
        && should_fetch_models(config, "github-copilot")
        && crate::github_copilot::has_stored_tokens()
    {
        tasks.push((
            "github-copilot".into(),
            Box::pin(crate::github_copilot::fetch_models()),
        ));
    }

    let _ = tasks;
    let _ = config;
    let _ = filter_matches;
}

pub(crate) fn resolve_compatible_api_key(
    config: &ProvidersConfig,
    definition: &crate::model_catalogs::OpenAiCompatDef,
    env_overrides: &HashMap<String, String>,
) -> Option<secrecy::Secret<String>> {
    let key = resolve_api_key(
        config,
        definition.config_name,
        definition.env_key,
        env_overrides,
    );
    if !definition.requires_api_key {
        return key.or_else(|| Some(secrecy::Secret::new(definition.config_name.into())));
    }
    if definition.config_name == "gemini" {
        return key
            .or_else(|| env_value(env_overrides, "GOOGLE_API_KEY").map(secrecy::Secret::new));
    }
    key
}
