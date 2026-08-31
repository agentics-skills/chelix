use std::sync::Arc;

use {
    chelix_agents::tool_registry::ToolRegistry,
    chelix_config::schema::ReasoningEffort,
    chelix_service_traits::ResolvedModelReasoning,
    chelix_sessions::{metadata::SqliteSessionMetadata, store::SessionStore},
    serde_json::Value,
};

use crate::state::GatewayState;

pub(super) fn register_session_tools(
    tool_registry: &mut ToolRegistry,
    state: &Arc<GatewayState>,
    session_store: &Arc<SessionStore>,
    session_metadata: &Arc<SqliteSessionMetadata>,
) {
    let explore_sessions = build_explore_sessions(Arc::clone(state));
    let create_session = build_create_session(Arc::clone(state), Arc::clone(session_metadata));
    let delete_session = build_delete_session(Arc::clone(state));
    let send_to_session = build_send_to_session(Arc::clone(state));

    tool_registry.register(Box::new(
        chelix_tools::sessions_manage::SessionsExploreTool::new(explore_sessions),
    ));
    tool_registry.register(Box::new(
        chelix_tools::sessions_manage::SessionsCreateTool::new(create_session),
    ));
    tool_registry.register(Box::new(
        chelix_tools::sessions_manage::SessionsDeleteTool::new(
            Arc::clone(session_metadata),
            delete_session,
        ),
    ));
    tool_registry.register(Box::new(
        chelix_tools::sessions_communicate::SessionsListTool::new(Arc::clone(session_metadata)),
    ));
    tool_registry.register(Box::new(
        chelix_tools::sessions_communicate::SessionsHistoryTool::new(
            Arc::clone(session_store),
            Arc::clone(session_metadata),
        ),
    ));
    tool_registry.register(Box::new(
        chelix_tools::sessions_communicate::SessionsSearchTool::new(
            Arc::clone(session_store),
            Arc::clone(session_metadata),
        ),
    ));
    tool_registry.register(Box::new(
        chelix_tools::sessions_communicate::SessionsSendTool::new(
            Arc::clone(session_metadata),
            send_to_session,
        ),
    ));
}

fn build_explore_sessions(
    state: Arc<GatewayState>,
) -> chelix_tools::sessions_manage::ExploreSessionsFn {
    Arc::new(move || {
        let state = Arc::clone(&state);
        Box::pin(async move {
            let agents_config = state.services.agents_config.as_ref().ok_or_else(|| {
                chelix_tools::Error::message("agent configuration is not available")
            })?;
            let guard = agents_config.read().await;
            let default_id = guard.default.clone();
            if !guard.entries.contains_key(&default_id) {
                return Err(chelix_tools::Error::message(format!(
                    "default agent '{default_id}' not found"
                )));
            }
            let mut agents = guard
                .entries
                .iter()
                .map(|(id, agent)| {
                    serde_json::json!({
                        "id": id,
                        "name": agent.name,
                        "description": agent.description,
                        "emoji": agent.emoji,
                        "isDefault": id == &default_id,
                        "model": agent.model,
                        "reasoningEffort": agent.reasoning_effort.as_str(),
                    })
                })
                .collect::<Vec<_>>();
            agents.sort_by(|left, right| {
                let left_id = left.get("id").and_then(Value::as_str).unwrap_or("");
                let right_id = right.get("id").and_then(Value::as_str).unwrap_or("");
                left_id.cmp(right_id)
            });

            Ok(serde_json::json!({
                "defaultAgentId": default_id,
                "agents": agents,
            }))
        })
    })
}

fn build_create_session(
    state: Arc<GatewayState>,
    metadata: Arc<SqliteSessionMetadata>,
) -> chelix_tools::sessions_manage::CreateSessionFn {
    Arc::new(
        move |req: chelix_tools::sessions_manage::CreateSessionRequest| {
            let state = Arc::clone(&state);
            let metadata = Arc::clone(&metadata);
            Box::pin(async move {
                let key = req.key.clone();
                let agent_id = req.agent_id.clone();
                let parent_session_key = req.parent_session_key.clone();

                validate_agent_id(&state, &agent_id).await?;
                let model_reasoning = resolve_model_and_reasoning_effort(
                    &state,
                    &agent_id,
                    req.model_override.as_ref(),
                )
                .await?;

                metadata
                    .create_llm_session(
                        &key,
                        req.label.as_deref(),
                        &model_reasoning,
                        Some(&agent_id),
                    )
                    .await
                    .map_err(|error| chelix_tools::Error::message(error.to_string()))?;

                if let Some(project_id) = req.project_id.as_deref() {
                    metadata
                        .set_project_id(&key, Some(project_id))
                        .await
                        .map_err(|error| chelix_tools::Error::message(error.to_string()))?;
                }
                if let Some(parent) = parent_session_key.as_deref()
                    && parent != key
                    && metadata
                        .get(parent)
                        .await
                        .map_err(|error| chelix_tools::Error::message(error.to_string()))?
                        .is_some()
                {
                    metadata
                        .set_parent(&key, Some(parent), None)
                        .await
                        .map_err(|error| chelix_tools::Error::message(error.to_string()))?;
                }
                let entry = metadata
                    .get(&key)
                    .await
                    .map_err(|error| chelix_tools::Error::message(error.to_string()))?
                    .ok_or_else(|| {
                        chelix_tools::Error::message(format!(
                            "session '{key}' disappeared after create"
                        ))
                    })?;
                Ok(session_entry_payload(entry))
            })
        },
    )
}

