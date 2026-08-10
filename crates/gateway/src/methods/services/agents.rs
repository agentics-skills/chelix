use super::*;

#[cfg(feature = "agent")]
use crate::session_reasoning::agent_defaults_for_agent;

#[cfg(feature = "agent")]
#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct SaveAgentParams {
    id: String,
    agent: chelix_config::AgentConfig,
    #[serde(default)]
    soul: Option<String>,
    #[serde(default)]
    subagent_prompt: Option<String>,
}

pub(super) fn register(reg: &mut MethodRegistry) {
    reg.register(
        "agent",
        Box::new(|ctx| {
            Box::pin(async move {
                ctx.state
                    .services
                    .agent
                    .run(ctx.params.clone())
                    .await
                    .map_err(ErrorShape::from)
            })
        }),
    );
    reg.register(
        "agent.wait",
        Box::new(|ctx| {
            Box::pin(async move {
                ctx.state
                    .services
                    .agent
                    .run_wait(ctx.params.clone())
                    .await
                    .map_err(ErrorShape::from)
            })
        }),
    );

    #[cfg(not(feature = "agent"))]
    reg.register(
        "agents.list",
        Box::new(|ctx| {
            Box::pin(async move {
                ctx.state
                    .services
                    .agent
                    .list()
                    .await
                    .map_err(ErrorShape::from)
            })
        }),
    );

    #[cfg(feature = "agent")]
    register_agent_config_methods(reg);
}

