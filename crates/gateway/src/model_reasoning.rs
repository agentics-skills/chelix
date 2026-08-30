use std::sync::Arc;

use {
    chelix_config::{AgentConfig, AgentsConfig, ChelixConfig, schema::ReasoningEffort},
    chelix_providers::ProviderRegistry,
    chelix_service_traits::{ModelService, ResolvedModelReasoning, ServiceError, ServiceResult},
};

/// Resolve one explicit model/reasoning pair through the canonical model service.
pub async fn resolve_model_reasoning(
    model_service: &dyn ModelService,
    model: &str,
    reasoning_effort: &ReasoningEffort,
) -> ServiceResult<ResolvedModelReasoning> {
    model_service
        .resolve_model_reasoning(model, Some(reasoning_effort))
        .await
}

/// Validate one complete agent configuration without mutating state.
pub async fn validate_agent_config(
    model_service: &dyn ModelService,
    agent_id: &str,
    agent: &AgentConfig,
) -> ServiceResult<ResolvedModelReasoning> {
    resolve_model_reasoning(model_service, &agent.model, &agent.reasoning_effort)
        .await
        .map_err(|error| {
            ServiceError::message(format!(
                "agent '{agent_id}' has invalid model/reasoning configuration: {error}"
            ))
        })
}

/// Validate the exact agents registry state and every configured pair.
pub async fn validate_agents_config(
    model_service: &dyn ModelService,
    agents: &AgentsConfig,
) -> ServiceResult<()> {
    agents
        .resolve_state()
        .map_err(|error| ServiceError::message(error.to_string()))?;

    let mut agent_ids = agents
        .entries
        .keys()
        .map(String::as_str)
        .collect::<Vec<_>>();
    agent_ids.sort_unstable();
    for agent_id in agent_ids {
        let agent = agents
            .entries
            .get(agent_id)
            .ok_or_else(|| ServiceError::message(format!("agent '{agent_id}' disappeared")))?;
        validate_agent_config(model_service, agent_id, agent).await?;
    }
    Ok(())
}

/// Validate candidate agent pairs against the registry that the candidate config will start with.
pub async fn validate_candidate_agents_config(config: &ChelixConfig) -> ServiceResult<()> {
    let key_store = crate::provider_setup::KeyStore::new();
    let effective_providers =
        crate::provider_setup::config_with_saved_keys(&config.providers, &key_store)?;
    let registry = ProviderRegistry::from_config(&effective_providers, &config.env)
        .map_err(ServiceError::message)?;
    let disabled = crate::chat::DisabledModelsStore::load()
        .map_err(|error| ServiceError::message(error.to_string()))?;
    let model_service = crate::chat::LiveModelService::new(
        Arc::new(tokio::sync::RwLock::new(registry)),
        Arc::new(tokio::sync::RwLock::new(disabled)),
        config.chat.priority_models.clone(),
    );
    validate_agents_config(&model_service, &config.agents).await
}

#[cfg(test)]
mod tests {
    use super::*;

    struct ExactModelService;

    #[async_trait::async_trait]
    impl ModelService for ExactModelService {
        async fn list(&self) -> ServiceResult {
            Ok(serde_json::json!([]))
        }

        async fn list_all(&self) -> ServiceResult {
            Ok(serde_json::json!([]))
        }

        async fn resolve_model_reasoning(
            &self,
            model: &str,
            reasoning_effort: Option<&ReasoningEffort>,
        ) -> Result<ResolvedModelReasoning, ServiceError> {
            let effort = reasoning_effort
                .ok_or_else(|| ServiceError::message("reasoning effort is required"))?;
            let supported = matches!(
                (model, effort.as_str()),
                ("test::ordinary", "medium") | ("test::off-only", "off")
            );
            if !supported {
                return Err(ServiceError::message(format!(
                    "unsupported pair '{model}' + '{}'",
                    effort.as_str()
                )));
            }
            ResolvedModelReasoning::try_new(model.to_string(), effort.clone())
                .map_err(|error| ServiceError::message(error.to_string()))
        }

        async fn disable(&self, _params: serde_json::Value) -> ServiceResult {
            Ok(serde_json::json!({}))
        }

        async fn enable(&self, _params: serde_json::Value) -> ServiceResult {
            Ok(serde_json::json!({}))
        }
    }

    #[tokio::test]
    async fn exact_setup_state_requires_no_registry_resolution() -> Result<(), ServiceError> {
        validate_agents_config(&ExactModelService, &AgentsConfig::default()).await?;
        Ok(())
    }

    #[tokio::test]
    async fn configured_agents_accept_ordinary_and_off_efforts() -> Result<(), ServiceError> {
        let mut agents = AgentsConfig {
            default: "ordinary".to_string(),
            ..Default::default()
        };
        agents.entries.insert(
            "ordinary".to_string(),
            AgentConfig::new(
                "Ordinary",
                "test::ordinary",
                ReasoningEffort::from("medium"),
            ),
        );
        agents.entries.insert(
            "off-only".to_string(),
            AgentConfig::new("Off only", "test::off-only", ReasoningEffort::from("off")),
        );

        validate_agents_config(&ExactModelService, &agents).await?;
        Ok(())
    }

    #[tokio::test]
    async fn configured_agents_reject_unknown_or_unsupported_pairs() -> Result<(), ServiceError> {
        let cases = [("unknown::model", "medium"), ("test::ordinary", "off")];

        for (model, effort) in cases {
            let mut agents = AgentsConfig {
                default: "invalid".to_string(),
                ..Default::default()
            };
            agents.entries.insert(
                "invalid".to_string(),
                AgentConfig::new("Invalid", model, ReasoningEffort::from(effort)),
            );

            let Err(error) = validate_agents_config(&ExactModelService, &agents).await else {
                return Err(ServiceError::message("invalid pair was accepted"));
            };
            assert!(error.to_string().contains("agent 'invalid'"));
        }
        Ok(())
    }
}