fn build_delete_session(
    state: Arc<GatewayState>,
) -> chelix_tools::sessions_manage::DeleteSessionFn {
    Arc::new(
        move |req: chelix_tools::sessions_manage::DeleteSessionRequest| {
            let state = Arc::clone(&state);
            Box::pin(async move {
                state
                    .services
                    .session
                    .delete(serde_json::json!({
                        "key": req.key,
                        "force": req.force,
                    }))
                    .await
                    .map_err(|error| chelix_tools::Error::message(error.to_string()))
            })
        },
    )
}

fn build_send_to_session(
    state: Arc<GatewayState>,
) -> chelix_tools::sessions_communicate::SendToSessionFn {
    Arc::new(
        move |req: chelix_tools::sessions_communicate::SendToSessionRequest| {
            let state = Arc::clone(&state);
            Box::pin(async move {
                let mut params = serde_json::json!({
                    "text": req.message,
                    "_session_key": req.key,
                });
                if let Some(model_override) = req.model_override {
                    let model_reasoning = model_from_override(&state, &model_override).await?;
                    params["model"] = serde_json::json!(model_reasoning.model_id());
                    params["reasoningEffort"] =
                        serde_json::json!(model_reasoning.reasoning_effort().as_str());
                }
                let chat = state.chat();
                if req.wait_for_reply {
                    chat.send_sync(params)
                        .await
                        .map_err(|error| chelix_tools::Error::message(error.to_string()))
                } else {
                    chat.send(params)
                        .await
                        .map_err(|error| chelix_tools::Error::message(error.to_string()))
                }
            })
        },
    )
}

#[tracing::instrument(skip(state))]
async fn validate_agent_id(state: &GatewayState, agent_id: &str) -> chelix_tools::Result<()> {
    let agents_config = state
        .services
        .agents_config
        .as_ref()
        .ok_or_else(|| chelix_tools::Error::message("agent configuration is not available"))?;
    if agents_config.read().await.entries.contains_key(agent_id) {
        return Ok(());
    }
    Err(chelix_tools::Error::message(format!(
        "agent '{agent_id}' not found; call sessions_explore and pass an explicit agent_id"
    )))
}

#[tracing::instrument(skip(state, model_override))]
async fn resolve_model_and_reasoning_effort(
    state: &GatewayState,
    agent_id: &str,
    model_override: Option<&chelix_tools::session_model_override::ModelOverride>,
) -> chelix_tools::Result<ResolvedModelReasoning> {
    let (model, effort) = if let Some(model_override) = model_override {
        (
            model_override.model.clone(),
            model_override.reasoning_effort.clone(),
        )
    } else {
        agent_model_and_reasoning(state, agent_id).await?
    };

    validate_model_and_reasoning_effort(state, &model, &effort).await
}

#[tracing::instrument(skip(state, model_override))]
async fn model_from_override(
    state: &GatewayState,
    model_override: &chelix_tools::session_model_override::ModelOverride,
) -> chelix_tools::Result<ResolvedModelReasoning> {
    validate_model_and_reasoning_effort(
        state,
        &model_override.model,
        &model_override.reasoning_effort,
    )
    .await
}

#[tracing::instrument(skip(state))]
async fn validate_model_and_reasoning_effort(
    state: &GatewayState,
    model: &str,
    reasoning_effort: &ReasoningEffort,
) -> chelix_tools::Result<ResolvedModelReasoning> {
    crate::model_reasoning::resolve_model_reasoning(
        state.services.model.as_ref(),
        model,
        reasoning_effort,
    )
    .await
    .map_err(|error| chelix_tools::Error::message(error.to_string()))
}

#[tracing::instrument(skip(state))]
async fn agent_model_and_reasoning(
    state: &GatewayState,
    agent_id: &str,
) -> chelix_tools::Result<(String, ReasoningEffort)> {
    let agents_config = state.services.agents_config.as_ref().ok_or_else(|| {
        chelix_tools::Error::message(
            "agent configuration is not available; pass model and reasoning_effort explicitly",
        )
    })?;
    let guard = agents_config.read().await;
    let agent = guard
        .get(agent_id)
        .ok_or_else(|| chelix_tools::Error::message(format!("agent '{agent_id}' not found")))?;
    Ok((agent.model.clone(), agent.reasoning_effort.clone()))
}

