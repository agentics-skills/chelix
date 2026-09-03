use {
    chelix_common::{ModelOverride, ResolvedModelReasoning},
    chelix_config::AgentsConfig,
    chelix_service_traits::{ModelService, ServiceError},
    chelix_sessions::{SessionKey, metadata::EnsureLlmSessionOutcome},
    tokio::sync::RwLock,
};

use crate::services::GatewayServices;

pub(super) async fn default_agent_id(
    agents_config: &RwLock<AgentsConfig>,
) -> Result<String, ServiceError> {
    let agents = agents_config.read().await;
    if agents.default.trim().is_empty() {
        return Err(ServiceError::message("agents.default is not configured"));
    }
    if !agents.entries.contains_key(&agents.default) {
        return Err(ServiceError::message(format!(
            "default agent '{}' not found",
            agents.default
        )));
    }
    Ok(agents.default.clone())
}

pub(super) async fn resolved_agent_pair(
    agents_config: &RwLock<AgentsConfig>,
    model_service: &dyn ModelService,
    agent_id: &str,
) -> Result<ResolvedModelReasoning, ServiceError> {
    let (model, reasoning_effort) = {
        let agents = agents_config.read().await;
        let agent = agents.get(agent_id).ok_or_else(|| {
            ServiceError::message(format!("agent '{agent_id}' is not configured"))
        })?;
        (agent.model.clone(), agent.reasoning_effort.clone())
    };
    model_service
        .resolve_model_reasoning(&model, Some(&reasoning_effort))
        .await
}

fn external_agent_initialization_error(session_key: &str, agent_id: &str) -> ServiceError {
    ServiceError::message(format!(
        "session '{session_key}' is external-only and cannot use agent '{agent_id}' without a complete model/reasoning override"
    ))
}

