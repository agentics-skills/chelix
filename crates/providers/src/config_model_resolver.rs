use std::{collections::HashSet, sync::Arc};

use {
    async_trait::async_trait,
    chelix_config::{ChelixConfig, Diagnostic, DiagnosticCategory, Severity},
    chelix_config_service::ConfigValidator,
    chelix_service_traits::{ModelResolver, ResolvedModelReasoning, ServiceError, ServiceResult},
};

use crate::ProviderRegistry;

#[must_use]
pub fn disabled_model_ids(config: &ChelixConfig) -> HashSet<String> {
    config
        .providers
        .providers
        .iter()
        .flat_map(|(provider, entry)| {
            entry
                .models
                .iter()
                .filter(|(_, metadata)| !metadata.enabled)
                .map(move |(model, _)| format!("{provider}::{model}"))
        })
        .collect()
}

pub struct ConfigModelResolver {
    registry: ProviderRegistry,
    disabled: HashSet<String>,
}

impl ConfigModelResolver {
    pub fn from_config(config: &ChelixConfig) -> ServiceResult<Self> {
        Ok(Self {
            registry: ProviderRegistry::from_config(&config.providers, &config.env)
                .map_err(ServiceError::message)?,
            disabled: disabled_model_ids(config),
        })
    }
}

#[async_trait]
impl ModelResolver for ConfigModelResolver {
    async fn resolve_model_reasoning(
        &self,
        model: &str,
        reasoning_effort: Option<&chelix_common::ReasoningEffort>,
    ) -> ServiceResult<ResolvedModelReasoning> {
        self.registry
            .resolve_chat_model(model, reasoning_effort, &self.disabled)
    }
}

pub struct ModelConfigValidator;

#[async_trait]
impl ConfigValidator for ModelConfigValidator {
    async fn validate(&self, candidate: Arc<ChelixConfig>) -> Vec<Diagnostic> {
        let result = match ConfigModelResolver::from_config(&candidate) {
            Ok(resolver) => {
                chelix_agents::model_validation::validate_agents_config(
                    &resolver,
                    &candidate.agents,
                )
                .await
            },
            Err(error) => Err(error),
        };
        match result {
            Ok(()) => Vec::new(),
            Err(error) => vec![Diagnostic {
                severity: Severity::Error,
                category: DiagnosticCategory::InvalidValue,
                path: "agents".into(),
                message: error.to_string(),
                summary_message: None,
            }],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn candidate_model_validation_accepts_setup_and_rejects_unknown_agent_model() {
        let mut config = ChelixConfig::default();
        assert!(
            ModelConfigValidator
                .validate(Arc::new(config.clone()))
                .await
                .is_empty()
        );
        config.agents.default = "main".into();
        config.agents.entries.insert(
            "main".into(),
            chelix_config::AgentConfig::new(
                "Main",
                "test::missing",
                chelix_common::ReasoningEffort::from("off"),
            ),
        );
        let diagnostics = ModelConfigValidator.validate(Arc::new(config)).await;
        assert_eq!(diagnostics.len(), 1);
        assert_eq!(diagnostics[0].severity, Severity::Error);
        assert_eq!(diagnostics[0].category, DiagnosticCategory::InvalidValue);
        assert_eq!(diagnostics[0].path, "agents");
        assert!(
            diagnostics[0]
                .message
                .starts_with("agent 'main' has invalid model/reasoning configuration: ")
        );
    }
}
