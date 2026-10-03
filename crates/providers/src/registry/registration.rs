//! Config-only provider registry construction.

use std::{collections::HashMap, sync::Arc};

use {
    anyhow::{Result, anyhow},
    chelix_agents::model::LlmProvider,
    chelix_common::{ModelMetadata, PartialModelMetadata},
    chelix_config::schema::{ProviderEntry, ProvidersConfig},
    secrecy::ExposeSecret,
};

use crate::{model_capabilities::ModelInfo, openai};

use super::ProviderRegistry;

#[derive(Debug, Clone)]
struct ConfiguredModel {
    id: String,
    metadata: ModelMetadata,
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

impl ProviderRegistry {
    /// Build and validate the complete registry without network I/O.
    pub fn from_config(
        config: &ProvidersConfig,
        _env_overrides: &HashMap<String, String>,
    ) -> Result<Self> {
        let resolved = resolve_configured_models(config)?;
        let mut registry = Self::empty();
        registry.register_openai_compatible(config, &resolved);
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

    fn register_openai_compatible(
        &mut self,
        config: &ProvidersConfig,
        resolved: &HashMap<String, Vec<ConfiguredModel>>,
    ) -> usize {
        config
            .providers
            .keys()
            .map(|name| {
                self.register_one_openai_compatible(config, name, models_for(resolved, name))
            })
            .sum()
    }

    fn register_one_openai_compatible(
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

fn configure_openai_transport(
    mut provider: openai::OpenAiProvider,
    entry: &ProviderEntry,
) -> openai::OpenAiProvider {
    provider = provider
        .with_stream_transport(entry.stream_transport)
        .with_tool_mode(entry.tool_mode);
    if !matches!(entry.wire_api, chelix_config::WireApi::ChatCompletions) {
        provider = provider.with_wire_api(entry.wire_api);
    }
    provider
}
