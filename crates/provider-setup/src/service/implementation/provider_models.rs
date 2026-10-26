//! Add, replace, and delete provider model records.

use std::{collections::HashSet, sync::Arc};

use {
    chelix_config::schema::{AgentConfig, PartialModelMetadata, ProviderEntry},
    chelix_providers::{ProviderRegistry, model_id::namespaced_model_id},
    chelix_service_traits::{ServiceError, ServiceResult},
    serde_json::Value,
};

use {
    super::{
        config_error::writer_error,
        service::{LiveProviderSetupService, ProviderConfigPersistence},
    },
    crate::model_document::edit_provider_models,
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
        let removed_model_id = removed_canonical_id(&write);
        let memory_agents = if let ProviderConfigPersistence::MemoryOnly = &self.config_persistence
        {
            self.agent_entries().await
        } else {
            Vec::new()
        };
        let mut registry = self.registry.write().await;
        let models = match &self.config_persistence {
            ProviderConfigPersistence::Service(service) => {
                let service = service.clone();
                let provider = write.provider.clone();
                let previous_id = write.previous_model_id.clone();
                let model_id = write.model_id.clone();
                let metadata = write.metadata.clone();
                let priority_id = removed_model_id.clone();
                let validator = agent_candidate_validator("model");
                service
                    .edit_document_checked(
                        move |document| {
                            edit_provider_models(
                                document,
                                &provider,
                                previous_id.as_deref(),
                                &model_id,
                                metadata.as_ref(),
                                priority_id.as_deref(),
                            )
                        },
                        validator,
                    )
                    .await
                    .map_err(writer_error)?
            },
            ProviderConfigPersistence::MemoryOnly => {
                let mut base = self.config_snapshot();
                mutate_provider_models(&mut base, &write)?;
                let candidate = self.build_registry(&base)?;
                let blocked = blocked_agents(&candidate, &memory_agents);
                if !blocked.is_empty() {
                    return Err(ServiceError::message(format!(
                        "model change breaks configured agents: {}",
                        blocked.join(", ")
                    )));
                }
                base.get(&write.provider)
                    .map(|entry| entry.models.clone())
                    .unwrap_or_default()
            },
        };
        let mut base = self.config_snapshot();
        base.providers
            .entry(write.provider.clone())
            .or_default()
            .models = models;
        let candidate = self.build_registry(&base)?;
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
        let renamed = match (
            write.metadata.is_some(),
            write.previous_model_id.as_deref(),
            removed_model_id.as_deref(),
        ) {
            (true, Some(previous), Some(from)) if previous != write.model_id => {
                vec![serde_json::json!({
                    "from": from,
                    "to": canonical_model_id(&write.provider, &write.model_id),
                })]
            },
            _ => Vec::new(),
        };
        let removed = if renamed.is_empty() {
            removed_model_id.into_iter().collect::<Vec<_>>()
        } else {
            Vec::new()
        };
        Ok(serde_json::json!({
            "ok": true,
            "removedModelIds": removed,
            "renamedModelIds": renamed,
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

/// Checks the queued candidate registry against that candidate's configured agents.
pub(super) fn agent_candidate_validator(
    change: &'static str,
) -> Arc<dyn chelix_config_service::ConfigValidator> {
    Arc::new(AgentCandidateValidator { change })
}

struct AgentCandidateValidator {
    change: &'static str,
}

#[async_trait::async_trait]
impl chelix_config_service::ConfigValidator for AgentCandidateValidator {
    async fn validate(
        &self,
        candidate: Arc<chelix_config::ChelixConfig>,
    ) -> Vec<chelix_config::Diagnostic> {
        let agents = candidate
            .agents
            .entries
            .iter()
            .map(|(agent_id, agent)| (agent_id.clone(), agent.clone()))
            .collect::<Vec<_>>();
        let registry = match ProviderRegistry::from_config(&candidate.providers, &candidate.env) {
            Ok(registry) => registry,
            Err(error) => return vec![candidate_diagnostic(error.to_string())],
        };
        let blocked = blocked_agents(&registry, &agents);
        if blocked.is_empty() {
            Vec::new()
        } else {
            vec![candidate_diagnostic(format!(
                "{} change breaks configured agents: {}",
                self.change,
                blocked.join(", ")
            ))]
        }
    }
}

pub(super) fn removed_model_agent_validator(
    provider_name: String,
    removed_model_ids: HashSet<String>,
) -> Arc<dyn chelix_config_service::ConfigValidator> {
    Arc::new(RemovedModelAgentValidator {
        provider_name,
        removed_model_ids,
    })
}

struct RemovedModelAgentValidator {
    provider_name: String,
    removed_model_ids: HashSet<String>,
}

#[async_trait::async_trait]
impl chelix_config_service::ConfigValidator for RemovedModelAgentValidator {
    async fn validate(
        &self,
        candidate: Arc<chelix_config::ChelixConfig>,
    ) -> Vec<chelix_config::Diagnostic> {
        let mut agent_ids = candidate
            .agents
            .entries
            .iter()
            .filter(|(_, agent)| self.removed_model_ids.contains(&agent.model))
            .map(|(agent_id, _)| agent_id.clone())
            .collect::<Vec<_>>();
        agent_ids.sort_unstable();
        if agent_ids.is_empty() {
            Vec::new()
        } else {
            vec![candidate_diagnostic(format!(
                "provider '{}' supplies models configured for agents: {}",
                self.provider_name,
                agent_ids.join(", ")
            ))]
        }
    }
}

fn candidate_diagnostic(message: String) -> chelix_config::Diagnostic {
    chelix_config::Diagnostic {
        severity: chelix_config::Severity::Error,
        category: chelix_config::DiagnosticCategory::InvalidValue,
        path: String::new(),
        message,
        summary_message: None,
    }
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
    registry.supports_chat_model_assignment(&agent.model, &agent.reasoning_effort)
}

#[cfg(test)]
mod tests {
    use {
        chelix_config::{ChelixConfig, DiagnosticCategory, Severity},
        chelix_config_service::ConfigValidator,
    };

    use super::*;

    fn agent(model: &str) -> AgentConfig {
        AgentConfig::new(
            "Agent",
            model,
            chelix_config::schema::ReasoningEffort::from("off"),
        )
    }

    fn assert_rejection(diagnostics: &[chelix_config::Diagnostic], message: &str) {
        assert_eq!(diagnostics.len(), 1);
        assert_eq!(diagnostics[0].severity, Severity::Error);
        assert_eq!(diagnostics[0].category, DiagnosticCategory::InvalidValue);
        assert_eq!(diagnostics[0].path, "");
        assert_eq!(diagnostics[0].message, message);
    }

    #[tokio::test]
    async fn candidate_validator_checks_configured_agent_assignments() {
        let validator = AgentCandidateValidator { change: "model" };
        let mut config = ChelixConfig::default();
        assert!(
            validator
                .validate(Arc::new(config.clone()))
                .await
                .is_empty()
        );
        config
            .agents
            .entries
            .insert("main".into(), agent("test::missing"));
        assert_rejection(
            &validator.validate(Arc::new(config)).await,
            "model change breaks configured agents: main",
        );
    }

    #[tokio::test]
    async fn removal_validator_rejects_only_removed_models_in_sorted_agent_order() {
        let validator = RemovedModelAgentValidator {
            provider_name: "test".into(),
            removed_model_ids: HashSet::from(["test::removed".into()]),
        };
        let mut config = ChelixConfig::default();
        config
            .agents
            .entries
            .insert("kept".into(), agent("test::kept"));
        assert!(
            validator
                .validate(Arc::new(config.clone()))
                .await
                .is_empty()
        );
        config
            .agents
            .entries
            .insert("zulu".into(), agent("test::removed"));
        config
            .agents
            .entries
            .insert("alpha".into(), agent("test::removed"));
        assert_rejection(
            &validator.validate(Arc::new(config)).await,
            "provider 'test' supplies models configured for agents: alpha, zulu",
        );
    }
}
