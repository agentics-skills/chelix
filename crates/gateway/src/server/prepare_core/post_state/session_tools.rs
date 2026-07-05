use std::sync::Arc;

use {
    moltis_agents::tool_registry::ToolRegistry,
    moltis_config::{AgentPreset, schema::ReasoningEffort},
    moltis_providers::{model_id_with_reasoning_suffix, split_reasoning_suffix},
    moltis_sessions::{metadata::SqliteSessionMetadata, store::SessionStore},
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
        moltis_tools::sessions_manage::SessionsExploreTool::new(explore_sessions),
    ));
    tool_registry.register(Box::new(
        moltis_tools::sessions_manage::SessionsCreateTool::new(
            Arc::clone(session_metadata),
            create_session,
        ),
    ));
    tool_registry.register(Box::new(
        moltis_tools::sessions_manage::SessionsDeleteTool::new(
            Arc::clone(session_metadata),
            delete_session,
        ),
    ));
    tool_registry.register(Box::new(
        moltis_tools::sessions_communicate::SessionsListTool::new(Arc::clone(session_metadata)),
    ));
    tool_registry.register(Box::new(
        moltis_tools::sessions_communicate::SessionsHistoryTool::new(
            Arc::clone(session_store),
            Arc::clone(session_metadata),
        ),
    ));
    tool_registry.register(Box::new(
        moltis_tools::sessions_communicate::SessionsSearchTool::new(
            Arc::clone(session_store),
            Arc::clone(session_metadata),
        ),
    ));
    tool_registry.register(Box::new(
        moltis_tools::sessions_communicate::SessionsSendTool::new(
            Arc::clone(session_metadata),
            send_to_session,
        ),
    ));
}

