//! Config-first model resolution and provider transport construction.

use std::{collections::HashMap, sync::Arc};

use {
    chelix_agents::model::LlmProvider,
    chelix_common::ModelConfigMap,
    chelix_config::schema::{ProviderEntry, ProviderStreamTransport, ProvidersConfig},
    secrecy::ExposeSecret,
};

use crate::{
    anthropic,
    config_helpers::{env_value, oauth_discovery_enabled, resolve_api_key},
    discovered_model::{ResolvedModel, resolve_models},
    model_capabilities::ModelInfo,
    model_catalogs::{OPENAI_COMPAT_PROVIDERS, OpenAiCompatDef},
    openai,
};

use super::{DiscoveryResult, ProviderRegistry, discover_models};

const OPENAI_DEFAULT_BASE_URL: &str = "https://api.openai.com/v1";

fn resolve_openai_base_url(
    config: &ProvidersConfig,
    env_overrides: &HashMap<String, String>,
) -> (String, bool) {
    if let Some(base_url) = config
        .get("openai")
        .and_then(|entry| entry.base_url.clone())
    {
        return (base_url, true);
    }
    if let Some(base_url) = env_value(env_overrides, "OPENAI_BASE_URL") {
        return (base_url, true);
    }
    (OPENAI_DEFAULT_BASE_URL.into(), false)
}

pub(crate) fn openai_builtin_capabilities(
    base_url_overridden: bool,
) -> openai::OpenAiProviderCapabilities {
    if base_url_overridden {
        return openai::OpenAiProviderCapabilities::DEFAULT;
    }
    openai::OpenAiProviderCapabilities {
        responses_websocket_policy: openai::ResponsesWebSocketPolicy::OpenAiPlatform,
        ..openai::OpenAiProviderCapabilities::DEFAULT
    }
}

fn configured_models<'a>(
    config: &'a ProvidersConfig,
    provider_name: &str,
    empty: &'a ModelConfigMap,
) -> &'a ModelConfigMap {
    config
        .get(provider_name)
        .map(|entry| &entry.models)
        .unwrap_or(empty)
}

fn resolved_models(
    config: &ProvidersConfig,
    discovery: &DiscoveryResult,
    provider_name: &str,
) -> Vec<ResolvedModel> {
    let empty = ModelConfigMap::new();
    resolve_models(
        configured_models(config, provider_name, &empty),
        discovery.models_for(provider_name),
    )
}

impl ProviderRegistry {
    /// Build a registry from complete config-only records without network I/O.
    #[must_use]
    pub fn from_config(config: &ProvidersConfig, env_overrides: &HashMap<String, String>) -> Self {
        Self::from_discovery(config, env_overrides, &DiscoveryResult::empty())
    }

    /// Discover models asynchronously and build a registry from resolved records.
    pub async fn discover(
        config: &ProvidersConfig,
        env_overrides: &HashMap<String, String>,
    ) -> Self {
        let discovery = discover_models(config, env_overrides, None).await;
        Self::from_discovery(config, env_overrides, &discovery)
    }

    /// Build a registry from config and an already fetched discovery snapshot.
    #[must_use]
    pub fn from_discovery(
        config: &ProvidersConfig,
        env_overrides: &HashMap<String, String>,
        discovery: &DiscoveryResult,
    ) -> Self {
        let mut registry = Self::empty();
        registry.register_anthropic(config, env_overrides, discovery);
        registry.register_openai(config, env_overrides, discovery);
        registry.register_openai_compatible(config, env_overrides, discovery);
        registry.register_custom(config, discovery);
        registry.register_openai_codex(config, discovery);
        registry.register_github_copilot(config, discovery);
        registry.register_kimi_code(config, env_overrides, discovery);
        registry
    }