#[cfg(feature = "agent")]
fn register_agent_config_methods(reg: &mut MethodRegistry) {
    reg.register(
        "agents.list",
        Box::new(|ctx| {
            Box::pin(async move {
                let agents = agents_config_for_ctx(&ctx)?;
                let guard = agents.read().await;
                let mut entries = guard
                    .entries
                    .iter()
                    .map(|(id, agent)| agent_payload(id, agent, id == &guard.default))
                    .collect::<Result<Vec<_>, _>>()?;
                entries.sort_by(|left, right| {
                    let left_id = left
                        .get("id")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or("");
                    let right_id = right
                        .get("id")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or("");
                    left_id.cmp(right_id)
                });
                Ok(serde_json::json!({
                    "default_id": guard.default,
                    "agents": entries,
                    "defaults": {
                        "max_tools_threshold": chelix_config::schema::DEFAULT_MAX_TOOLS_THRESHOLD,
                    },
                }))
            })
        }),
    );

    reg.register(
        "agents.get",
        Box::new(|ctx| {
            Box::pin(async move {
                let id = required_agent_id(&ctx.params)?;
                let agents = agents_config_for_ctx(&ctx)?;
                let guard = agents.read().await;
                let agent = guard.get(&id).ok_or_else(|| agent_not_found(&id))?;
                agent_payload(&id, agent, id == guard.default)
            })
        }),
    );

    reg.register(
        "agents.create",
        Box::new(|ctx| {
            Box::pin(async move {
                let params = parse_save_agent_params(ctx.params.clone())?;
                validate_agent_id(&params.id)?;
                validate_agent_config(&params.agent)?;
                let id = params.id.clone();
                let agent = params.agent.clone();
                persist_agents_config(&ctx, move |config| {
                    if config.agents.entries.contains_key(&id) {
                        return Err(chelix_config::Error::message(format!(
                            "agent '{id}' already exists"
                        )));
                    }
                    config.agents.entries.insert(id, agent);
                    Ok(())
                })
                .await?;
                save_agent_prompts(
                    &params.id,
                    params.soul.as_deref(),
                    params.subagent_prompt.as_deref(),
                )?;

                let agents = agents_config_for_ctx(&ctx)?;
                let guard = agents.read().await;
                let agent = guard
                    .get(&params.id)
                    .ok_or_else(|| agent_not_found(&params.id))?;
                agent_payload(&params.id, agent, params.id == guard.default)
            })
        }),
    );

    reg.register(
        "agents.update",
        Box::new(|ctx| {
            Box::pin(async move {
                let params = parse_save_agent_params(ctx.params.clone())?;
                validate_agent_id(&params.id)?;
                validate_agent_config(&params.agent)?;
                let id = params.id.clone();
                let agent = params.agent.clone();
                persist_agents_config(&ctx, move |config| {
                    let Some(entry) = config.agents.entries.get_mut(&id) else {
                        return Err(chelix_config::Error::message(format!(
                            "agent '{id}' not found"
                        )));
                    };
                    *entry = agent;
                    Ok(())
                })
                .await?;
                save_agent_prompts(
                    &params.id,
                    params.soul.as_deref(),
                    params.subagent_prompt.as_deref(),
                )?;

                let agents = agents_config_for_ctx(&ctx)?;
                let guard = agents.read().await;
                let agent = guard
                    .get(&params.id)
                    .ok_or_else(|| agent_not_found(&params.id))?;
                agent_payload(&params.id, agent, params.id == guard.default)
            })
        }),
    );

    reg.register(
        "agents.delete",
        Box::new(|ctx| {
            Box::pin(async move {
                let id = required_agent_id(&ctx.params)?;
                let deleted_id = id.clone();
                persist_agents_config(&ctx, move |config| {
                    if config.agents.default == deleted_id {
                        return Err(chelix_config::Error::message(format!(
                            "cannot delete default agent '{deleted_id}'; select another default first"
                        )));
                    }
                    if config.agents.entries.remove(&deleted_id).is_none() {
                        return Err(chelix_config::Error::message(format!(
                            "agent '{deleted_id}' not found"
                        )));
                    }
                    Ok(())
                })
                .await?;

                let default_id = default_agent_id_for_ctx(&ctx).await?;
                let mut reassigned_sessions = 0_u64;
                if let Some(metadata) = &ctx.state.services.session_metadata {
                    let sessions = metadata.list_by_agent_id(&id).await.map_err(|error| {
                        ErrorShape::new(error_codes::UNAVAILABLE, error.to_string())
                    })?;
                    for session in sessions {
                        metadata
                            .set_agent_id(&session.key, Some(&default_id))
                            .await
                            .map_err(|error| {
                                ErrorShape::new(error_codes::UNAVAILABLE, error.to_string())
                            })?;
                        reassigned_sessions = reassigned_sessions.saturating_add(1);
                    }
                }

                let workspace = chelix_config::agent_workspace_dir(&id);
                if workspace.exists() {
                    std::fs::remove_dir_all(&workspace).map_err(|error| {
                        ErrorShape::new(error_codes::UNAVAILABLE, error.to_string())
                    })?;
                }

                Ok(serde_json::json!({
                    "deleted": true,
                    "reassigned_sessions": reassigned_sessions,
                    "default_id": default_id,
                }))
            })
        }),
    );

    reg.register(
        "agents.set_default",
        Box::new(|ctx| {
            Box::pin(async move {
                let id = required_agent_id(&ctx.params)?;
                let new_default = id.clone();
                persist_agents_config(&ctx, move |config| {
                    if !config.agents.entries.contains_key(&new_default) {
                        return Err(chelix_config::Error::message(format!(
                            "agent '{new_default}' not found"
                        )));
                    }
                    config.agents.default = new_default;
                    Ok(())
                })
                .await?;
                Ok(serde_json::json!({ "ok": true, "default_id": id }))
            })
        }),
    );

    reg.register(
        "agents.set_session",
        Box::new(|ctx| {
            Box::pin(async move {
                let session_key = ctx
                    .params
                    .get("session_key")
                    .and_then(serde_json::Value::as_str)
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .ok_or_else(|| {
                        ErrorShape::new(
                            error_codes::INVALID_REQUEST,
                            "missing 'session_key' parameter",
                        )
                    })?;
                let agent_id = if parse_agent_id_param(&ctx.params).is_some() {
                    let id = required_agent_id(&ctx.params)?;
                    ensure_agent_exists_for_ctx(&ctx, &id).await?;
                    id
                } else {
                    default_agent_id_for_ctx(&ctx).await?
                };
                let metadata = ctx
                    .state
                    .services
                    .session_metadata
                    .as_ref()
                    .ok_or_else(|| {
                        ErrorShape::new(error_codes::UNAVAILABLE, "session metadata not available")
                    })?;
                metadata.upsert(session_key, None).await.map_err(|error| {
                    ErrorShape::new(error_codes::UNAVAILABLE, error.to_string())
                })?;
                let (agent_model, agent_reasoning) =
                    agent_defaults_for_agent(&ctx.state, Some(&agent_id)).await;
                let entry = metadata
                    .assign_agent_with_defaults(
                        session_key,
                        &agent_id,
                        agent_model.as_deref(),
                        agent_reasoning.as_deref(),
                    )
                    .await
                    .map_err(|error| {
                        ErrorShape::new(error_codes::UNAVAILABLE, error.to_string())
                    })?;
                Ok(serde_json::json!({
                    "ok": true,
                    "agent_id": agent_id,
                    "model": entry.model,
                    "reasoningEffort": entry.reasoning_effort,
                    "version": entry.version,
                }))
            })
        }),
    );

    register_agent_file_methods(reg);
}

