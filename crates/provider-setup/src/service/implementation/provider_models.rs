//! Add, replace, and delete provider model records.

use {
    chelix_config::schema::{AgentConfig, PartialModelMetadata, ProviderEntry},
    chelix_providers::{ProviderRegistry, model_id::namespaced_model_id},
    chelix_service_traits::{ServiceError, ServiceResult},
    serde_json::Value,
};

use {
    super::service::{LiveProviderSetupService, ProviderConfigPersistence},
    crate::config_helpers::config_with_saved_keys,
};

struct ModelWrite {
    provider: String,
    model_id: String,
    previous_model_id: Option<String>,
    metadata: Option<PartialModelMetadata>,
}

impl LiveProviderSetupService {
    pub(super) async fn upsert_model_inner(&self, params: Value) -> ServiceResult {
        let metadata = params
            .get("metadata")
            .cloned()
            .ok_or_else(|| ServiceError::message("missing 'metadata' parameter"))?;
        let metadata = serde_json::from_value::<PartialModelMetadata>(metadata)
            .map_err(|error| ServiceError::message(format!("invalid model metadata: {error}")))?;
        metadata
            .clone()
            .resolve()
            .map_err(|error| ServiceError::message(error.to_string()))?;
        self.apply_model_write(ModelWrite {
            provider: required_provider(&params)?,
            model_id: required_raw_model_id(&params, "modelId")?,
            previous_model_id: optional_raw_model_id(&params, "previousModelId")?,
            metadata: Some(metadata),
        })
        .await
    }

    pub(super) async fn delete_model_inner(&self, params: Value) -> ServiceResult {
        self.apply_model_write(ModelWrite {
            provider: required_provider(&params)?,
            model_id: required_raw_model_id(&params, "modelId")?,
            previous_model_id: None,
            metadata: None,
        })
        .await
    }

    async fn apply_model_write(&self, write: ModelWrite) -> ServiceResult {
        if !provider_name_allowed(&write.provider) {
            return Err(ServiceError::message(format!(
                "unknown provider: {}",
                write.provider
            )));
        }
        let agents = self.agent_entries().await;
        let removed_model_id = removed_canonical_id(&write);
        let mut registry = self.registry.write().await;
        let models = if self.config_persistence == ProviderConfigPersistence::Filesystem {
            let base = self.config_snapshot();
            let key_store = self.key_store.clone();
            let env_overrides = self.env_overrides.clone();
            let provider_name = write.provider.clone();
            chelix_config::update_provider_model_toml(
                &write.provider,
                write.previous_model_id.as_deref(),
                &write.model_id,
                write.metadata.as_ref(),
                removed_model_id.as_deref(),
                |file_models| {
                    let mut candidate_config = base.clone();
                    candidate_config
                        .providers
                        .entry(provider_name.clone())
                        .or_default()
                        .models = file_models.clone();
                    let merged = config_with_saved_keys(&candidate_config, &key_store)
                        .map_err(|error| chelix_config::Error::message(error.to_string()))?;
                    let candidate = ProviderRegistry::from_config(&merged, &env_overrides)
                        .map_err(|error| chelix_config::Error::message(error.to_string()))?;
                    let blocked = blocked_agents(&candidate, &agents);
                    if !blocked.is_empty() {
                        return Err(chelix_config::Error::message(format!(
                            "model change breaks configured agents: {}",
                            blocked.join(", ")
                        )));
                    }
                    Ok(())
                },
            )
            .map_err(ServiceError::message)?
        } else {
            let mut base = self.config_snapshot();
            mutate_provider_models(&mut base, &write)?;
            let merged = config_with_saved_keys(&base, &self.key_store)?;
            let candidate = self.build_registry(&merged)?;
            let blocked = blocked_agents(&candidate, &agents);
            if !blocked.is_empty() {
                return Err(ServiceError::message(format!(
                    "model change breaks configured agents: {}",
                    blocked.join(", ")
                )));
            }
            base.get(&write.provider)
                .map(|entry| entry.models.clone())
                .unwrap_or_default()
        };
        let mut base = self.config_snapshot();
        base.providers
            .entry(write.provider.clone())
            .or_default()
            .models = models;
        let merged = config_with_saved_keys(&base, &self.key_store)?;
        let candidate = self.build_registry(&merged)?;
        *self
            .config
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = base;
        if let Some(model_id) = removed_model_id.as_deref()
            && let Some(priority_models) = self.priority_models.as_ref()
        {
            priority_models
                .write()
                .await
                .retain(|priority_id| priority_id != model_id);
        }
        *registry = candidate;
        let mut removed = Vec::new();
        if let Some(model_id) = removed_model_id {
            removed.push(model_id);
        }
        Ok(serde_json::json!({
            "ok": true,
            "removedModelIds": removed,
        }))
    }

