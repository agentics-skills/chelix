//! Key validation — `validate_key` implementation.

use {secrecy::Secret, serde_json::Value, tracing::info};

use {
    chelix_config::schema::ModelConfigMap,
    chelix_providers::{model_id::namespaced_model_id, resolve_models},
    chelix_service_traits::{ServiceError, ServiceResult},
};

use {
    super::{
        LiveProviderSetupService,
        support::{ProviderSetupTiming, progress_payload},
    },
    crate::{
        config_helpers::normalize_provider_name,
        custom_providers::{is_custom_provider, validation_provider_name_for_endpoint},
        key_store::parse_models_param,
        known_providers::{AuthType, KnownProvider, known_providers},
        provider_base_url::validate_provider_base_url,
    },
};

impl LiveProviderSetupService {
    pub(super) async fn validate_key_inner(&self, params: Value) -> ServiceResult {
        let provider_name = params
            .get("provider")
            .and_then(|v| v.as_str())
            .ok_or_else(|| "missing 'provider' parameter".to_string())?;

        let api_key = params.get("apiKey").and_then(|v| v.as_str());
        let base_url = params.get("baseUrl").and_then(|v| v.as_str());
        let preferred_models = parse_models_param(&params)
            .map_err(ServiceError::message)?
            .unwrap_or_default();
        let request_id = params
            .get("requestId")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|id| !id.is_empty())
            .map(ToString::to_string);
        let saved_config = self.key_store.load_config(provider_name);
        let saved_base_url = saved_config
            .as_ref()
            .and_then(|config| config.base_url.as_deref())
            .filter(|url| !url.trim().is_empty());
        let effective_base_url = base_url
            .filter(|url| !url.trim().is_empty())
            .or(saved_base_url);

        // Custom providers bypass known_providers() validation.
        let is_custom = is_custom_provider(provider_name);
        let provider_info = if is_custom {
            None
        } else {
            let known = known_providers();
            let info = known
                .iter()
                .find(|p| p.name == provider_name)
                .ok_or_else(|| format!("unknown provider: {provider_name}"))?;
            // API key is required for api-key providers unless the provider
            // marks the key as optional (local backends).
            if info.auth_type == AuthType::ApiKey && !info.key_optional && api_key.is_none() {
                return Err("missing 'apiKey' parameter".into());
            }
            Some(KnownProvider {
                name: info.name,
                display_name: info.display_name,
                auth_type: info.auth_type,
                env_key: info.env_key,
                default_base_url: info.default_base_url,
                requires_model: info.requires_model,
                key_optional: info.key_optional,
                local_only: info.local_only,
            })
        };

        if is_custom && api_key.is_none() {
            return Err("missing 'apiKey' parameter".into());
        }
        if is_custom && effective_base_url.is_none() {
            return Err("missing 'baseUrl' parameter".into());
        }
        validate_provider_base_url(effective_base_url).map_err(ServiceError::message)?;

        let selected_model = preferred_models
            .first()
            .map(|(model_id, _)| model_id.as_str());
        let validation_provider_name = validation_provider_name_for_endpoint(
            provider_name,
            provider_info.as_ref().and_then(|p| p.default_base_url),
            effective_base_url,
        );
        let _timing =
            ProviderSetupTiming::start("providers.validate_key", Some(&validation_provider_name));
        self.emit_validation_progress(
            &validation_provider_name,
            request_id.as_deref(),
            "start",
            progress_payload(serde_json::json!({
                "message": "Starting provider validation.",
            })),
        )
        .await;

        // Custom OpenAI-compatible providers: discover models via /v1/models
        // when no model is specified.
        if is_custom && selected_model.is_none() {
            return self
                .validate_custom_discover(
                    provider_name,
                    &validation_provider_name,
                    request_id.as_deref(),
                    api_key.unwrap_or_default(),
                    effective_base_url.unwrap_or_default(),
                )
                .await;
        }

        let normalized_base_url = effective_base_url.map(String::from);

        // Build a temporary ProvidersConfig with just this provider.
        let mut temp_config = chelix_config::schema::ProvidersConfig::default();
        temp_config.providers.insert(
            validation_provider_name.clone(),
            chelix_config::schema::ProviderEntry {
                enabled: true,
                api_key: api_key.map(|k| Secret::new(k.to_string())),
                base_url: normalized_base_url,
                models: preferred_models,
                ..Default::default()
            },
        );

