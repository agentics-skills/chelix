use super::*;

use {
    chelix_common::ActiveToolInvocation,
    chelix_service_traits::{
        ChatCompactRequest, ChatContextRequest, ChatExecutionContext, ChatFullContextRequest,
        ChatRawPromptRequest,
    },
};

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct QueuedPromptsStatusParams {
    session_key: chelix_sessions::SessionKey,
}

#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct QueuedPromptsRemoveParams {
    id: i64,
}

fn queued_prompts_response(
    status: chelix_sessions::QueuedPromptsStatus,
) -> Result<serde_json::Value, ErrorShape> {
    serde_json::to_value(status).map_err(|error| {
        ErrorShape::from(ServiceError::message(format!(
            "failed to serialize queued prompts status: {error}"
        )))
    })
}

async fn chat_execution_context(ctx: &MethodContext) -> Result<ChatExecutionContext, ErrorShape> {
    let session_id = ctx.resolved_session_id().await.ok_or_else(|| {
        ErrorShape::new(
            error_codes::INVALID_REQUEST,
            "no session context for request",
        )
    })?;
    let (accept_language, remote_ip, timezone) = {
        let registry = ctx.state.client_registry.read().await;
        let client = registry.clients.get(&ctx.client_conn_id);
        (
            client.and_then(|client| client.accept_language.clone()),
            client.and_then(|client| client.remote_ip.clone()),
            client.and_then(|client| client.timezone.clone()),
        )
    };
    let mut context = ChatExecutionContext::client(session_id, ctx.client_conn_id.clone());
    context.accept_language = accept_language;
    context.remote_ip = remote_ip;
    context.timezone = timezone;
    Ok(context)
}

fn insert_session_activity_snapshot(
    obj: &mut serde_json::Map<String, serde_json::Value>,
    replying: bool,
    tool_invocations: Vec<ActiveToolInvocation>,
    voice_pending: bool,
) {
    obj.insert("replying".to_string(), serde_json::Value::Bool(replying));
    if !replying {
        return;
    }
    if !tool_invocations.is_empty() {
        match serde_json::to_value(tool_invocations) {
            Ok(value) => {
                obj.insert("activeToolInvocations".to_string(), value);
            },
            Err(error) => {
                tracing::error!(%error, "failed to serialize active tool invocations");
            },
        }
    }
    if voice_pending {
        obj.insert("voicePending".to_string(), serde_json::Value::Bool(true));
    }
}