#[cfg(feature = "agent")]
fn register_agent_file_methods(reg: &mut MethodRegistry) {
    reg.register(
        "agents.files.list",
        Box::new(|ctx| {
            Box::pin(async move {
                let agent_id = resolve_requested_agent_id(&ctx, &ctx.params).await?;
                let limit_chars = workspace_file_limit_chars(&ctx);
                let mut files = Vec::new();
                let root = chelix_config::agent_workspace_dir(&agent_id);
                if root.exists() {
                    list_agent_workspace_files_recursively(&root, &root, &mut files);
                }
                for file_name in &["AGENTS.md", "TOOLS.md"] {
                    let relative_path = Path::new(file_name);
                    if !should_fallback_agent_file_to_root(relative_path) {
                        continue;
                    }
                    let agent_path = root.join(file_name);
                    let root_path = chelix_config::data_dir().join(file_name);
                    if !agent_path.exists() && root_path.exists() {
                        let mut entry = serde_json::json!({
                            "path": file_name,
                            "source": "root",
                            "size": std::fs::metadata(&root_path).ok().map(|metadata| metadata.len()),
                        });
                        if let Some(object) = entry.as_object_mut()
                            && let Some(status) = workspace_prompt_file_status(&agent_id, file_name, limit_chars)
                            && let Ok(status_value) = serde_json::to_value(status)
                            && let Some(status_object) = status_value.as_object()
                        {
                            for (key, value) in status_object {
                                if key != "path" && key != "source" && key != "size" {
                                    object.insert(key.clone(), value.clone());
                                }
                            }
                        }
                        files.push(entry);
                    }
                }
                files.sort_by(|left, right| {
                    let left_path = left.get("path").and_then(serde_json::Value::as_str).unwrap_or("");
                    let right_path = right.get("path").and_then(serde_json::Value::as_str).unwrap_or("");
                    left_path.cmp(right_path)
                });
                Ok(serde_json::json!({ "agent_id": agent_id, "files": files }))
            })
        }),
    );

    reg.register(
        "agents.files.get",
        Box::new(|ctx| {
            Box::pin(async move {
                let agent_id = resolve_requested_agent_id(&ctx, &ctx.params).await?;
                let relative_path = normalize_relative_agent_path(
                    ctx.params
                        .get("path")
                        .and_then(serde_json::Value::as_str)
                        .ok_or_else(|| {
                            ErrorShape::new(
                                error_codes::INVALID_REQUEST,
                                "missing 'path' parameter",
                            )
                        })?,
                )?;
                let content = read_agent_file(&agent_id, &relative_path)?;
                Ok(serde_json::json!({
                    "agent_id": agent_id,
                    "path": relative_path.to_string_lossy(),
                    "content": content,
                }))
            })
        }),
    );

    reg.register(
        "agents.files.set",
        Box::new(|ctx| {
            Box::pin(async move {
                let agent_id = resolve_requested_agent_id(&ctx, &ctx.params).await?;
                let relative_path = normalize_relative_agent_path(
                    ctx.params
                        .get("path")
                        .and_then(serde_json::Value::as_str)
                        .ok_or_else(|| {
                            ErrorShape::new(
                                error_codes::INVALID_REQUEST,
                                "missing 'path' parameter",
                            )
                        })?,
                )?;
                let content = ctx
                    .params
                    .get("content")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("");
                let full_path = chelix_config::agent_workspace_dir(&agent_id).join(&relative_path);
                if let Some(parent) = full_path.parent() {
                    std::fs::create_dir_all(parent).map_err(|error| {
                        ErrorShape::new(error_codes::UNAVAILABLE, error.to_string())
                    })?;
                }
                std::fs::write(&full_path, content).map_err(|error| {
                    ErrorShape::new(error_codes::UNAVAILABLE, error.to_string())
                })?;
                Ok(serde_json::json!({
                    "ok": true,
                    "agent_id": agent_id,
                    "path": relative_path.to_string_lossy(),
                }))
            })
        }),
    );
}

#[cfg(feature = "agent")]
fn parse_save_agent_params(params: serde_json::Value) -> Result<SaveAgentParams, ErrorShape> {
    serde_json::from_value(params)
        .map_err(|error| ErrorShape::new(error_codes::INVALID_REQUEST, error.to_string()))
}

#[cfg(feature = "agent")]
fn required_agent_id(params: &serde_json::Value) -> Result<String, ErrorShape> {
    parse_agent_id_param(params).ok_or_else(|| {
        ErrorShape::new(
            error_codes::INVALID_REQUEST,
            "missing 'id' or 'agent_id' parameter",
        )
    })
}

