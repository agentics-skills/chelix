//! Credential management and model preference updates.

use std::collections::{HashMap, HashSet};

use {
    serde_json::Value,
    tracing::{info, warn},
};

use {
    chelix_providers::model_id::raw_model_id,
    chelix_service_traits::{ServiceError, ServiceResult},
};

use {
    super::{LiveProviderSetupService, support::ProviderSetupTiming},
    crate::{
        config_helpers::is_custom_provider, known_providers::known_providers,
        provider_base_url::validate_provider_base_url,
    },
};

impl LiveProviderSetupService {
    pub(super) async fn save_key_inner(&self, params: Value) -> ServiceResult {
        let _timing = ProviderSetupTiming::start(
            "providers.save_key",
            params.get("provider").and_then(Value::as_str),
        );
        let provider_name = params
            .get("provider")
            .and_then(|v| v.as_str())
            .ok_or_else(|| "missing 'provider' parameter".to_string())?;

        if params.get("models").is_some() {
            return Err("unknown 'models' parameter; model records must be configured in the service configuration".into());
        }

        // API key is optional for some providers (e.g., local backends).
        let api_key = params.get("apiKey").and_then(|value| value.as_str());
        let base_url = params.get("baseUrl").and_then(|value| value.as_str());

        // Custom providers bypass known_providers() validation.
        let is_custom = is_custom_provider(provider_name);
        if !is_custom {
            let known = known_providers();
            let provider = known
                .iter()
                .find(|provider| provider.name == provider_name)
                .ok_or_else(|| format!("unknown provider: {provider_name}"))?;

            if !provider.key_optional && api_key.is_none() {
                return Err("missing 'apiKey' parameter".into());
            }
        } else if api_key.is_none() {
            return Err("missing 'apiKey' parameter".into());
        }

        validate_provider_base_url(base_url).map_err(ServiceError::message)?;

        let normalized_base_url = base_url.map(String::from);

        let key_store_path = self.key_store.path();
        info!(
            provider = provider_name,
            has_api_key = api_key.is_some(),
            has_base_url = normalized_base_url
                .as_ref()
                .is_some_and(|url| !url.trim().is_empty()),
            key_store_path = %key_store_path.display(),
            "saving provider config"
        );

        let candidate = self.prospective_config_with_saved_update(
            provider_name,
            api_key,
            normalized_base_url.as_deref(),
            Some(true),
        )?;
        let new_registry = self.build_registry(&candidate)?;

        // Persist only after the complete replacement has been built successfully.
        if let Err(error) = self.key_store.save_config(
            provider_name,
            api_key.map(String::from),
            normalized_base_url,
        ) {
            warn!(
                provider = provider_name,
                key_store_path = %key_store_path.display(),
                error = %error,
                "failed to persist provider config"
            );
            return Err(ServiceError::message(error));
        }
        self.set_provider_enabled(provider_name, true)?;

        let provider_summary = new_registry.provider_summary();
        let model_count = new_registry.list_models().len();
        let mut reg = self.registry.write().await;
        *reg = new_registry;

        info!(
            provider = provider_name,
            provider_summary = %provider_summary,
            models = model_count,
            "saved provider config to disk and rebuilt provider registry"
        );

        Ok(serde_json::json!({ "ok": true }))
    }

    pub(super) async fn remove_key_inner(&self, params: Value) -> ServiceResult {
        let provider_name = params
            .get("provider")
            .and_then(|v| v.as_str())
            .ok_or_else(|| "missing 'provider' parameter".to_string())?;

        let candidate = self.prospective_config_without_saved_provider(provider_name)?;
        let new_registry = self.build_registry(&candidate)?;
        let prospective_model_ids = new_registry
            .list_models()
            .iter()
            .map(|model| model.id.clone())
            .collect::<HashSet<_>>();
        let removed_model_ids = {
            let registry = self.registry.read().await;
            registry
                .list_models()
                .iter()
                .filter(|model| !prospective_model_ids.contains(&model.id))
                .map(|model| model.id.clone())
                .collect::<HashSet<_>>()
        };
        let mut agent_ids = if let Some(agents_config) = self.agents_config.as_ref() {
            let agents = agents_config.read().await;
            agents
                .entries
                .iter()
                .filter(|(_, agent)| removed_model_ids.contains(&agent.model))
                .map(|(agent_id, _)| agent_id.clone())
                .collect::<Vec<_>>()
        } else {
            Vec::new()
        };
        agent_ids.sort_unstable();
        if !agent_ids.is_empty() {
            return Err(ServiceError::message(format!(
                "provider '{provider_name}' supplies models configured for agents: {}",
                agent_ids.join(", ")
            )));
        }

        if is_custom_provider(provider_name) {
            // Custom provider: remove key store entry + disable.
            self.key_store
                .remove(provider_name)
                .map_err(ServiceError::message)?;
            self.set_provider_enabled(provider_name, false)?;
        } else {
            let providers = known_providers();
            providers
                .iter()
                .find(|provider| provider.name == provider_name)
                .ok_or_else(|| format!("unknown provider: {provider_name}"))?;

            self.key_store
                .remove(provider_name)
                .map_err(ServiceError::message)?;

            // Persist explicit disable so auto-detected/global credentials do not
            // immediately re-enable the provider on next rebuild.
            self.set_provider_enabled(provider_name, false)?;
        }

        let mut reg = self.registry.write().await;
        *reg = new_registry;

        info!(
            provider = provider_name,
            "removed provider credentials and rebuilt registry"
        );

        Ok(serde_json::json!({ "ok": true }))
    }

