//! Atomic config-only provider registry construction.

use std::{collections::HashMap, sync::Arc};

use {
    anyhow::{Result, anyhow},
    chelix_agents::model::LlmProvider,
    chelix_common::{ModelMetadata, PartialModelMetadata},
    chelix_config::schema::{ProviderEntry, ProvidersConfig},
    secrecy::ExposeSecret,
};

use crate::{
    config_helpers::{env_value, resolve_api_key},
    model_capabilities::ModelInfo,
    model_catalogs::{OPENAI_COMPAT_PROVIDERS, OpenAiCompatDef},
    openai,
};

use super::ProviderRegistry;

const OPENAI_DEFAULT_BASE_URL: &str = "https://api.openai.com/v1";

#[derive(Debug, Clone)]
struct ConfiguredModel {
    id: String,
    metadata: ModelMetadata,
}

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

fn resolve_model(
    provider_name: &str,
    model_id: &str,
    metadata: &PartialModelMetadata,
) -> Result<ConfiguredModel> {
    let metadata = metadata
        .clone()
        .resolve()
        .map_err(|error| anyhow!("provider `{provider_name}` model `{model_id}`: {error}"))?;
    Ok(ConfiguredModel {
        id: model_id.to_string(),
        metadata,
    })
}

fn resolve_configured_models(
    config: &ProvidersConfig,
) -> Result<HashMap<String, Vec<ConfiguredModel>>> {
    let mut resolved = HashMap::with_capacity(config.providers.len());
    for (provider_name, entry) in &config.providers {
        let enabled = config.is_enabled(provider_name);
        let models = entry
            .models
            .iter()
            .map(|(model_id, metadata)| resolve_model(provider_name, model_id, metadata))
            .collect::<Result<Vec<_>>>()?;
        if enabled {
            resolved.insert(provider_name.clone(), models);
        }
    }
    Ok(resolved)
}

fn models_for<'a>(
    resolved: &'a HashMap<String, Vec<ConfiguredModel>>,
    provider_name: &str,
) -> &'a [ConfiguredModel] {
    resolved.get(provider_name).map_or(&[], Vec::as_slice)
}

fn resolve_compatible_api_key(
    config: &ProvidersConfig,
    definition: &OpenAiCompatDef,
    env_overrides: &HashMap<String, String>,
) -> Option<secrecy::Secret<String>> {
    let key = resolve_api_key(
        config,
        definition.config_name,
        definition.env_key,
        env_overrides,
    );
    if definition.requires_api_key {
        return key;
    }
    key.or_else(|| Some(secrecy::Secret::new(definition.config_name.into())))
}

impl ProviderRegistry {
    /// Build and validate the complete registry without network I/O.
    pub fn from_config(
        config: &ProvidersConfig,
        env_overrides: &HashMap<String, String>,
    ) -> Result<Self> {
        let resolved = resolve_configured_models(config)?;
        let mut registry = Self::empty();
        registry.register_openai(config, env_overrides, models_for(&resolved, "openai"));
        registry.register_openai_compatible(config, env_overrides, &resolved);
        registry.register_custom(config, &resolved);
        Ok(registry)
    }

    fn register_configured<F>(
        &mut self,
        provider_name: &str,
        models: &[ConfiguredModel],
        mut build_provider: F,
    ) -> usize
    where
        F: FnMut(&ConfiguredModel) -> Arc<dyn LlmProvider>,
    {
        let pending: Vec<&ConfiguredModel> = models
            .iter()
            .filter(|model| !self.has_provider_model(provider_name, &model.id))
            .collect();
        let count = pending.len();
        pending.into_iter().for_each(|model| {
            let provider = build_provider(model);
            self.register(
                ModelInfo {
                    id: model.id.clone(),
                    provider: provider_name.to_string(),
                    metadata: model.metadata.clone(),
                },
                provider,
            );
        });
        count
    }