pub(super) fn register(reg: &mut MethodRegistry) {
    // Config
    reg.register(
        "config.get",
        Box::new(|ctx| {
            Box::pin(async move {
                ctx.state
                    .services
                    .config
                    .get(ctx.params.clone())
                    .await
                    .map_err(ErrorShape::from)
            })
        }),
    );
    reg.register(
        "config.set",
        Box::new(|ctx| {
            Box::pin(async move {
                ctx.state
                    .services
                    .config
                    .set(ctx.params.clone())
                    .await
                    .map_err(ErrorShape::from)
            })
        }),
    );
    reg.register(
        "config.apply",
        Box::new(|ctx| {
            Box::pin(async move {
                ctx.state
                    .services
                    .config
                    .apply(ctx.params.clone())
                    .await
                    .map_err(ErrorShape::from)
            })
        }),
    );
    reg.register(
        "config.patch",
        Box::new(|ctx| {
            Box::pin(async move {
                ctx.state
                    .services
                    .config
                    .patch(ctx.params.clone())
                    .await
                    .map_err(ErrorShape::from)
            })
        }),
    );
    reg.register(
        "config.schema",
        Box::new(|ctx| {
            Box::pin(async move {
                ctx.state
                    .services
                    .config
                    .schema()
                    .await
                    .map_err(ErrorShape::from)
            })
        }),
    );

    // Cron
    reg.register(
        "cron.list",
        Box::new(|ctx| {
            Box::pin(async move {
                ctx.state
                    .services
                    .cron
                    .list()
                    .await
                    .map_err(ErrorShape::from)
            })
        }),
    );
    reg.register(
        "cron.status",
        Box::new(|ctx| {
            Box::pin(async move {
                ctx.state
                    .services
                    .cron
                    .status()
                    .await
                    .map_err(ErrorShape::from)
            })
        }),
    );
    reg.register(
        "cron.add",
        Box::new(|ctx| {
            Box::pin(async move {
                ctx.state
                    .services
                    .cron
                    .add(ctx.params.clone())
                    .await
                    .map_err(ErrorShape::from)
            })
        }),
    );
    reg.register(
        "cron.update",
        Box::new(|ctx| {
            Box::pin(async move {
                ctx.state
                    .services
                    .cron
                    .update(ctx.params.clone())
                    .await
                    .map_err(ErrorShape::from)
            })
        }),
    );
    reg.register(
        "cron.remove",
        Box::new(|ctx| {
            Box::pin(async move {
                ctx.state
                    .services
                    .cron
                    .remove(ctx.params.clone())
                    .await
                    .map_err(ErrorShape::from)
            })
        }),
    );
    reg.register(
        "cron.run",
        Box::new(|ctx| {
            Box::pin(async move {
                ctx.state
                    .services
                    .cron
                    .run(ctx.params.clone())
                    .await
                    .map_err(ErrorShape::from)
            })
        }),
    );
    reg.register(
        "cron.runs",
        Box::new(|ctx| {
            Box::pin(async move {
                ctx.state
                    .services
                    .cron
                    .runs(ctx.params.clone())
                    .await
                    .map_err(ErrorShape::from)
            })
        }),
    );

    // Webhooks
    reg.register(
        "webhooks.list",
        Box::new(|ctx| {
            Box::pin(async move {
                ctx.state
                    .services
                    .webhooks
                    .list()
                    .await
                    .map_err(ErrorShape::from)
            })
        }),
    );
    reg.register(
        "webhooks.get",
        Box::new(|ctx| {
            Box::pin(async move {
                ctx.state
                    .services
                    .webhooks
                    .get(ctx.params.clone())
                    .await
                    .map_err(ErrorShape::from)
            })
        }),
    );
    reg.register(
        "webhooks.create",
        Box::new(|ctx| {
            Box::pin(async move {
                ctx.state
                    .services
                    .webhooks
                    .create(ctx.params.clone())
                    .await
                    .map_err(ErrorShape::from)
            })
        }),
    );
    reg.register(
        "webhooks.update",
        Box::new(|ctx| {
            Box::pin(async move {
                ctx.state
                    .services
                    .webhooks
                    .update(ctx.params.clone())
                    .await
                    .map_err(ErrorShape::from)
            })
        }),
    );
    reg.register(
        "webhooks.delete",
        Box::new(|ctx| {
            Box::pin(async move {
                ctx.state
                    .services
                    .webhooks
                    .delete(ctx.params.clone())
                    .await
                    .map_err(ErrorShape::from)
            })
        }),
    );
    reg.register(
        "webhooks.deliveries",
        Box::new(|ctx| {
            Box::pin(async move {
                ctx.state
                    .services
                    .webhooks
                    .deliveries(ctx.params.clone())
                    .await
                    .map_err(ErrorShape::from)
            })
        }),
    );
    reg.register(
        "webhooks.delivery.get",
        Box::new(|ctx| {
            Box::pin(async move {
                ctx.state
                    .services
                    .webhooks
                    .delivery_get(ctx.params.clone())
                    .await
                    .map_err(ErrorShape::from)
            })
        }),
    );
    reg.register(
        "webhooks.delivery.payload",
        Box::new(|ctx| {
            Box::pin(async move {
                ctx.state
                    .services
                    .webhooks
                    .delivery_payload(ctx.params.clone())
                    .await
                    .map_err(ErrorShape::from)
            })
        }),
    );
    reg.register(
        "webhooks.delivery.actions",
        Box::new(|ctx| {
            Box::pin(async move {
                ctx.state
                    .services
                    .webhooks
                    .delivery_actions(ctx.params.clone())
                    .await
                    .map_err(ErrorShape::from)
            })
        }),
    );
    reg.register(
        "webhooks.profiles",
        Box::new(|ctx| {
            Box::pin(async move {
                ctx.state
                    .services
                    .webhooks
                    .profiles()
                    .await
                    .map_err(ErrorShape::from)
            })
        }),
    );

    // Heartbeat
    reg.register(
        "heartbeat.status",
        Box::new(|ctx| {
            Box::pin(async move {
                let config = ctx.state.inner.read().await.heartbeat_config.clone();
                let heartbeat_path = chelix_config::heartbeat_path();
                let heartbeat_file_exists = heartbeat_path.exists();
                let heartbeat_md = chelix_config::load_heartbeat_md();
                let (_, prompt_source) = chelix_cron::heartbeat::resolve_heartbeat_prompt(
                    config.prompt.as_deref(),
                    heartbeat_md.as_deref(),
                );
                // No meaningful prompt → heartbeat won't execute.
                let has_prompt =
                    prompt_source != chelix_cron::heartbeat::HeartbeatPromptSource::Default;
                // Find the heartbeat job to get its state.
                let jobs_val = ctx
                    .state
                    .services
                    .cron
                    .list()
                    .await
                    .map_err(ErrorShape::from)?;
                let jobs: Vec<chelix_cron::types::CronJob> =
                    serde_json::from_value(jobs_val).unwrap_or_default();
                let hb_job = jobs.iter().find(|j| j.name == "__heartbeat__");
                Ok(serde_json::json!({
                    "config": config,
                    "job": hb_job,
                    "promptSource": prompt_source.as_str(),
                    "heartbeatFileExists": heartbeat_file_exists,
                    "hasPrompt": has_prompt,
                }))
            })
        }),
    );
    reg.register(
            "heartbeat.update",
            Box::new(|ctx| {
                Box::pin(async move {
                    let patch: chelix_config::schema::HeartbeatConfig =
                        serde_json::from_value(ctx.params.clone()).map_err(|e| {
                            ErrorShape::new(
                                error_codes::INVALID_REQUEST,
                                format!("invalid heartbeat config: {e}"),
                            )
                        })?;
                    ctx.state.inner.write().await.heartbeat_config = patch.clone();

                    // Persist to chelix.toml so the config survives restarts.
                    if let Err(e) = chelix_config::update_config(|cfg| {
                        cfg.heartbeat = patch.clone();
                    }) {
                        tracing::warn!(error = %e, "failed to persist heartbeat config");
                    }

                    // Update the heartbeat cron job in-place.
                    let jobs_val = ctx
                        .state
                        .services
                        .cron
                        .list()
                        .await
                        .map_err(ErrorShape::from)?;
                    let jobs: Vec<chelix_cron::types::CronJob> =
                        serde_json::from_value(jobs_val).unwrap_or_default();
                    let interval_ms = chelix_cron::heartbeat::parse_interval_ms(&patch.every)
                        .unwrap_or(chelix_cron::heartbeat::DEFAULT_INTERVAL_MS);
                    let heartbeat_md = chelix_config::load_heartbeat_md();
                    let (prompt, prompt_source) =
                        chelix_cron::heartbeat::resolve_heartbeat_prompt(
                            patch.prompt.as_deref(),
                            heartbeat_md.as_deref(),
                        );
                    if prompt_source
                        == chelix_cron::heartbeat::HeartbeatPromptSource::HeartbeatMd
                    {
                        tracing::info!("loaded heartbeat prompt from HEARTBEAT.md");
                    }
                    if patch.prompt.as_deref().is_some_and(|p| !p.trim().is_empty())
                        && heartbeat_md.as_deref().is_some_and(|p| !p.trim().is_empty())
                        && prompt_source
                            == chelix_cron::heartbeat::HeartbeatPromptSource::Config
                    {
                        tracing::warn!(
                            "heartbeat prompt source conflict: config heartbeat.prompt overrides HEARTBEAT.md"
                        );
                    }
                    // Disable the job when there is no meaningful prompt,
                    // even if the user toggled enabled=true.
                    let has_prompt = prompt_source
                        != chelix_cron::heartbeat::HeartbeatPromptSource::Default;
                    let effective_enabled = patch.enabled && has_prompt;

                    if let Some(hb_job) = jobs.iter().find(|j| j.id == "__heartbeat__") {
                        let job_patch = chelix_cron::types::CronJobPatch {
                            schedule: Some(chelix_cron::types::CronSchedule::Every {
                                every_ms: interval_ms,
                                anchor_ms: None,
                            }),
                            payload: Some(chelix_cron::types::CronPayload::AgentTurn(
                                chelix_cron::types::CronAgentTurn {
                                    message: prompt,
                                    model_override: patch.model_override.as_ref().map(Into::into),
                                    agent_id: patch.agent_id.clone(),
                                    timeout_secs: None,
                                    tool_choice: None,
                                    deliver: patch.deliver,
                                    channel: patch.channel.clone(),
                                    to: patch.to.clone(),
                                },
                            )),
                            enabled: Some(effective_enabled),
                            ..Default::default()
                        };
                        ctx.state
                            .services
                            .cron
                            .update(serde_json::json!({
                                "id": hb_job.id,
                                "patch": job_patch,
                            }))
                            .await
                            .map_err(ErrorShape::from)?;
                    } else if effective_enabled {
                        // Create the heartbeat job only when enabled with a valid prompt.
                        let create = chelix_cron::types::CronJobCreate {
                            id: Some("__heartbeat__".into()),
                            name: "__heartbeat__".into(),
                            schedule: chelix_cron::types::CronSchedule::Every {
                                every_ms: interval_ms,
                                anchor_ms: None,
                            },
                            payload: chelix_cron::types::CronPayload::AgentTurn(
                                chelix_cron::types::CronAgentTurn {
                                    message: prompt,
                                    model_override: patch.model_override.as_ref().map(Into::into),
                                    agent_id: patch.agent_id.clone(),
                                    timeout_secs: None,
                                    tool_choice: None,
                                    deliver: patch.deliver,
                                    channel: patch.channel.clone(),
                                    to: patch.to.clone(),
                                },
                            ),
                            session_target: chelix_cron::types::SessionTarget::Named("heartbeat".into()),
                            delete_after_run: false,
                            enabled: effective_enabled,
                            system: true,
                            auto_prune_container: None,
                            wake_mode: chelix_cron::types::CronWakeMode::default(),
                        };
                        let create_json = serde_json::to_value(create)
                            .map_err(|e| ErrorShape::new(error_codes::INVALID_REQUEST, format!("failed to serialize job: {e}")))?;
                        ctx.state
                            .services
                            .cron
                            .add(create_json)
                            .await
                            .map_err(ErrorShape::from)?;
                    }
                    Ok(serde_json::json!({ "updated": true }))
                })
            }),
        );
    reg.register(
        "heartbeat.run",
        Box::new(|ctx| {
            Box::pin(async move {
                let jobs_val = ctx
                    .state
                    .services
                    .cron
                    .list()
                    .await
                    .map_err(ErrorShape::from)?;
                let jobs: Vec<chelix_cron::types::CronJob> =
                    serde_json::from_value(jobs_val).unwrap_or_default();
                let hb_job = jobs
                    .iter()
                    .find(|j| j.name == "__heartbeat__")
                    .ok_or_else(|| {
                        ErrorShape::new(error_codes::INVALID_REQUEST, "heartbeat job not found")
                    })?;
                ctx.state
                    .services
                    .cron
                    .run(serde_json::json!({
                        "id": hb_job.id,
                        "force": true,
                    }))
                    .await
                    .map_err(ErrorShape::from)?;
                Ok(serde_json::json!({ "triggered": true }))
            })
        }),
    );
    reg.register(
        "heartbeat.runs",
        Box::new(|ctx| {
            Box::pin(async move {
                let jobs_val = ctx
                    .state
                    .services
                    .cron
                    .list()
                    .await
                    .map_err(ErrorShape::from)?;
                let jobs: Vec<chelix_cron::types::CronJob> =
                    serde_json::from_value(jobs_val).unwrap_or_default();
                let hb_job = jobs
                    .iter()
                    .find(|j| j.name == "__heartbeat__")
                    .ok_or_else(|| {
                        ErrorShape::new(error_codes::INVALID_REQUEST, "heartbeat job not found")
                    })?;
                let limit = ctx
                    .params
                    .get("limit")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(20);
                ctx.state
                    .services
                    .cron
                    .runs(serde_json::json!({
                        "id": hb_job.id,
                        "limit": limit,
                    }))
                    .await
                    .map_err(ErrorShape::from)
            })
        }),
    );

    // Chat (uses chat_override if set, otherwise falls back to services.chat).
    reg.register(
        "chat.send",
        Box::new(|ctx| {
            Box::pin(async move {
                let request = serde_json::from_value::<chelix_service_traits::ChatSendRequest>(
                    ctx.params.clone(),
                )
                .map_err(|error| {
                    ErrorShape::new(
                        error_codes::INVALID_REQUEST,
                        format!("invalid chat.send request: {error}"),
                    )
                })?;
                let execution_context = chat_execution_context(&ctx).await?;
                ctx.state
                    .chat()
                    .send(request, execution_context)
                    .await
                    .map_err(ErrorShape::from)
            })
        }),
    );
    reg.register(
        "chat.send_sync",
        Box::new(|ctx| {
            Box::pin(async move {
                let request = serde_json::from_value::<chelix_service_traits::ChatSendSyncRequest>(
                    ctx.params.clone(),
                )
                .map_err(|error| {
                    ErrorShape::new(
                        error_codes::INVALID_REQUEST,
                        format!("invalid chat.send_sync request: {error}"),
                    )
                })?;
                let execution_context = chat_execution_context(&ctx).await?;
                ctx.state
                    .chat()
                    .send_sync(request, execution_context)
                    .await
                    .map_err(ErrorShape::from)
            })
        }),
    );
    reg.register(
        "chat.abort",
        Box::new(|ctx| {
            Box::pin(async move {
                ctx.state
                    .chat()
                    .abort(ctx.params.clone())
                    .await
                    .map_err(ErrorShape::from)
            })
        }),
    );

    reg.register(
        "external_agents.list",
        Box::new(|ctx| {
            Box::pin(async move {
                ctx.state
                    .services
                    .external_agent
                    .list()
                    .await
                    .map_err(ErrorShape::from)
            })
        }),
    );
    reg.register(
        "external_agents.bind",
        Box::new(|ctx| {
            Box::pin(async move {
                ctx.state
                    .services
                    .external_agent
                    .bind(ctx.params.clone())
                    .await
                    .map_err(ErrorShape::from)
            })
        }),
    );
    reg.register(
        "external_agents.unbind",
        Box::new(|ctx| {
            Box::pin(async move {
                ctx.state
                    .services
                    .external_agent
                    .unbind(ctx.params.clone())
                    .await
                    .map_err(ErrorShape::from)
            })
        }),
    );
    reg.register(
        "external_agents.status",
        Box::new(|ctx| {
            Box::pin(async move {
                ctx.state
                    .services
                    .external_agent
                    .status(ctx.params.clone())
                    .await
                    .map_err(ErrorShape::from)
            })
        }),
    );
    reg.register(
        "chat.peek",
        Box::new(|ctx| {
            Box::pin(async move {
                ctx.state
                    .chat()
                    .peek(ctx.params.clone())
                    .await
                    .map_err(ErrorShape::from)
            })
        }),
    );
    reg.register(
        "chat.queued_prompts.status",
        Box::new(|ctx| {
            Box::pin(async move {
                let params: QueuedPromptsStatusParams = serde_json::from_value(ctx.params)
                    .map_err(|error| {
                        ErrorShape::new(error_codes::INVALID_REQUEST, error.to_string())
                    })?;
                let status = ctx
                    .state
                    .chat()
                    .queued_prompts_status(params.session_key)
                    .await
                    .map_err(ErrorShape::from)?;
                queued_prompts_response(status)
            })
        }),
    );
    reg.register(
        "chat.queued_prompts.remove",
        Box::new(|ctx| {
            Box::pin(async move {
                let params: QueuedPromptsRemoveParams = serde_json::from_value(ctx.params)
                    .map_err(|error| {
                        ErrorShape::new(error_codes::INVALID_REQUEST, error.to_string())
                    })?;
                let status = ctx
                    .state
                    .chat()
                    .queued_prompts_remove(params.id)
                    .await
                    .map_err(ErrorShape::from)?;
                queued_prompts_response(status)
            })
        }),
    );
    reg.register(
        "chat.history",
        Box::new(|ctx| {
            Box::pin(async move {
                let mut params = ctx.params.clone();
                params["_conn_id"] = serde_json::json!(ctx.client_conn_id);
                ctx.state
                    .chat()
                    .history(params)
                    .await
                    .map_err(ErrorShape::from)
            })
        }),
    );
    reg.register(
        "chat.inject",
        Box::new(|ctx| {
            Box::pin(async move {
                ctx.state
                    .chat()
                    .inject(ctx.params.clone())
                    .await
                    .map_err(ErrorShape::from)
            })
        }),
    );
    reg.register(
        "chat.clear",
        Box::new(|ctx| {
            Box::pin(async move {
                // Export the session before the clear destroys its history.
                if let Some(session_key) = active_session_key_for_ctx(&ctx).await {
                    let hooks = ctx.state.inner.read().await.hook_registry.clone();
                    if let Some(ref hooks) = hooks {
                        crate::session::dispatch_command_hook(hooks, &session_key, "reset", None)
                            .await;
                    }
                }

                let mut params = ctx.params.clone();
                params["_conn_id"] = serde_json::json!(ctx.client_conn_id);
                ctx.state
                    .chat()
                    .clear(params)
                    .await
                    .map_err(ErrorShape::from)
            })
        }),
    );
    reg.register(
        "chat.compact",
        Box::new(|ctx| {
            Box::pin(async move {
                let request = serde_json::from_value::<ChatCompactRequest>(ctx.params.clone())
                    .map_err(|error| {
                        ErrorShape::new(
                            error_codes::INVALID_REQUEST,
                            format!("invalid chat.compact request: {error}"),
                        )
                    })?;
                let execution_context = chat_execution_context(&ctx).await?;
                let session_key = Some(execution_context.session_id.as_str().to_string());
                let progress = crate::operation_progress::OperationProgressEmitter::new(
                    ctx.state.clone(),
                    ctx.client_conn_id.clone(),
                    ctx.request_id.clone(),
                    "chat.compact",
                    "session_compact",
                    session_key,
                );
                progress
                    .emit(
                        "started",
                        "Preparing to compact the context window…",
                        Some(0),
                        Some(2),
                        false,
                    )
                    .await;
                let result = progress
                    .run_with_heartbeat(
                        "compacting",
                        "Compacting the context window…",
                        Some(1),
                        Some(2),
                        ctx.state.chat().compact(request, execution_context),
                    )
                    .await
                    .map_err(ErrorShape::from);
                match &result {
                    Ok(_) => {
                        progress
                            .emit(
                                "completed",
                                "Context window compaction complete",
                                Some(2),
                                Some(2),
                                true,
                            )
                            .await;
                    },
                    Err(_) => {
                        progress
                            .emit(
                                "failed",
                                "Context window compaction failed",
                                None,
                                None,
                                true,
                            )
                            .await;
                    },
                }
                result
            })
        }),
    );

    reg.register(
        "chat.context",
        Box::new(|ctx| {
            Box::pin(async move {
                let request = serde_json::from_value::<ChatContextRequest>(ctx.params.clone())
                    .map_err(|error| {
                        ErrorShape::new(
                            error_codes::INVALID_REQUEST,
                            format!("invalid chat.context request: {error}"),
                        )
                    })?;
                let execution_context = chat_execution_context(&ctx).await?;
                ctx.state
                    .chat()
                    .context(request, execution_context)
                    .await
                    .map_err(ErrorShape::from)
            })
        }),
    );

    reg.register(
        "chat.raw_prompt",
        Box::new(|ctx| {
            Box::pin(async move {
                let request = serde_json::from_value::<ChatRawPromptRequest>(ctx.params.clone())
                    .map_err(|error| {
                        ErrorShape::new(
                            error_codes::INVALID_REQUEST,
                            format!("invalid chat.raw_prompt request: {error}"),
                        )
                    })?;
                let execution_context = chat_execution_context(&ctx).await?;
                ctx.state
                    .chat()
                    .raw_prompt(request, execution_context)
                    .await
                    .map_err(ErrorShape::from)
            })
        }),
    );

    reg.register(
        "chat.full_context",
        Box::new(|ctx| {
            Box::pin(async move {
                let request = serde_json::from_value::<ChatFullContextRequest>(ctx.params.clone())
                    .map_err(|error| {
                        ErrorShape::new(
                            error_codes::INVALID_REQUEST,
                            format!("invalid chat.full_context request: {error}"),
                        )
                    })?;
                let execution_context = chat_execution_context(&ctx).await?;
                ctx.state
                    .chat()
                    .full_context(request, execution_context)
                    .await
                    .map_err(ErrorShape::from)
            })
        }),
    );

    reg.register(
        "chat.prompt_memory.refresh",
        Box::new(|ctx| {
            Box::pin(async move {
                let mut params = ctx.params.clone();
                params["_conn_id"] = serde_json::json!(ctx.client_conn_id);
                ctx.state
                    .chat()
                    .refresh_prompt_memory(params)
                    .await
                    .map_err(ErrorShape::from)
            })
        }),
    );

    // Session switching
    reg.register(
        "sessions.switch",
        Box::new(|ctx| {
            Box::pin(async move {
                if ctx.transport.is_stateless_http() {
                    return Err(ErrorShape::new(
                        error_codes::INVALID_REQUEST,
                        "sessions.switch is unavailable over stateless HTTP",
                    ));
                }
                let key = ctx
                    .params
                    .get("key")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| {
                        ErrorShape::new(error_codes::INVALID_REQUEST, "missing 'key' parameter")
                    })?;
                let include_history = ctx
                    .params
                    .get("include_history")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(true);
                let previous_active_key = {
                    let registry = ctx.state.client_registry.read().await;
                    registry.active_sessions.get(&ctx.client_conn_id).cloned()
                };
                let was_existing_session =
                    if let Some(ref metadata) = ctx.state.services.session_metadata {
                        metadata
                            .get(key)
                            .await
                            .map_err(|error| {
                                ErrorShape::new(error_codes::UNAVAILABLE, error.to_string())
                            })?
                            .is_some()
                    } else {
                        false
                    };

                // Store the active session (and project if provided) for this connection.
                {
                    let mut registry = ctx.state.client_registry.write().await;
                    registry
                        .active_sessions
                        .insert(ctx.client_conn_id.clone(), key.to_string());

                    if let Some(project_id) = ctx.params.get("project_id").and_then(|v| v.as_str())
                    {
                        if project_id.is_empty() {
                            registry.active_projects.remove(&ctx.client_conn_id);
                        } else {
                            registry
                                .active_projects
                                .insert(ctx.client_conn_id.clone(), project_id.to_string());
                        }
                    }
                }

                // Resolve first (auto-creates session if needed), then
                // persist project_id so the entry exists when we patch.
                let mut resolve_params = serde_json::json!({
                    "key": key,
                    "include_history": include_history,
                });
                if !was_existing_session
                    && let Some(previous_key) = previous_active_key
                        .as_deref()
                        .filter(|previous_key| *previous_key != key)
                {
                    resolve_params["inherit_agent_from"] = serde_json::json!(previous_key);
                }
                let mut result = ctx
                    .state
                    .services
                    .session
                    .resolve(resolve_params)
                    .await
                    .map_err(|e| {
                        tracing::error!("session resolve failed: {e}");
                        ErrorShape::new(
                            error_codes::UNAVAILABLE,
                            format!("session resolve failed: {e}"),
                        )
                    })?;

                // Mark the session as seen so unread state clears.
                ctx.state.services.session.mark_seen(key).await;

                // Export the previous session when the user creates a brand-new
                // session (e.g. "+" button or /new).  Switching between two
                // *existing* sessions intentionally skips export — only new-
                // session creation signals the end of the previous conversation.
                if !was_existing_session
                    && let Some(prev_key) = previous_active_key.as_deref().filter(|pk| *pk != key)
                {
                    let hooks = ctx.state.inner.read().await.hook_registry.clone();
                    if let Some(ref hooks) = hooks {
                        crate::session::dispatch_command_hook(hooks, prev_key, "new", None).await;
                    }
                }

                if let Some(pid) = ctx.params.get("project_id").and_then(|v| v.as_str()) {
                    ctx.state
                        .services
                        .session
                        .patch(serde_json::json!({ "key": key, "projectId": pid }))
                        .await
                        .map_err(ErrorShape::from)?;

                    // Auto-create worktree if project has auto_worktree enabled.
                    if let Ok(proj_val) = ctx
                        .state
                        .services
                        .project
                        .get(serde_json::json!({"id": pid}))
                        .await
                        && proj_val
                            .get("auto_worktree")
                            .and_then(|v| v.as_bool())
                            .unwrap_or(false)
                        && let Some(dir) = proj_val.get("directory").and_then(|v| v.as_str())
                    {
                        let project_dir = Path::new(dir);
                        let create_result =
                            match chelix_projects::WorktreeManager::resolve_base_branch(project_dir)
                                .await
                            {
                                Ok(base) => {
                                    chelix_projects::WorktreeManager::create_from_base(
                                        project_dir,
                                        key,
                                        &base,
                                    )
                                    .await
                                },
                                Err(_) => {
                                    chelix_projects::WorktreeManager::create(project_dir, key).await
                                },
                            };
                        match create_result {
                            Ok(wt_dir) => {
                                let prefix = proj_val
                                    .get("branch_prefix")
                                    .and_then(|v| v.as_str())
                                    .filter(|s| !s.is_empty())
                                    .unwrap_or("chelix");
                                let branch = format!("{prefix}/{key}");
                                ctx.state
                                    .services
                                    .session
                                    .patch(serde_json::json!({
                                        "key": key,
                                        "worktreeBranch": branch,
                                    }))
                                    .await
                                    .map_err(ErrorShape::from)?;

                                if let Err(e) = chelix_projects::worktree::copy_project_config(
                                    project_dir,
                                    &wt_dir,
                                ) {
                                    tracing::warn!("failed to copy project config: {e}");
                                }

                                if let Some(cmd) = proj_val
                                    .get("setup_command")
                                    .and_then(|v| v.as_str())
                                    .filter(|s| !s.is_empty())
                                    && let Err(e) = chelix_projects::WorktreeManager::run_setup(
                                        &wt_dir,
                                        cmd,
                                        project_dir,
                                        key,
                                    )
                                    .await
                                {
                                    tracing::warn!("worktree setup failed: {e}");
                                }
                            },
                            Err(e) => {
                                tracing::warn!("auto-create worktree failed: {e}");
                            },
                        }
                    }
                }

                // If the client already has a cached history with the same
                // message count, skip sending the full history to avoid
                // transferring megabytes of data on every session switch.
                let cached_count = ctx
                    .params
                    .get("cached_message_count")
                    .and_then(|v| v.as_u64());
                if !include_history && let Some(obj) = result.as_object_mut() {
                    obj.insert("history".to_string(), serde_json::Value::Array(Vec::new()));
                    obj.insert("historyOmitted".to_string(), serde_json::Value::Bool(true));
                    obj.remove("historyTruncated");
                    obj.remove("historyDroppedCount");
                }
                if let Some(cached) = cached_count
                    && include_history
                    && let Some(obj) = result.as_object_mut()
                    && let Some(entry_obj) = obj.get("entry").and_then(|e| e.as_object())
                    && let Some(server_count) =
                        entry_obj.get("messageCount").and_then(|v| v.as_u64())
                    && cached == server_count
                {
                    obj.insert("history".to_string(), serde_json::Value::Array(Vec::new()));
                    obj.insert("historyCacheHit".to_string(), serde_json::Value::Bool(true));
                    obj.remove("historyTruncated");
                    obj.remove("historyDroppedCount");
                }

                // Inject replying state so frontend restores thinking
                // indicator and voice-pending state after page reload.
                let chat = ctx.state.chat();
                let active_keys = chat.active_session_keys().await;
                let replying = active_keys.iter().any(|k| k == key);
                let tool_invocations = if replying {
                    chat.active_tool_invocations(key).await
                } else {
                    Vec::new()
                };
                let voice_pending = replying && chat.active_voice_pending(key).await;
                let queued_prompts = chat
                    .queued_prompts_status(chelix_sessions::SessionKey::new(key))
                    .await
                    .map_err(ErrorShape::from)?;
                let queued_prompts = queued_prompts_response(queued_prompts)?;
                if let Some(obj) = result.as_object_mut() {
                    insert_session_activity_snapshot(
                        obj,
                        replying,
                        tool_invocations,
                        voice_pending,
                    );
                    obj.insert("queuedPrompts".to_string(), queued_prompts);
                }

                Ok(result)
            })
        }),
    );

    // TTS and STT (voice feature)
    #[cfg(feature = "voice")]
    {
        reg.register(
            "tts.status",
            Box::new(|ctx| {
                Box::pin(async move {
                    let mut status = ctx
                        .state
                        .services
                        .tts
                        .status()
                        .await
                        .map_err(ErrorShape::from)?;

                    // Enrich with active persona info.
                    if let Some(ref store) = ctx.state.services.voice_persona_store
                        && let Ok(Some(active)) = store.get_active().await
                    {
                        status["persona"] = serde_json::json!({
                            "id": active.persona.id,
                            "label": active.persona.label,
                        });
                    }

                    Ok(status)
                })
            }),
        );
        reg.register(
            "tts.providers",
            Box::new(|ctx| {
                Box::pin(async move {
                    ctx.state
                        .services
                        .tts
                        .providers()
                        .await
                        .map_err(ErrorShape::from)
                })
            }),
        );
        reg.register(
            "tts.enable",
            Box::new(|ctx| {
                Box::pin(async move {
                    ctx.state
                        .services
                        .tts
                        .enable(ctx.params.clone())
                        .await
                        .map_err(ErrorShape::from)
                })
            }),
        );
        reg.register(
            "tts.disable",
            Box::new(|ctx| {
                Box::pin(async move {
                    ctx.state
                        .services
                        .tts
                        .disable()
                        .await
                        .map_err(ErrorShape::from)
                })
            }),
        );
        reg.register(
            "tts.convert",
            Box::new(|ctx| {
                Box::pin(async move {
                    let mut params = ctx.params.clone();

                    // Resolve voice persona through the full chain:
                    // explicit personaId → session agent's voice_persona_id → global active.
                    if params.get("persona").is_none()
                        && let Some(ref vp_store) = ctx.state.services.voice_persona_store
                    {
                        let explicit_id = params.get("personaId").and_then(|v| v.as_str());
                        let session_key = params
                            .get("_session_key")
                            .and_then(|v| v.as_str())
                            .map(String::from);

                        let persona = crate::voice_persona::resolve_persona(
                            vp_store,
                            ctx.state.services.agents_config.as_deref(),
                            explicit_id,
                            session_key.as_deref(),
                            ctx.state.services.session_metadata.as_deref(),
                        )
                        .await;

                        if let Some(persona) = persona
                            && let Ok(v) = serde_json::to_value(&persona)
                        {
                            params["persona"] = v;
                        }
                    }

                    ctx.state
                        .services
                        .tts
                        .convert(params)
                        .await
                        .map_err(ErrorShape::from)
                })
            }),
        );
        reg.register(
            "tts.setProvider",
            Box::new(|ctx| {
                Box::pin(async move {
                    ctx.state
                        .services
                        .tts
                        .set_provider(ctx.params.clone())
                        .await
                        .map_err(ErrorShape::from)
                })
            }),
        );
        reg.register(
            "stt.status",
            Box::new(|ctx| {
                Box::pin(async move {
                    ctx.state
                        .services
                        .stt
                        .status()
                        .await
                        .map_err(ErrorShape::from)
                })
            }),
        );
        reg.register(
            "stt.providers",
            Box::new(|ctx| {
                Box::pin(async move {
                    ctx.state
                        .services
                        .stt
                        .providers()
                        .await
                        .map_err(ErrorShape::from)
                })
            }),
        );
        reg.register(
            "stt.transcribe",
            Box::new(|ctx| {
                Box::pin(async move {
                    ctx.state
                        .services
                        .stt
                        .transcribe(ctx.params.clone())
                        .await
                        .map_err(ErrorShape::from)
                })
            }),
        );
        reg.register(
            "stt.setProvider",
            Box::new(|ctx| {
                Box::pin(async move {
                    ctx.state
                        .services
                        .stt
                        .set_provider(ctx.params.clone())
                        .await
                        .map_err(ErrorShape::from)
                })
            }),
        );
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use {
        super::{
            ActiveToolInvocation, QueuedPromptsRemoveParams, QueuedPromptsStatusParams,
            insert_session_activity_snapshot,
        },
        crate::{
            auth::{AuthMode, ResolvedAuth},
            methods::{MethodContext, MethodRegistry, MethodTransport},
            services::GatewayServices,
            state::GatewayState,
        },
    };

    fn active_tool_invocation() -> ActiveToolInvocation {
        ActiveToolInvocation {
            lifecycle: chelix_common::tool_lifecycle::ToolLifecycleEvent {
                tool_call_id: "tool-1".to_owned(),
                tool_name: "execute_command".to_owned(),
                sequence: 1,
                emitted_at_ms: 1,
                run_id: Some("run-1".to_owned()),
                context_budget: None,
                update: chelix_common::tool_lifecycle::ToolLifecycleUpdate::Executing {
                    arguments: serde_json::json!({"command": "ls"}),
                    started_at_ms: 1,
                },
            },
            execution_mode: None,
            accumulated_arguments: None,
            context_budget: None,
        }
    }

    #[test]
    fn session_activity_snapshot_includes_active_tool_invocations_only_when_replying() {
        let mut obj = serde_json::Map::new();

        insert_session_activity_snapshot(&mut obj, true, vec![active_tool_invocation()], true);

        assert_eq!(obj.get("replying").and_then(|v| v.as_bool()), Some(true));
        assert_eq!(
            obj.get("voicePending").and_then(|v| v.as_bool()),
            Some(true)
        );
        let tool_call = obj
            .get("activeToolInvocations")
            .and_then(|v| v.as_array())
            .and_then(|calls| calls.first())
            .unwrap_or_else(|| panic!("active tool call is included"));
        assert_eq!(
            tool_call.get("toolName").and_then(|v| v.as_str()),
            Some("execute_command")
        );
    }

    #[test]
    fn session_activity_snapshot_omits_active_fields_when_idle() {
        let mut obj = serde_json::Map::new();

        insert_session_activity_snapshot(&mut obj, false, vec![active_tool_invocation()], true);

        assert_eq!(obj.get("replying").and_then(|v| v.as_bool()), Some(false));
        assert!(obj.get("activeToolInvocations").is_none());
        assert!(obj.get("voicePending").is_none());
    }

    #[test]
    fn queued_prompts_status_params_accept_the_canonical_payload() {
        let params: QueuedPromptsStatusParams =
            serde_json::from_value(serde_json::json!({ "sessionKey": "session:one" }))
                .unwrap_or_else(|error| panic!("canonical status params must parse: {error}"));

        assert_eq!(params.session_key.as_str(), "session:one");
    }

    #[test]
    fn queued_prompts_status_params_reject_an_additional_field() {
        assert!(
            serde_json::from_value::<QueuedPromptsStatusParams>(serde_json::json!({
                "sessionKey": "session:one",
                "additional": true,
            }))
            .is_err()
        );
    }

    #[test]
    fn queued_prompts_remove_params_accept_the_canonical_payload() {
        let params: QueuedPromptsRemoveParams =
            serde_json::from_value(serde_json::json!({ "id": 42 }))
                .unwrap_or_else(|error| panic!("canonical remove params must parse: {error}"));

        assert_eq!(params.id, 42);
    }

    #[test]
    fn queued_prompts_remove_params_reject_an_additional_field() {
        assert!(
            serde_json::from_value::<QueuedPromptsRemoveParams>(serde_json::json!({
                "id": 42,
                "additional": true,
            }))
            .is_err()
        );
    }

    #[tokio::test]
    async fn chat_auxiliary_rpc_parsers_reject_invalid_payloads_before_session_lookup() {
        let state = GatewayState::new(
            ResolvedAuth {
                mode: AuthMode::Token,
                token: None,
                password: None,
            },
            GatewayServices::noop(),
        );
        let registry = MethodRegistry::new();

        for method in [
            "chat.compact",
            "chat.context",
            "chat.raw_prompt",
            "chat.full_context",
        ] {
            for params in [
                serde_json::Value::Null,
                serde_json::json!({ "unexpected": true }),
            ] {
                let response = registry
                    .dispatch(MethodContext {
                        request_id: format!("{method}-invalid"),
                        method: method.to_string(),
                        params,
                        client_conn_id: "unbound-client".into(),
                        transport: MethodTransport::StatefulConnection,
                        client_role: "operator".into(),
                        client_scopes: vec![
                            chelix_protocol::scopes::READ.into(),
                            chelix_protocol::scopes::WRITE.into(),
                        ],
                        state: Arc::clone(&state),
                        channel: None,
                    })
                    .await;
                let error = response
                    .error
                    .unwrap_or_else(|| panic!("{method} invalid payload should fail"));
                assert_eq!(error.code, chelix_protocol::error_codes::INVALID_REQUEST);
                assert!(
                    error
                        .message
                        .starts_with(&format!("invalid {method} request:")),
                    "{method} read session state before rejecting its payload: {}",
                    error.message
                );
            }
        }
    }

    #[test]
    fn stateless_http_session_switch_is_rejected_without_mutating_connection_state() {
        let state = GatewayState::new(
            ResolvedAuth {
                mode: AuthMode::Token,
                token: None,
                password: None,
            },
            GatewayServices::noop(),
        );
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap_or_else(|error| panic!("failed to build runtime: {error}"));
        runtime.block_on(async {
            let mut registry = state.client_registry.write().await;
            registry
                .active_sessions
                .insert("http-rpc".into(), "session:existing".into());
            registry
                .active_projects
                .insert("http-rpc".into(), "project:existing".into());
        });

        let context = MethodContext {
            request_id: "switch".into(),
            method: "sessions.switch".into(),
            params: serde_json::json!({
                "key": "session:replacement",
                "project_id": "project:replacement",
            }),
            client_conn_id: "http-rpc".into(),
            transport: MethodTransport::StatelessHttp {
                session_id: Some(chelix_sessions::SessionKey::new("session:http")),
            },
            client_role: "operator".into(),
            client_scopes: vec![chelix_protocol::scopes::WRITE.into()],
            state: Arc::clone(&state),
            channel: None,
        };
        let response = runtime.block_on(MethodRegistry::new().dispatch(context));

        assert!(!response.ok);
        assert_eq!(
            response.error.as_ref().map(|error| error.code.as_str()),
            Some(chelix_protocol::error_codes::INVALID_REQUEST)
        );
        runtime.block_on(async {
            let registry = state.client_registry.read().await;
            assert_eq!(
                registry.active_sessions.get("http-rpc").map(String::as_str),
                Some("session:existing")
            );
            assert_eq!(
                registry.active_projects.get("http-rpc").map(String::as_str),
                Some("project:existing")
            );
        });
    }
}