    /// Replace only providers whose discovery request completed successfully.
    ///
    /// Failed requests are absent from `discovery` and leave current entries untouched.
    pub fn refresh_from_discovery(
        &mut self,
        config: &ProvidersConfig,
        env_overrides: &HashMap<String, String>,
        discovery: &DiscoveryResult,
    ) -> usize {
        let previous_ids: std::collections::HashSet<String> =
            self.models.iter().map(|model| model.id.clone()).collect();

        if discovery.models.contains_key("anthropic") {
            self.remove_provider(&provider_label(config, "anthropic"));
            self.register_anthropic(config, env_overrides, discovery);
        }
        if discovery.models.contains_key("openai") {
            self.remove_provider(&provider_label(config, "openai"));
            self.register_openai(config, env_overrides, discovery);
        }
        for definition in OPENAI_COMPAT_PROVIDERS {
            if discovery.models.contains_key(definition.config_name) {
                self.remove_provider(&provider_label(config, definition.config_name));
                self.register_one_openai_compatible(config, env_overrides, discovery, definition);
            }
        }
        for name in config
            .providers
            .keys()
            .filter(|name| name.starts_with("custom-"))
        {
            if discovery.models.contains_key(name) {
                self.remove_provider(name);
                self.register_one_custom(config, discovery, name);
            }
        }
        #[cfg(feature = "provider-openai-codex")]
        if discovery.models.contains_key("openai-codex") {
            self.remove_provider(&provider_label(config, "openai-codex"));
            self.register_openai_codex(config, discovery);
        }
        #[cfg(feature = "provider-github-copilot")]
        if discovery.models.contains_key("github-copilot") {
            self.remove_provider(&provider_label(config, "github-copilot"));
            self.register_github_copilot(config, discovery);
        }

        self.models
            .iter()
            .filter(|model| !previous_ids.contains(&model.id))
            .count()
    }

    fn register_resolved<F>(
        &mut self,
        provider_name: &str,
        models: Vec<ResolvedModel>,
        mut build_provider: F,
    ) -> usize
    where
        F: FnMut(&str) -> Arc<dyn LlmProvider>,
    {
        let pending: Vec<ResolvedModel> = models
            .into_iter()
            .filter(|model| !self.has_provider_model(provider_name, &model.id))
            .collect();
        let count = pending.len();
        pending.into_iter().for_each(|model| {
            let provider = build_provider(&model.id);
            self.register(
                ModelInfo {
                    id: model.id,
                    provider: provider_name.to_string(),
                    display_name: model.display_name,
                    created_at: model.created_at,
                    recommended: model.recommended,
                    metadata: model.metadata,
                },
                provider,
            );
        });
        count
    }

    fn register_anthropic(
        &mut self,
        config: &ProvidersConfig,
        env_overrides: &HashMap<String, String>,
        discovery: &DiscoveryResult,
    ) -> usize {
        if !config.is_enabled("anthropic") {
            return 0;
        }
        let Some(key) = resolve_api_key(config, "anthropic", "ANTHROPIC_API_KEY", env_overrides)
        else {
            return 0;
        };
        let base_url = config
            .get("anthropic")
            .and_then(|entry| entry.base_url.clone())
            .or_else(|| env_value(env_overrides, "ANTHROPIC_BASE_URL"))
            .unwrap_or_else(|| "https://api.anthropic.com".into());
        let alias = config
            .get("anthropic")
            .and_then(|entry| entry.alias.clone());
        let provider_name = alias.clone().unwrap_or_else(|| "anthropic".into());
        let cache_retention = config
            .get("anthropic")
            .map(|entry| entry.cache_retention)
            .unwrap_or_default();
        let models = resolved_models(config, discovery, "anthropic");

        self.register_resolved(&provider_name, models, move |model_id| {
            Arc::new(
                anthropic::AnthropicProvider::with_alias(
                    key.clone(),
                    model_id.to_string(),
                    base_url.clone(),
                    alias.clone(),
                )
                .with_cache_retention(cache_retention),
            )
        })
    }

    fn register_openai(
        &mut self,
        config: &ProvidersConfig,
        env_overrides: &HashMap<String, String>,
        discovery: &DiscoveryResult,
    ) -> usize {
        if !config.is_enabled("openai") {
            return 0;
        }
        let Some(key) = resolve_api_key(config, "openai", "OPENAI_API_KEY", env_overrides) else {
            return 0;
        };
        let (base_url, base_url_overridden) = resolve_openai_base_url(config, env_overrides);
        let capabilities = openai_builtin_capabilities(base_url_overridden);
        let provider_name = provider_label(config, "openai");
        let entry = config.get("openai").cloned().unwrap_or_default();
        let models = resolved_models(config, discovery, "openai");
        let transport_provider_name = provider_name.clone();

        self.register_resolved(&provider_name, models, move |model_id| {
            Arc::new(configure_openai_transport(
                openai::OpenAiProvider::new_with_name(
                    key.clone(),
                    model_id.to_string(),
                    base_url.clone(),
                    transport_provider_name.clone(),
                )
                .with_capabilities(capabilities),
                &entry,
            ))
        })
    }