fn build_explore_sessions(
    state: Arc<GatewayState>,
) -> moltis_tools::sessions_manage::ExploreSessionsFn {
    Arc::new(move || {
        let state = Arc::clone(&state);
        Box::pin(async move {
            let store =
                state.services.agent_persona_store.as_ref().ok_or_else(|| {
                    moltis_tools::Error::message("agent personas are not available")
                })?;
            let (agents, default_id, presets) =
                tokio::join!(store.list(), store.default_id(), agent_presets(&state),);
            let agents = agents.map_err(|error| moltis_tools::Error::message(error.to_string()))?;
            let default_id =
                default_id.map_err(|error| moltis_tools::Error::message(error.to_string()))?;

            let agents = agents
                .into_iter()
                .map(|agent| {
                    let preset = presets.as_ref().and_then(|presets| presets.get(&agent.id));
                    serde_json::json!({
                        "id": agent.id,
                        "name": agent.name,
                        "description": agent.description,
                        "emoji": agent.emoji,
                        "theme": agent.theme,
                        "isDefault": agent.is_default,
                        "model": preset.and_then(|preset| preset.model.clone()),
                        "reasoningEffort": preset.and_then(|preset| preset.reasoning_effort).map(ReasoningEffort::as_str),
                    })
                })
                .collect::<Vec<_>>();

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
) -> moltis_tools::sessions_manage::CreateSessionFn {
    Arc::new(
        move |req: moltis_tools::sessions_manage::CreateSessionRequest| {
            let state = Arc::clone(&state);
            let metadata = Arc::clone(&metadata);
            Box::pin(async move {
                let key = req.key.clone();
                let agent_id = req.agent_id.clone();
                let parent_session_key = req.parent_session_key.clone();

                validate_agent_id(&state, &agent_id).await?;
                let effective_model = if req.created {
                    Some(resolve_effective_model(&state, &agent_id, &req).await?)
                } else {
                    None
                };

                state
                    .services
                    .session
                    .resolve(serde_json::json!({ "key": key.clone() }))
                    .await
                    .map_err(|error| moltis_tools::Error::message(error.to_string()))?;

                if req.created {
                    metadata
                        .set_agent_id(&key, Some(&agent_id))
                        .await
                        .map_err(|error| moltis_tools::Error::message(error.to_string()))?;

                    let mut patch = serde_json::Map::new();
                    patch.insert("key".to_string(), serde_json::json!(key.clone()));
                    if let Some(label) = req.label {
                        patch.insert("label".to_string(), serde_json::json!(label));
                    }
                    if let Some(effective_model) = effective_model {
                        patch.insert("model".to_string(), serde_json::json!(effective_model));
                    }
                    if let Some(project_id) = req.project_id {
                        patch.insert("projectId".to_string(), serde_json::json!(project_id));
                    }
                    state
                        .services
                        .session
                        .patch(Value::Object(patch))
                        .await
                        .map_err(|error| moltis_tools::Error::message(error.to_string()))?;

                    // Link the new session to its creator so the UI renders it as a child.
                    if let Some(parent) = parent_session_key
                        && parent != key
                        && metadata.get(&parent).await.is_some()
                    {
                        metadata.set_parent(&key, Some(parent), None).await;
                    }
                }

                let entry = metadata.get(&key).await.ok_or_else(|| {
                    moltis_tools::Error::message(format!("session '{key}' not found after create"))
                })?;
                Ok(session_entry_payload(entry))
            })
        },
    )
}

fn build_delete_session(
    state: Arc<GatewayState>,
) -> moltis_tools::sessions_manage::DeleteSessionFn {
    Arc::new(
        move |req: moltis_tools::sessions_manage::DeleteSessionRequest| {
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
                    .map_err(|error| moltis_tools::Error::message(error.to_string()))
            })
        },
    )
}

fn build_send_to_session(
    state: Arc<GatewayState>,
) -> moltis_tools::sessions_communicate::SendToSessionFn {
    Arc::new(
        move |req: moltis_tools::sessions_communicate::SendToSessionRequest| {
            let state = Arc::clone(&state);
            Box::pin(async move {
                let mut params = serde_json::json!({
                    "text": req.message,
                    "_session_key": req.key,
                });
                if let Some(model_override) = req.model_override {
                    let model = effective_model_from_override(&state, model_override).await?;
                    params["model"] = serde_json::json!(model);
                }
                let chat = state.chat();
                if req.wait_for_reply {
                    chat.send_sync(params)
                        .await
                        .map_err(|error| moltis_tools::Error::message(error.to_string()))
                } else {
                    chat.send(params)
                        .await
                        .map_err(|error| moltis_tools::Error::message(error.to_string()))
                }
            })
        },
    )
}

#[tracing::instrument(skip(state))]
async fn validate_agent_id(state: &GatewayState, agent_id: &str) -> moltis_tools::Result<()> {
    let store = state
        .services
        .agent_persona_store
        .as_ref()
        .ok_or_else(|| moltis_tools::Error::message("agent personas are not available"))?;
    if store
        .get(agent_id)
        .await
        .map_err(|error| moltis_tools::Error::message(error.to_string()))?
        .is_some()
    {
        return Ok(());
    }
    Err(moltis_tools::Error::message(format!(
        "agent '{agent_id}' not found; call sessions_explore and pass an explicit agent_id"
    )))
}

#[tracing::instrument(skip(state, req))]
async fn resolve_effective_model(
    state: &GatewayState,
    agent_id: &str,
    req: &moltis_tools::sessions_manage::CreateSessionRequest,
) -> moltis_tools::Result<String> {
    let (model, effort) = if let Some(model_override) = req.model_override.clone() {
        (model_override.model, model_override.reasoning_effort)
    } else {
        preset_model_and_reasoning(state, agent_id).await?
    };

    effective_model_from_parts(state, model, effort).await
}

#[tracing::instrument(skip(state, model_override))]
async fn effective_model_from_override(
    state: &GatewayState,
    model_override: moltis_tools::session_model_override::ModelOverride,
) -> moltis_tools::Result<String> {
    effective_model_from_parts(state, model_override.model, model_override.reasoning_effort).await
}

#[tracing::instrument(skip(state))]
async fn effective_model_from_parts(
    state: &GatewayState,
    model: String,
    effort: ReasoningEffort,
) -> moltis_tools::Result<String> {
    if split_reasoning_suffix(&model).1.is_some() {
        return Err(moltis_tools::Error::message(
            "model_override.model must be a base model id without an @reasoning-* suffix",
        ));
    }
    validate_base_model(state, &model).await?;
    Ok(model_id_with_reasoning_suffix(&model, effort))
}

#[tracing::instrument(skip(state))]
async fn preset_model_and_reasoning(
    state: &GatewayState,
    agent_id: &str,
) -> moltis_tools::Result<(String, ReasoningEffort)> {
    let agents_config = state.services.agents_config.as_ref().ok_or_else(|| {
        moltis_tools::Error::message(
            "agent presets are not available; pass model and reasoning_effort explicitly",
        )
    })?;
    let guard = agents_config.read().await;
    let preset = guard.presets.get(agent_id).ok_or_else(|| {
        moltis_tools::Error::message(format!(
            "agent '{agent_id}' has no preset; pass model+reasoning_effort or configure [agents.presets.{agent_id}]"
        ))
    })?;
    let model = preset.model.clone().ok_or_else(|| {
        moltis_tools::Error::message(format!(
            "agent '{agent_id}' has no preset model; pass model+reasoning_effort or configure [agents.presets.{agent_id}].model"
        ))
    })?;
    let effort = preset.reasoning_effort.ok_or_else(|| {
        moltis_tools::Error::message(format!(
            "agent '{agent_id}' has no reasoning_effort; pass model+reasoning_effort or configure [agents.presets.{agent_id}].reasoning_effort"
        ))
    })?;
    Ok((model, effort))
}

#[tracing::instrument(skip(state))]
async fn agent_presets(
    state: &GatewayState,
) -> Option<std::collections::HashMap<String, AgentPreset>> {
    let agents_config = state.services.agents_config.as_ref()?;
    Some(agents_config.read().await.presets.clone())
}

#[tracing::instrument(skip(state))]
async fn validate_base_model(state: &GatewayState, model_id: &str) -> moltis_tools::Result<()> {
    let models = state
        .services
        .model
        .list()
        .await
        .map_err(|error| moltis_tools::Error::message(error.to_string()))?;
    let Some(models) = models.as_array() else {
        return Err(moltis_tools::Error::message(
            "models.list returned an invalid response",
        ));
    };

    let Some(model) = models
        .iter()
        .find(|model| model.get("id").and_then(Value::as_str) == Some(model_id))
    else {
        return Err(moltis_tools::Error::message(format!(
            "model '{model_id}' not found in chat model registry"
        )));
    };

    if !model
        .get("supportsReasoning")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        return Err(moltis_tools::Error::message(format!(
            "model '{model_id}' does not support reasoning_effort"
        )));
    }
    Ok(())
}

fn session_entry_payload(entry: moltis_sessions::metadata::SessionEntry) -> Value {
    let moltis_sessions::metadata::SessionEntry {
        id,
        key,
        label,
        model,
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

    #[test]
    fn model_id_with_reasoning_suffix_builds_virtual_reasoning_id() {
        assert_eq!(
            model_id_with_reasoning_suffix("anthropic::claude", ReasoningEffort::ExtraHigh),
            "anthropic::claude@reasoning-xhigh"
        );
    }
}