    fn register_openai(
        &mut self,
        config: &ProvidersConfig,
        env_overrides: &HashMap<String, String>,
        models: &[ConfiguredModel],
    ) -> usize {
        let Some(entry) = config.get("openai").filter(|_| config.is_enabled("openai")) else {
            return 0;
        };
        let Some(key) = resolve_api_key(config, "openai", "OPENAI_API_KEY", env_overrides) else {
            return 0;
        };
        let (base_url, base_url_overridden) = resolve_openai_base_url(config, env_overrides);
        let capabilities = openai_builtin_capabilities(base_url_overridden);
        let provider_name = provider_label(config, "openai");
        let entry = entry.clone();
        let transport_provider_name = provider_name.clone();

        self.register_configured(&provider_name, models, move |model| {
            Arc::new(configure_openai_transport(
                openai::OpenAiProvider::new_with_name(
                    key.clone(),
                    model.id.clone(),
                    base_url.clone(),
                    transport_provider_name.clone(),
                )
                .with_capabilities(capabilities)
                .with_reasoning_metadata(&model.metadata),
                &entry,
            ))
        })
    }

    fn register_openai_compatible(
        &mut self,
        config: &ProvidersConfig,
        env_overrides: &HashMap<String, String>,
        resolved: &HashMap<String, Vec<ConfiguredModel>>,
    ) -> usize {
        OPENAI_COMPAT_PROVIDERS
            .iter()
            .map(|definition| {
                self.register_one_openai_compatible(
                    config,
                    env_overrides,
                    definition,
                    models_for(resolved, definition.config_name),
                )
            })
            .sum()
    }

    fn register_one_openai_compatible(
        &mut self,
        config: &ProvidersConfig,
        env_overrides: &HashMap<String, String>,
        definition: &OpenAiCompatDef,
        models: &[ConfiguredModel],
    ) -> usize {
        let Some(entry) = config
            .get(definition.config_name)
            .filter(|_| config.is_enabled(definition.config_name))
        else {
            return 0;
        };
        let Some(key) = resolve_compatible_api_key(config, definition, env_overrides) else {
            return 0;
        };
        let base_url = entry
            .base_url
            .clone()
            .or_else(|| env_value(env_overrides, definition.env_base_url_key))
            .unwrap_or_else(|| definition.default_base_url.into());
        let entry = entry.clone();
        let provider_name = provider_label(config, definition.config_name);
        let capabilities = definition.capabilities;
        let transport_provider_name = provider_name.clone();

        self.register_configured(&provider_name, models, move |model| {
            Arc::new(configure_openai_transport(
                openai::OpenAiProvider::new_with_name(
                    key.clone(),
                    model.id.clone(),
                    base_url.clone(),
                    transport_provider_name.clone(),
                )
                .with_capabilities(capabilities)
                .with_reasoning_metadata(&model.metadata),
                &entry,
            ))
        })
    }

    fn register_custom(
        &mut self,
        config: &ProvidersConfig,
        resolved: &HashMap<String, Vec<ConfiguredModel>>,
    ) -> usize {
        config
            .providers
            .keys()
            .filter(|name| name.starts_with("custom-"))
            .map(|name| self.register_one_custom(config, name, models_for(resolved, name)))
            .sum()
    }

    fn register_one_custom(
        &mut self,
        config: &ProvidersConfig,
        name: &str,
        models: &[ConfiguredModel],
    ) -> usize {
        let Some(entry) = config.get(name).filter(|_| config.is_enabled(name)) else {
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

        self.register_configured(name, models, move |model| {
            Arc::new(configure_openai_transport(
                openai::OpenAiProvider::new_with_name(
                    api_key.clone(),
                    model.id.clone(),
                    base_url.clone(),
                    name.to_string(),
                )
                .with_reasoning_metadata(&model.metadata),
                &entry,
            ))
        })
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
        .with_tool_mode(entry.tool_mode);
    if !matches!(entry.wire_api, chelix_config::WireApi::ChatCompletions) {
        provider = provider.with_wire_api(entry.wire_api);
    }
    provider
}