    pub(super) async fn set_model_preferences_inner(&self, params: Value) -> ServiceResult {
        let _timing = ProviderSetupTiming::start(
            "providers.set_model_preferences",
            params.get("provider").and_then(Value::as_str),
        );
        let provider_name = params
            .get("provider")
            .and_then(Value::as_str)
            .ok_or_else(|| "missing 'provider' parameter".to_string())?;
        if params.get("models").is_some() {
            return Err("unknown 'models' parameter; expected canonical 'modelIds'".into());
        }
        let model_values = params
            .get("modelIds")
            .and_then(Value::as_array)
            .ok_or_else(|| "missing 'modelIds' array parameter".to_string())?;
        if model_values.is_empty() {
            return Err("'modelIds' must contain at least one canonical model ID".into());
        }

        let mut model_ids = Vec::with_capacity(model_values.len());
        let mut unique_ids = HashSet::with_capacity(model_values.len());
        for value in model_values {
            let model_id = value
                .as_str()
                .filter(|model_id| !model_id.is_empty())
                .ok_or_else(|| "'modelIds' entries must be non-empty strings".to_string())?;
            if !unique_ids.insert(model_id.to_string()) {
                return Err(format!("duplicate model ID `{model_id}` in 'modelIds'").into());
            }
            model_ids.push(model_id.to_string());
        }

        let (provider_model_ids, provider_raw_model_ids, raw_model_counts) = {
            let registry = self.registry.read().await;
            let mut provider_model_ids = HashSet::new();
            let mut provider_raw_model_ids = HashSet::new();
            let mut raw_model_counts = HashMap::<String, usize>::new();
            for model in registry.list_models() {
                let raw_id = raw_model_id(&model.id).to_string();
                *raw_model_counts.entry(raw_id.clone()).or_default() += 1;
                if model.provider == provider_name {
                    provider_model_ids.insert(model.id.clone());
                    provider_raw_model_ids.insert(raw_id);
                }
            }
            for model_id in &model_ids {
                if !provider_model_ids.contains(model_id) {
                    return Err(format!(
                        "model `{model_id}` is not configured for provider `{provider_name}`"
                    )
                    .into());
                }
            }
            (provider_model_ids, provider_raw_model_ids, raw_model_counts)
        };

        let priority_models = self
            .priority_models
            .as_ref()
            .ok_or_else(|| "model preference service is not configured".to_string())?;
        let current = priority_models.read().await.clone();
        for model_id in &current {
            if provider_raw_model_ids.contains(model_id)
                && raw_model_counts.get(model_id).copied().unwrap_or_default() > 1
            {
                return Err(format!(
                    "chat.priority_models contains ambiguous raw model ID `{model_id}`; replace it with canonical model IDs before updating provider preferences"
                )
                .into());
            }
        }

        let mut next = model_ids.clone();
        for model_id in current {
            if !provider_model_ids.contains(&model_id)
                && !provider_raw_model_ids.contains(&model_id)
                && !unique_ids.contains(&model_id)
            {
                next.push(model_id);
            }
        }

        if self.config_persistence == super::ProviderConfigPersistence::Filesystem {
            let persisted = next.clone();
            chelix_config::update_config(|config| {
                config.chat.priority_models = persisted;
            })
            .map_err(ServiceError::message)?;
        }

        *priority_models.write().await = next;

        info!(
            provider = provider_name,
            count = model_ids.len(),
            model_ids = ?model_ids,
            "saved canonical model preferences"
        );
        Ok(serde_json::json!({ "ok": true }))
    }
}