    pub(super) async fn agent_entries(&self) -> Vec<(String, AgentConfig)> {
        let Some(agents_config) = self.agents_config.as_ref() else {
            return Vec::new();
        };
        let agents = agents_config.read().await;
        agents
            .entries
            .iter()
            .map(|(agent_id, agent)| (agent_id.clone(), agent.clone()))
            .collect()
    }
}

fn provider_name_allowed(name: &str) -> bool {
    chelix_config::schema::openai_compatible_provider_name_error(name).is_none()
}

fn required_provider(params: &Value) -> Result<String, ServiceError> {
    params
        .get("provider")
        .and_then(Value::as_str)
        .filter(|provider| !provider.is_empty())
        .map(str::to_string)
        .ok_or_else(|| ServiceError::message("missing 'provider' parameter"))
}

fn required_raw_model_id(params: &Value, field: &str) -> Result<String, ServiceError> {
    let model_id = params
        .get(field)
        .and_then(Value::as_str)
        .ok_or_else(|| ServiceError::message(format!("missing '{field}' parameter")))?;
    validate_raw_model_id(model_id)
}

fn optional_raw_model_id(params: &Value, field: &str) -> Result<Option<String>, ServiceError> {
    match params.get(field) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => {
            let model_id = value.as_str().ok_or_else(|| {
                ServiceError::message(format!("'{field}' must be a non-empty string"))
            })?;
            validate_raw_model_id(model_id).map(Some)
        },
    }
}

fn validate_raw_model_id(model_id: &str) -> Result<String, ServiceError> {
    if model_id.is_empty() || model_id.contains("::") {
        return Err(ServiceError::message(
            "model id must be a non-empty raw id without '::'",
        ));
    }
    Ok(model_id.to_string())
}

fn mutate_provider_models(
    config: &mut chelix_config::schema::ProvidersConfig,
    write: &ModelWrite,
) -> Result<(), ServiceError> {
    let entry = config.providers.entry(write.provider.clone()).or_default();
    if let Some(metadata) = write.metadata.clone() {
        upsert_metadata(entry, write, metadata)?;
    } else if entry.models.shift_remove(&write.model_id).is_none() {
        return Err(ServiceError::message(format!(
            "model `{}` is not configured for provider `{}`",
            write.model_id, write.provider
        )));
    }
    Ok(())
}

fn upsert_metadata(
    entry: &mut ProviderEntry,
    write: &ModelWrite,
    metadata: PartialModelMetadata,
) -> Result<(), ServiceError> {
    match write.previous_model_id.as_deref() {
        Some(previous) if previous != write.model_id => {
            let Some(index) = entry.models.get_index_of(previous) else {
                return Err(ServiceError::message(format!(
                    "model `{previous}` is not configured for provider `{}`",
                    write.provider
                )));
            };
            if entry.models.contains_key(&write.model_id) {
                return Err(ServiceError::message(format!(
                    "model `{}` is already configured for provider `{}`",
                    write.model_id, write.provider
                )));
            }
            entry.models.shift_remove(previous);
            entry
                .models
                .shift_insert(index, write.model_id.clone(), metadata);
        },
        Some(_) => {
            if !entry.models.contains_key(&write.model_id) {
                return Err(ServiceError::message(format!(
                    "model `{}` is not configured for provider `{}`",
                    write.model_id, write.provider
                )));
            }
            entry.models.insert(write.model_id.clone(), metadata);
        },
        None => {
            if entry.models.contains_key(&write.model_id) {
                return Err(ServiceError::message(format!(
                    "model `{}` is already configured for provider `{}`",
                    write.model_id, write.provider
                )));
            }
            entry.models.insert(write.model_id.clone(), metadata);
        },
    }
    Ok(())
}

fn removed_canonical_id(write: &ModelWrite) -> Option<String> {
    let raw = if write.metadata.is_none() {
        Some(write.model_id.as_str())
    } else {
        write
            .previous_model_id
            .as_deref()
            .filter(|previous| *previous != write.model_id)
    }?;
    Some(canonical_model_id(&write.provider, raw))
}

fn canonical_model_id(provider: &str, model_id: &str) -> String {
    namespaced_model_id(provider, model_id)
}

pub(super) fn blocked_agents(
    registry: &ProviderRegistry,
    agents: &[(String, AgentConfig)],
) -> Vec<String> {
    agents
        .iter()
        .filter(|(_, agent)| !agent_model_ok(registry, agent))
        .map(|(agent_id, _)| agent_id.clone())
        .collect()
}

fn agent_model_ok(registry: &ProviderRegistry, agent: &AgentConfig) -> bool {
    registry
        .list_models()
        .iter()
        .find(|model| model.id == agent.model)
        .is_some_and(|model| {
            model.supports_text_chat()
                && model
                    .metadata
                    .reasoning_supported_efforts
                    .iter()
                    .any(|effort| effort.as_str() == agent.reasoning_effort.as_str())
        })
}