fn session_entry_payload(entry: chelix_sessions::metadata::SessionEntry) -> Value {
    let model = entry.model().map(str::to_string);
    let reasoning_effort = entry
        .reasoning_effort()
        .map(|effort| effort.as_str().to_string());
    let chelix_sessions::metadata::SessionEntry {
        id,
        key,
        label,
        created_at,
        updated_at,
        message_count,
        project_id,
        parent_session_key,
        agent_id,
        version,
        ..
    } = entry;
    let agent_id = agent_id.as_deref();
    serde_json::json!({
        "entry": {
            "id": id,
            "key": key,
            "label": label,
            "model": model,
            "reasoningEffort": reasoning_effort,
            "createdAt": created_at,
            "updatedAt": updated_at,
            "messageCount": message_count,
            "projectId": project_id,
            "parentSessionKey": parent_session_key,
            "agent_id": agent_id,
            "agentId": agent_id,
            "version": version,
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    use {
        async_trait::async_trait,
        chelix_service_traits::{ModelService, ServiceError, ServiceResult},
        chelix_tools::{
            session_model_override::ModelOverride, sessions_manage::CreateSessionRequest,
        },
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
            if model != "test::valid" || effort.as_str() != "medium" {
                return Err(ServiceError::message(format!(
                    "unsupported pair '{model}' + '{}'",
                    effort.as_str()
                )));
            }
            ResolvedModelReasoning::try_new(model.to_string(), effort.clone())
                .map_err(|error| ServiceError::message(error.to_string()))
        }

        async fn disable(&self, _params: Value) -> ServiceResult {
            Ok(serde_json::json!({}))
        }

        async fn enable(&self, _params: Value) -> ServiceResult {
            Ok(serde_json::json!({}))
        }
    }

    async fn create_test_state() -> Result<
        (
            Arc<GatewayState>,
            Arc<SqliteSessionMetadata>,
            tempfile::TempDir,
        ),
        Box<dyn std::error::Error>,
    > {
        let dir = tempfile::tempdir()?;
        let store = Arc::new(SessionStore::new(dir.path().to_path_buf()));
        let pool = sqlx::SqlitePool::connect("sqlite::memory:").await?;
        chelix_projects::run_migrations(&pool).await?;
        SqliteSessionMetadata::init(&pool).await?;
        let metadata = Arc::new(SqliteSessionMetadata::new(pool));
        let mut agents = chelix_config::AgentsConfig {
            default: "main".to_string(),
            ..Default::default()
        };
        agents.entries.insert(
            "main".to_string(),
            chelix_config::AgentConfig::new("Main", "test::valid", ReasoningEffort::from("medium")),
        );
        let agents_config = Arc::new(tokio::sync::RwLock::new(agents));
        let model_service: Arc<dyn ModelService> = Arc::new(ExactModelService);
        let session_service = crate::session::LiveSessionService::from_router(
            Arc::clone(&store),
            Arc::clone(&metadata),
            Arc::new(chelix_tools::sandbox::SandboxRouter::disabled()),
            Arc::clone(&agents_config),
            Arc::clone(&model_service),
        );
        let services = crate::services::GatewayServices::noop()
            .with_session(Arc::new(session_service))
            .with_model(model_service)
            .with_agents_config(agents_config)
            .with_session_metadata(Arc::clone(&metadata));
        let state = GatewayState::new(crate::auth::resolve_auth(None, None), services);
        Ok((state, metadata, dir))
    }

    fn create_request(key: &str, model: &str) -> CreateSessionRequest {
        CreateSessionRequest {
            key: key.to_string(),
            agent_id: "main".to_string(),
            label: None,
            model_override: Some(ModelOverride {
                model: model.to_string(),
                reasoning_effort: ReasoningEffort::from("medium"),
            }),
            project_id: None,
            parent_session_key: None,
        }
    }

    #[tokio::test]
    async fn model_validation_precedes_session_metadata_mutation()
    -> Result<(), Box<dyn std::error::Error>> {
        let (state, metadata, _dir) = create_test_state().await?;
        let create = build_create_session(state, Arc::clone(&metadata));
        create(create_request("session:valid", "test::valid")).await?;
        let valid_entry = metadata
            .get("session:valid")
            .await?
            .ok_or_else(|| std::io::Error::other("valid session metadata was not created"))?;
        assert_eq!(valid_entry.model(), Some("test::valid"));
        assert_eq!(
            valid_entry.reasoning_effort().map(ReasoningEffort::as_str),
            Some("medium")
        );

        let (state, metadata, _dir) = create_test_state().await?;
        let create = build_create_session(state, Arc::clone(&metadata));
        let invalid = create(create_request("session:invalid", "test::invalid")).await;
        assert!(invalid.is_err());
        assert!(metadata.get("session:invalid").await?.is_none());
        Ok(())
    }
}
