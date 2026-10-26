use {
    chelix_common::{ModelOverride, ResolvedModelReasoning},
    chelix_config::AgentsConfig,
    chelix_service_traits::{ModelService, ServiceError},
    chelix_sessions::{SessionKey, metadata::EnsureSessionOutcome},
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

pub(crate) async fn ensure_chat_session(
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

    if metadata
        .get(key)
        .await
        .map_err(ServiceError::message)?
        .is_some()
    {
        return Ok(());
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
        .ensure_session(key, None, &model_reasoning, Some(&agent_id))
        .await
        .map_err(ServiceError::message)?
    {
        EnsureSessionOutcome::Created(_) | EnsureSessionOutcome::Existing(_) => Ok(()),
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use {
        super::*, async_trait::async_trait, chelix_common::ReasoningEffort,
        chelix_config::AgentConfig, chelix_service_traits::ServiceResult,
        chelix_sessions::metadata::SqliteSessionMetadata, std::sync::Arc,
    };

    struct ExactModelService;

    #[async_trait]
    impl chelix_service_traits::ModelResolver for ExactModelService {
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
    }

    #[async_trait]
    impl ModelService for ExactModelService {
        async fn list(&self) -> ServiceResult {
            Ok(serde_json::json!([]))
        }

        async fn list_all(&self) -> ServiceResult {
            Ok(serde_json::json!([]))
        }

        async fn disable(&self, _params: serde_json::Value) -> ServiceResult {
            Ok(serde_json::json!({}))
        }

        async fn enable(&self, _params: serde_json::Value) -> ServiceResult {
            Ok(serde_json::json!({}))
        }

        async fn forget_disabled(&self, _model_ids: &[String]) -> ServiceResult {
            Ok(serde_json::json!({}))
        }

        async fn rename_disabled(&self, _pairs: &[(String, String)]) -> ServiceResult {
            Ok(serde_json::json!({}))
        }
    }

    #[derive(Clone, Copy)]
    enum ExistingSession {
        Missing,
        Present,
    }

    struct InitializationCase {
        name: &'static str,
        existing: ExistingSession,
        requested_agent_id: Option<&'static str>,
        expected_model: &'static str,
        expected_agent_id: Option<&'static str>,
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

    fn test_services(
        metadata: Arc<SqliteSessionMetadata>,
        configuration: chelix_config_service::ConfigService,
    ) -> GatewayServices {
        GatewayServices::noop(configuration)
            .with_session_metadata(metadata)
            .with_agents_config(Arc::new(RwLock::new(test_agents())))
            .with_model(Arc::new(ExactModelService))
    }

    fn main_pair() -> ResolvedModelReasoning {
        ResolvedModelReasoning::try_new("test::main".to_string(), ReasoningEffort::from("medium"))
            .unwrap()
    }

    #[tokio::test]
    async fn initializes_only_missing_sessions() {
        let fixture = crate::test_config::ConfigFixture::new().await;
        let cases = [
            InitializationCase {
                name: "missing uses default agent",
                existing: ExistingSession::Missing,
                requested_agent_id: None,
                expected_model: "test::main",
                expected_agent_id: Some("main"),
            },
            InitializationCase {
                name: "missing uses requested agent",
                existing: ExistingSession::Missing,
                requested_agent_id: Some("worker"),
                expected_model: "test::worker",
                expected_agent_id: Some("worker"),
            },
            InitializationCase {
                name: "existing session remains unchanged",
                existing: ExistingSession::Present,
                requested_agent_id: Some("worker"),
                expected_model: "test::main",
                expected_agent_id: Some("main"),
            },
        ];

        for (index, case) in cases.into_iter().enumerate() {
            let metadata = sqlite_metadata().await;
            let key = format!("session:initialization-{index}");
            match case.existing {
                ExistingSession::Missing => {},
                ExistingSession::Present => {
                    metadata
                        .create_session(&key, None, &main_pair(), Some("main"))
                        .await
                        .unwrap();
                },
            }
            let services = test_services(Arc::clone(&metadata), fixture.service.clone());

            ensure_chat_session(
                &services,
                &SessionKey::new(key.clone()),
                case.requested_agent_id,
                None,
            )
            .await
            .unwrap();
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
        }
    }

    #[tokio::test]
    async fn complete_request_override_does_not_initialize_session() {
        let fixture = crate::test_config::ConfigFixture::new().await;
        let metadata = sqlite_metadata().await;
        let services = GatewayServices::noop(fixture.service.clone())
            .with_session_metadata(Arc::clone(&metadata));
        let session_key = SessionKey::new("session:override");
        let model_override = ModelOverride {
            model: "test::override".to_string(),
            reasoning_effort: ReasoningEffort::from("custom"),
        };

        ensure_chat_session(
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