#[cfg(feature = "agent")]
fn validate_agent_id(id: &str) -> Result<(), ErrorShape> {
    chelix_config::validate_agent_id(id)
        .map_err(|message| ErrorShape::new(error_codes::INVALID_REQUEST, message))
}

#[cfg(feature = "agent")]
fn validate_agent_config(agent: &chelix_config::AgentConfig) -> Result<(), ErrorShape> {
    if agent.name.trim().is_empty() {
        return Err(ErrorShape::new(
            error_codes::INVALID_REQUEST,
            "agent name must not be empty",
        ));
    }
    if agent.max_tools_threshold == 0 {
        return Err(ErrorShape::new(
            error_codes::INVALID_REQUEST,
            "max_tools_threshold must be at least 1",
        ));
    }
    if let Some(chelix_config::schema::ToolChoice::Tool { name }) = &agent.tool_controls.tool_choice
    {
        if name.trim().is_empty() {
            return Err(ErrorShape::new(
                error_codes::INVALID_REQUEST,
                "forced tool_choice requires a non-empty name",
            ));
        }
        if let Some(active_tools) = &agent.tool_controls.active_tools
            && !active_tools.iter().any(|active| active == name)
        {
            return Err(ErrorShape::new(
                error_codes::INVALID_REQUEST,
                "forced tool_choice must be included in active_tools",
            ));
        }
    }
    Ok(())
}

#[cfg(feature = "agent")]
fn save_agent_prompts(
    id: &str,
    soul: Option<&str>,
    subagent_prompt: Option<&str>,
) -> Result<(), ErrorShape> {
    chelix_config::save_soul_for_agent(id, soul)
        .map_err(|error| ErrorShape::new(error_codes::UNAVAILABLE, error.to_string()))?;
    chelix_config::save_subagent_prompt_for_agent(id, subagent_prompt)
        .map_err(|error| ErrorShape::new(error_codes::UNAVAILABLE, error.to_string()))?;
    Ok(())
}

#[cfg(feature = "agent")]
fn agent_payload(
    id: &str,
    agent: &chelix_config::AgentConfig,
    is_default: bool,
) -> Result<serde_json::Value, ErrorShape> {
    let mut value = serde_json::to_value(agent)
        .map_err(|error| ErrorShape::new(error_codes::INTERNAL, error.to_string()))?;
    let object = value.as_object_mut().ok_or_else(|| {
        ErrorShape::new(
            error_codes::INTERNAL,
            "agent config did not serialize to an object",
        )
    })?;
    object.insert("id".to_string(), serde_json::json!(id));
    object.insert("is_default".to_string(), serde_json::json!(is_default));
    object.insert(
        "soul".to_string(),
        serde_json::json!(chelix_config::load_soul_for_agent(id)),
    );
    object.insert(
        "subagent_prompt".to_string(),
        serde_json::json!(chelix_config::load_subagent_prompt_for_agent(id)),
    );
    Ok(value)
}

#[cfg(feature = "agent")]
fn agents_config_for_ctx(
    ctx: &MethodContext,
) -> Result<Arc<tokio::sync::RwLock<chelix_config::AgentsConfig>>, ErrorShape> {
    ctx.state.services.agents_config.clone().ok_or_else(|| {
        ErrorShape::new(
            error_codes::UNAVAILABLE,
            "agent configuration is not available",
        )
    })
}

#[cfg(feature = "agent")]
async fn persist_agents_config(
    ctx: &MethodContext,
    update: impl FnOnce(&mut chelix_config::ChelixConfig) -> chelix_config::Result<()>,
) -> Result<(), ErrorShape> {
    chelix_config::update_config_checked(update)
        .map_err(|error| ErrorShape::new(error_codes::INVALID_REQUEST, error.to_string()))?;
    let fresh = chelix_config::discover_and_load()
        .map_err(|error| ErrorShape::new(error_codes::INTERNAL, error.to_string()))?;
    let agents = agents_config_for_ctx(ctx)?;
    *agents.write().await = fresh.agents;
    Ok(())
}

#[cfg(all(test, feature = "agent"))]
mod tests {
    use super::*;

    #[test]
    fn validates_agent_ids() {
        assert!(validate_agent_id("qa-2").is_ok());
        assert!(validate_agent_id("default").is_err());
        assert!(validate_agent_id("QA").is_err());
        assert!(validate_agent_id("-qa").is_err());
    }

    #[test]
    fn rejects_empty_agent_name() {
        let agent = chelix_config::AgentConfig::default();
        assert!(validate_agent_config(&agent).is_err());
    }
}