        // Build a temporary registry from the temp config.
        let temp_registry = self.build_registry(&temp_config).await;

        // Filter models for this provider.
        let models: Vec<_> = temp_registry
            .list_models()
            .iter()
            .filter(|m| {
                normalize_provider_name(&m.provider)
                    == normalize_provider_name(&validation_provider_name)
            })
            .cloned()
            .collect();

        if models.is_empty() {
            let error =
                "No models available for this provider. Check your credentials and try again.";
            self.emit_validation_progress(
                &validation_provider_name,
                request_id.as_deref(),
                "error",
                progress_payload(serde_json::json!({
                    "message": error,
                })),
            )
            .await;
            return Ok(serde_json::json!({
                "valid": false,
                "error": error,
            }));
        }

        info!(
            provider = %validation_provider_name,
            model_count = models.len(),
            "provider validation discovered candidate models"
        );

        let model_list: Vec<Value> = models
            .iter()
            .filter(|model| model.supports_text_chat())
            .map(serde_json::to_value)
            .collect::<Result<_, _>>()
            .map_err(ServiceError::message)?;

        self.emit_validation_progress(
            &validation_provider_name,
            request_id.as_deref(),
            "complete",
            progress_payload(serde_json::json!({
                "message": "Validation complete.",
                "modelCount": model_list.len(),
            })),
        )
        .await;
        Ok(serde_json::json!({
            "valid": true,
            "models": model_list,
        }))
    }

    /// Discover models from a custom OpenAI-compatible endpoint.
    async fn validate_custom_discover(
        &self,
        provider_name: &str,
        validation_provider_name: &str,
        request_id: Option<&str>,
        api_key: &str,
        base_url: &str,
    ) -> ServiceResult {
        match chelix_providers::openai::fetch_models_from_api(
            Secret::new(api_key.to_string()),
            base_url.to_string(),
        )
        .await
        {
            Ok(discovered) => {
                let resolved = resolve_models(&ModelConfigMap::new(), discovered);
                let model_list: Vec<Value> = resolved
                    .iter()
                    .filter(|model| {
                        model
                            .metadata
                            .supports_input(chelix_config::schema::ModelModality::Text)
                            && model
                                .metadata
                                .supports_output(chelix_config::schema::ModelModality::Text)
                    })
                    .map(|model| {
                        let mut value =
                            serde_json::to_value(&model.metadata).map_err(ServiceError::message)?;
                        let object = value.as_object_mut().ok_or_else(|| {
                            ServiceError::message("model metadata did not serialize as an object")
                        })?;
                        object.insert(
                            "id".into(),
                            Value::String(namespaced_model_id(provider_name, &model.id)),
                        );
                        object.insert(
                            "display_name".into(),
                            Value::String(model.display_name.clone()),
                        );
                        object.insert("provider".into(), Value::String(provider_name.into()));
                        object.insert(
                            "created_at".into(),
                            model.created_at.map_or(Value::Null, Value::from),
                        );
                        object.insert("recommended".into(), Value::Bool(model.recommended));
                        Ok(value)
                    })
                    .collect::<ServiceResult<Vec<_>>>()?;
                if model_list.is_empty() {
                    let error = "No discovered model has complete mandatory metadata.";
                    self.emit_validation_progress(
                        validation_provider_name,
                        request_id,
                        "error",
                        progress_payload(serde_json::json!({
                            "message": error,
                        })),
                    )
                    .await;
                    return Ok(serde_json::json!({
                        "valid": false,
                        "error": error,
                    }));
                }
                self.emit_validation_progress(
                    validation_provider_name,
                    request_id,
                    "complete",
                    progress_payload(serde_json::json!({
                        "message": "Discovered models from endpoint.",
                        "modelCount": model_list.len(),
                    })),
                )
                .await;
                Ok(serde_json::json!({
                    "valid": true,
                    "models": model_list,
                }))
            },
            Err(err) => {
                let error = format!("Failed to discover models from endpoint: {err}");
                self.emit_validation_progress(
                    validation_provider_name,
                    request_id,
                    "error",
                    progress_payload(serde_json::json!({
                        "message": error.clone(),
                    })),
                )
                .await;
                Ok(serde_json::json!({
                    "valid": false,
                    "error": error,
                }))
            },
        }
    }
}