    fn register_openai_compatible(
        &mut self,
        config: &ProvidersConfig,
        env_overrides: &HashMap<String, String>,
        discovery: &DiscoveryResult,
    ) -> usize {
        OPENAI_COMPAT_PROVIDERS
            .iter()
            .map(|definition| {
                self.register_one_openai_compatible(config, env_overrides, discovery, definition)
            })
            .sum()
    }

    fn register_one_openai_compatible(
        &mut self,
        config: &ProvidersConfig,
        env_overrides: &HashMap<String, String>,
        discovery: &DiscoveryResult,
        definition: &OpenAiCompatDef,
    ) -> usize {
        if !config.is_enabled(definition.config_name) {
            return 0;
        }
        let Some(key) =
            super::discovery::resolve_compatible_api_key(config, definition, env_overrides)
        else {
            return 0;
        };
        let base_url = config
            .get(definition.config_name)
            .and_then(|entry| entry.base_url.clone())
            .or_else(|| env_value(env_overrides, definition.env_base_url_key))
            .unwrap_or_else(|| definition.default_base_url.into());
        let entry = config
            .get(definition.config_name)
            .cloned()
            .unwrap_or_default();
        if definition.local_only {
            let has_explicit_entry = config.get(definition.config_name).is_some();
            let has_env_base_url = env_value(env_overrides, definition.env_base_url_key).is_some();
            if !has_explicit_entry && !has_env_base_url && entry.models.is_empty() {
                return 0;
            }
        }
        let provider_name = provider_label(config, definition.config_name);
        let models = resolved_models(config, discovery, definition.config_name);
        let capabilities = definition.capabilities;
        let transport_provider_name = provider_name.clone();

        self.register_resolved(&provider_name, models, move |model_id| {
            Arc::new(configure_openai_transport(
                openai::OpenAiProvider::new_with_name(
                    key.clone(),
                    model_id.to_string(),
                    base_url.clone(),
                    transport_provider_name.clone(),
                )
                .with_capabilities(capabilities),
                &entry,
            ))
        })
    }

    fn register_custom(&mut self, config: &ProvidersConfig, discovery: &DiscoveryResult) -> usize {
        config
            .providers
            .keys()
            .filter(|name| name.starts_with("custom-"))
            .map(|name| self.register_one_custom(config, discovery, name))
            .sum()
    }

    fn register_one_custom(
        &mut self,
        config: &ProvidersConfig,
        discovery: &DiscoveryResult,
        name: &str,
    ) -> usize {
        let Some(entry) = config.get(name).filter(|entry| entry.enabled) else {
            return 0;
        };
        let Some(api_key) = entry
            .api_key
            .as_ref()
            .filter(|key| !key.expose_secret().is_empty())
        else {
            return 0;
        };
        let Some(base_url) = entry.base_url.as_ref().filter(|url| !url.trim().is_empty()) else {
            return 0;
        };
        let entry = entry.clone();
        let models = resolved_models(config, discovery, name);

        self.register_resolved(name, models, move |model_id| {
            Arc::new(configure_openai_transport(
                openai::OpenAiProvider::new_with_name(
                    api_key.clone(),
                    model_id.to_string(),
                    base_url.clone(),
                    name.to_string(),
                ),
                &entry,
            ))
        })
    }

    #[cfg(feature = "provider-openai-codex")]
    fn register_openai_codex(
        &mut self,
        config: &ProvidersConfig,
        discovery: &DiscoveryResult,
    ) -> usize {
        if !oauth_discovery_enabled(config, "openai-codex")
            || !crate::openai_codex::has_stored_tokens()
        {
            return 0;
        }
        let provider_name = provider_label(config, "openai-codex");
        let transport = config
            .get("openai-codex")
            .map(|entry| entry.stream_transport)
            .unwrap_or(ProviderStreamTransport::Sse);
        let models = resolved_models(config, discovery, "openai-codex");
        self.register_resolved(&provider_name, models, move |model_id| {
            Arc::new(
                crate::openai_codex::OpenAiCodexProvider::new_with_transport(
                    model_id.to_string(),
                    transport,
                ),
            )
        })
    }