pub(crate) async fn ensure_internal_chat_session(
    services: &GatewayServices,
    session_key: &SessionKey,
    requested_agent_id: Option<&str>,
    model_override: Option<&ModelOverride>,
) -> Result<(), ServiceError> {
    if model_override.is_some() {
        return Ok(());
    }

    let key = session_key.as_str();
    if key.is_empty() {
        return Err(ServiceError::message("session ID must not be empty"));
    }
    let metadata = services
        .session_metadata
        .as_deref()
        .ok_or_else(|| ServiceError::message("session metadata is not available"))?;

    if let Some(entry) = metadata.get(key).await.map_err(ServiceError::message)? {
        if entry.model_reasoning().is_some() {
            return Ok(());
        }
        let Some(agent_id) = requested_agent_id else {
            return Ok(());
        };
        return Err(external_agent_initialization_error(key, agent_id));
    }

    let agents_config = services
        .agents_config
        .as_deref()
        .ok_or_else(|| ServiceError::message("agent configuration is not available"))?;
    let agent_id = match requested_agent_id {
        Some(agent_id) => agent_id.to_string(),
        None => default_agent_id(agents_config).await?,
    };
    let model_reasoning =
        resolved_agent_pair(agents_config, services.model.as_ref(), &agent_id).await?;

    match metadata
        .ensure_llm_session(key, None, &model_reasoning, Some(&agent_id))
        .await
        .map_err(ServiceError::message)?
    {
        EnsureLlmSessionOutcome::Created(_) | EnsureLlmSessionOutcome::ExistingLlm(_) => Ok(()),
        EnsureLlmSessionOutcome::ExistingExternal(_) if requested_agent_id.is_none() => Ok(()),
        EnsureLlmSessionOutcome::ExistingExternal(_) => {
            Err(external_agent_initialization_error(key, &agent_id))
        },
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use {
        super::*,
        async_trait::async_trait,
        chelix_common::ReasoningEffort,
        chelix_config::AgentConfig,
        chelix_service_traits::ServiceResult,
        chelix_sessions::metadata::{
            ExternalAgentKind, ExternalSessionIdentity, SqliteSessionMetadata,
        },
        std::sync::Arc,
    };

    struct ExactModelService;

    #[async_trait]
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
            if !matches!(
                (model, effort.as_str()),
                ("test::main", "medium") | ("test::worker", "high")
            ) {
                return Err(ServiceError::message(format!(
                    "unsupported pair '{model}' + '{}'",
                    effort.as_str()
                )));
            }
            ResolvedModelReasoning::try_new(model.to_string(), effort.clone())
                .map_err(ServiceError::message)
        }

        async fn disable(&self, _params: serde_json::Value) -> ServiceResult {
            Ok(serde_json::json!({}))
        }

        async fn enable(&self, _params: serde_json::Value) -> ServiceResult {
            Ok(serde_json::json!({}))
        }
    }

    #[derive(Clone, Copy)]
    enum ExistingBacking {
        Missing,
        Llm,
        External,
    }

    struct InitializationCase {
        name: &'static str,
        existing: ExistingBacking,
        requested_agent_id: Option<&'static str>,
        expected_model: Option<&'static str>,
        expected_agent_id: Option<&'static str>,
        expected_error: bool,
    }

    async fn sqlite_metadata() -> Arc<SqliteSessionMetadata> {
        let pool = sqlx::SqlitePool::connect("sqlite::memory:").await.unwrap();
        chelix_projects::run_migrations(&pool).await.unwrap();
        SqliteSessionMetadata::init(&pool).await.unwrap();
        Arc::new(SqliteSessionMetadata::new(pool))
    }

    fn test_agents() -> AgentsConfig {
        let mut agents = AgentsConfig {
            default: "main".to_string(),
            ..Default::default()
        };
        agents.entries.insert(
            "main".to_string(),
            AgentConfig::new("Main", "test::main", ReasoningEffort::from("medium")),
        );
        agents.entries.insert(
            "worker".to_string(),
            AgentConfig::new("Worker", "test::worker", ReasoningEffort::from("high")),
        );
        agents
    }

    fn test_services(metadata: Arc<SqliteSessionMetadata>) -> GatewayServices {
        GatewayServices::noop()
            .with_session_metadata(metadata)
            .with_agents_config(Arc::new(RwLock::new(test_agents())))
            .with_model(Arc::new(ExactModelService))
    }

    fn main_pair() -> ResolvedModelReasoning {
        ResolvedModelReasoning::try_new("test::main".to_string(), ReasoningEffort::from("medium"))
            .unwrap()
    }

    #[tokio::test]
    async fn initializes_only_missing_internal_sessions() {
        let cases = [
            InitializationCase {
                name: "missing uses default agent",
                existing: ExistingBacking::Missing,
                requested_agent_id: None,
                expected_model: Some("test::main"),
                expected_agent_id: Some("main"),
                expected_error: false,
            },
            InitializationCase {
                name: "missing uses requested agent",
                existing: ExistingBacking::Missing,
                requested_agent_id: Some("worker"),
                expected_model: Some("test::worker"),
                expected_agent_id: Some("worker"),
                expected_error: false,
            },
            InitializationCase {
                name: "existing llm session remains unchanged",
                existing: ExistingBacking::Llm,
                requested_agent_id: Some("worker"),
                expected_model: Some("test::main"),
                expected_agent_id: Some("main"),
                expected_error: false,
            },
            InitializationCase {
                name: "external session without requested agent remains external",
                existing: ExistingBacking::External,
                requested_agent_id: None,
                expected_model: None,
                expected_agent_id: None,
                expected_error: false,
            },
            InitializationCase {
                name: "external session rejects requested agent without override",
                existing: ExistingBacking::External,
                requested_agent_id: Some("worker"),
                expected_model: None,
                expected_agent_id: None,
                expected_error: true,
            },
        ];

        for (index, case) in cases.into_iter().enumerate() {
            let metadata = sqlite_metadata().await;
            let key = format!("session:initialization-{index}");
            match case.existing {
                ExistingBacking::Missing => {},
                ExistingBacking::Llm => {
                    metadata
                        .create_llm_session(&key, None, &main_pair(), Some("main"))
                        .await
                        .unwrap();
                },
                ExistingBacking::External => {
                    metadata
                        .bind_external(
                            &key,
                            None,
                            &ExternalSessionIdentity::new(ExternalAgentKind::Codex, None),
                        )
                        .await
                        .unwrap();
                },
            }
            let services = test_services(Arc::clone(&metadata));

            let result = ensure_internal_chat_session(
                &services,
                &SessionKey::new(key.clone()),
                case.requested_agent_id,
                None,
            )
            .await;

            assert_eq!(result.is_err(), case.expected_error, "{}", case.name);
            if case.expected_error {
                assert!(
                    result
                        .expect_err("expected initialization error")
                        .to_string()
                        .contains("external-only"),
                    "{}",
                    case.name
                );
            }
            let entry = metadata
                .get(&key)
                .await
                .unwrap()
                .expect("session must exist after setup or initialization");
            assert_eq!(entry.model(), case.expected_model, "{}", case.name);
            assert_eq!(
                entry.agent_id.as_deref(),
                case.expected_agent_id,
                "{}",
                case.name
            );
            assert_eq!(
                entry.external_agent_kind(),
                matches!(case.existing, ExistingBacking::External)
                    .then_some(ExternalAgentKind::Codex),
                "{}",
                case.name
            );
        }
    }

    #[tokio::test]
    async fn complete_request_override_does_not_initialize_session() {
        let metadata = sqlite_metadata().await;
        let services = GatewayServices::noop().with_session_metadata(Arc::clone(&metadata));
        let session_key = SessionKey::new("session:override");
        let model_override = ModelOverride {
            model: "test::override".to_string(),
            reasoning_effort: ReasoningEffort::from("custom"),
        };

        ensure_internal_chat_session(
            &services,
            &session_key,
            Some("unconfigured-agent"),
            Some(&model_override),
        )
        .await
        .unwrap();

        assert!(metadata.get(session_key.as_str()).await.unwrap().is_none());
    }
}