    #[cfg(not(feature = "provider-openai-codex"))]
    fn register_openai_codex(
        &mut self,
        _config: &ProvidersConfig,
        _discovery: &DiscoveryResult,
    ) -> usize {
        0
    }

    #[cfg(feature = "provider-github-copilot")]
    fn register_github_copilot(
        &mut self,
        config: &ProvidersConfig,
        discovery: &DiscoveryResult,
    ) -> usize {
        if !oauth_discovery_enabled(config, "github-copilot")
            || !crate::github_copilot::has_stored_tokens()
        {
            return 0;
        }
        let provider_name = provider_label(config, "github-copilot");
        let wire_api = config
            .get("github-copilot")
            .map(|entry| entry.wire_api)
            .unwrap_or_default();
        let models = resolved_models(config, discovery, "github-copilot");
        self.register_resolved(&provider_name, models, move |model_id| {
            Arc::new(crate::github_copilot::GitHubCopilotProvider::new(
                model_id.to_string(),
                wire_api,
            ))
        })
    }

    #[cfg(not(feature = "provider-github-copilot"))]
    fn register_github_copilot(
        &mut self,
        _config: &ProvidersConfig,
        _discovery: &DiscoveryResult,
    ) -> usize {
        0
    }

    #[cfg(feature = "provider-kimi-code")]
    fn register_kimi_code(
        &mut self,
        config: &ProvidersConfig,
        env_overrides: &HashMap<String, String>,
        discovery: &DiscoveryResult,
    ) -> usize {
        if !config.is_enabled("kimi-code") {
            return 0;
        }
        let api_key = resolve_api_key(config, "kimi-code", "KIMI_API_KEY", env_overrides);
        if api_key.is_none() && !crate::kimi_code::has_stored_tokens() {
            return 0;
        }
        let base_url = config
            .get("kimi-code")
            .and_then(|entry| entry.base_url.clone())
            .or_else(|| env_value(env_overrides, "KIMI_BASE_URL"))
            .unwrap_or_else(|| "https://api.kimi.com/coding/v1".into());
        let provider_name = provider_label(config, "kimi-code");
        let models = resolved_models(config, discovery, "kimi-code");

        self.register_resolved(&provider_name, models, move |model_id| {
            if let Some(key) = api_key.as_ref() {
                Arc::new(crate::kimi_code::KimiCodeProvider::new_with_api_key(
                    key.clone(),
                    model_id.to_string(),
                    base_url.clone(),
                ))
            } else {
                Arc::new(crate::kimi_code::KimiCodeProvider::new(
                    model_id.to_string(),
                ))
            }
        })
    }

    #[cfg(not(feature = "provider-kimi-code"))]
    fn register_kimi_code(
        &mut self,
        _config: &ProvidersConfig,
        _env_overrides: &HashMap<String, String>,
        _discovery: &DiscoveryResult,
    ) -> usize {
        0
    }
}

fn provider_label(config: &ProvidersConfig, provider_name: &str) -> String {
    config
        .get(provider_name)
        .and_then(|entry| entry.alias.clone())
        .unwrap_or_else(|| provider_name.to_string())
}

fn configure_openai_transport(
    mut provider: openai::OpenAiProvider,
    entry: &ProviderEntry,
) -> openai::OpenAiProvider {
    provider = provider
        .with_stream_transport(entry.stream_transport)
        .with_cache_retention(entry.cache_retention)
        .with_probe_timeout_secs(entry.probe_timeout_secs);
    if !matches!(entry.wire_api, chelix_config::WireApi::ChatCompletions) {
        provider = provider.with_wire_api(entry.wire_api);
    }
    if !matches!(entry.tool_mode, chelix_config::ToolMode::Auto) {
        provider = provider.with_tool_mode(entry.tool_mode);
    }
    if let Some(strict_tools) = entry.strict_tools {
        provider = provider.with_strict_tools(strict_tools);
    }
    provider
}
