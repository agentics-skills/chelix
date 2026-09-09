//! `ChatService` trait implementation for `LiveChatService`.

mod send;

use std::{
    collections::{HashMap, HashSet},
    future::Future,
    path::Path,
    sync::Arc,
};

use {
    async_trait::async_trait, serde_json::Value, tokio::sync::RwLock,
    tokio_util::sync::CancellationToken, tracing::info,
};

use {
    chelix_agents::{
        ChatMessage, UserContent,
        model::values_to_chat_messages,
        prompt::{
            build_system_prompt_minimal_runtime_details,
            build_system_prompt_with_session_runtime_details,
        },
    },
    chelix_config::ToolMode,
    chelix_service_traits::{
        ChatCompactRequest, ChatContextRequest, ChatExecutionContext, ChatFullContextRequest,
        ChatRawPromptRequest, ChatService, ServiceError, ServiceResult,
    },
    chelix_sessions::{MessageContent, PersistedMessage},
    chelix_tools::policy::PolicyContext,
};

use crate::{
    channels::notify_channels_of_compaction,
    compaction,
    memory_tools::MemoryForgetProviderResolver,
    prompt::{
        apply_chat_execution_context, build_policy_context, build_prompt_runtime_context,
        clear_prompt_memory_snapshot, discover_skills_if_enabled, filter_skills_for_agent,
        load_prompt_persona_for_session, prepare_run_registry, prompt_build_limits_from_config,
        resolve_prompt_agent_id,
    },
    run_with_tools::run_with_tools,
    streaming::run_streaming,
    types::*,
};

use super::*;

async fn ensure_send_sync_session_agent(
    metadata: &chelix_sessions::metadata::SqliteSessionMetadata,
    entry: chelix_sessions::metadata::SessionEntry,
    agent_id: &str,
) -> Result<chelix_sessions::metadata::SessionEntry, ServiceError> {
    if entry
        .agent_id
        .as_deref()
        .is_some_and(|id| !id.trim().is_empty())
    {
        return Ok(entry);
    }
    let model_reasoning = entry
        .model_reasoning()
        .cloned()
        .ok_or_else(|| ServiceError::message("LLM session has no model/reasoning pair"))?;
    metadata
        .assign_agent(&entry.key, agent_id, &model_reasoning)
        .await
        .map_err(ServiceError::message)
}

async fn persist_send_sync_session_backing(
    metadata: &chelix_sessions::metadata::SqliteSessionMetadata,
    session_entry: Option<chelix_sessions::metadata::SessionEntry>,
    session_key: &str,
    model_reasoning: &chelix_common::ResolvedModelReasoning,
    agent_id: &str,
) -> Result<chelix_sessions::metadata::SessionEntry, ServiceError> {
    match session_entry {
        Some(entry) if entry.model_reasoning().is_none() => metadata
            .promote_external_to_llm(session_key, model_reasoning, agent_id)
            .await
            .map(chelix_sessions::metadata::PromoteExternalToLlmOutcome::into_entry)
            .map_err(ServiceError::message),
        Some(entry) if entry.model_reasoning() != Some(model_reasoning) => {
            let entry = metadata
                .set_model_reasoning(session_key, model_reasoning)
                .await
                .map_err(ServiceError::message)?;
            ensure_send_sync_session_agent(metadata, entry, agent_id).await
        },
        Some(entry) => ensure_send_sync_session_agent(metadata, entry, agent_id).await,
        None => match metadata
            .ensure_llm_session(session_key, None, model_reasoning, Some(agent_id))
            .await
            .map_err(ServiceError::message)?
        {
            chelix_sessions::metadata::EnsureLlmSessionOutcome::Created(entry) => Ok(entry),
            chelix_sessions::metadata::EnsureLlmSessionOutcome::ExistingLlm(entry) => {
                ensure_send_sync_session_agent(metadata, entry, agent_id).await
            },
            chelix_sessions::metadata::EnsureLlmSessionOutcome::ExistingExternal(_) => metadata
                .promote_external_to_llm(session_key, model_reasoning, agent_id)
                .await
                .map(chelix_sessions::metadata::PromoteExternalToLlmOutcome::into_entry)
                .map_err(ServiceError::message),
        },
    }
}

fn tool_mode_enables_tools(tool_mode: ToolMode) -> bool {
    !matches!(tool_mode, ToolMode::Off)
}

async fn resolve_send_sync_outcome<F, Fut>(result: ChatRunOutcome, on_failed: F) -> ServiceResult
where
    F: FnOnce() -> Fut,
    Fut: Future<Output = ServiceResult>,
{
    match result {
        ChatRunOutcome::Completed(assistant_output) => Ok(serde_json::json!({
            "text": assistant_output.text,
            "inputTokens": assistant_output.input_tokens,
            "outputTokens": assistant_output.output_tokens,
            "cacheReadTokens": assistant_output.cache_read_tokens,
            "cacheWriteTokens": assistant_output.cache_write_tokens,
            "durationMs": assistant_output.duration_ms,
            "requestInputTokens": assistant_output.request_input_tokens,
            "requestOutputTokens": assistant_output.request_output_tokens,
            "requestCacheReadTokens": assistant_output.request_cache_read_tokens,
            "requestCacheWriteTokens": assistant_output.request_cache_write_tokens,
        })),
        ChatRunOutcome::Cancelled => Err("agent run cancelled".into()),
        ChatRunOutcome::Failed => on_failed().await,
    }
}

#[async_trait]
impl ChatService for LiveChatService {
    async fn send(
        &self,
        request: chelix_service_traits::ChatSendRequest,
        context: ChatExecutionContext,
    ) -> ServiceResult {
        self.send_impl(request, context).await
    }

    async fn send_sync(
        &self,
        request: chelix_service_traits::ChatSendSyncRequest,
        context: ChatExecutionContext,
    ) -> ServiceResult {
        let session_key = context.session_id.as_str().to_string();
        if session_key.is_empty() {
            return Err(ServiceError::message("session ID must not be empty"));
        }
        let text = request.text;
        let desired_reply_medium = crate::message::explicit_reply_medium_override(&text)
            .or(request.input_medium)
            .or_else(|| {
                context.channel.as_ref().and_then(|channel| {
                    matches!(
                        channel.message_kind,
                        Some(chelix_channels::ChannelMessageKind::Voice)
                    )
                    .then_some(ReplyMedium::Voice)
                })
            })
            .unwrap_or(ReplyMedium::Text);
        let resolved = self
            .resolve_chat_turn(&context.session_id, request.model_override.as_ref())
            .await?;
        let stream_only = resolved.stream_only;
        let provider = Arc::clone(resolved.model.provider());
        let model_reasoning = resolved.model.model_reasoning().clone();
        let resolved_reasoning_effort =
            Some(model_reasoning.reasoning_effort().as_str().to_string());
        let mut session_entry = self
            .session_metadata
            .get(&session_key)
            .await
            .map_err(ServiceError::message)?;
        let runtime_config = self
            .load_runtime_config_for_agent_run()
            .await
            .map_err(ServiceError::message)?;
        let persona = load_prompt_persona_for_session(
            &runtime_config,
            &session_key,
            session_entry.as_ref(),
            context.agent_id.as_deref(),
            self.session_state_store.as_deref(),
        )
        .await
        .map_err(ServiceError::message)?;
        let session_agent_id = persona.agent_id.clone();
        let runtime_limits = persona
            .config
            .agent_runtime_limits(&session_agent_id)
            .map_err(ServiceError::message)?;
        session_entry = Some(
            persist_send_sync_session_backing(
                self.session_metadata.as_ref(),
                session_entry,
                &session_key,
                &model_reasoning,
                &session_agent_id,
            )
            .await?,
        );
        let user_msg = PersistedMessage::User {
            content: MessageContent::Text(text.clone()),
            created_at: Some(now_ms()),
            audio: None,
            documents: None,
            channel: None,
            seq: None,
            run_id: None,
        };

        self.session_store
            .append(&session_key, &user_msg.to_value())
            .await
            .map_err(ServiceError::message)?;
        let ui_message_count = self
            .session_store
            .ui_message_count(&session_key)
            .await
            .map_err(ServiceError::message)?;
        self.session_metadata
            .touch(&session_key, ui_message_count)
            .await
            .map_err(ServiceError::message)?;
        let mut runtime_context = build_prompt_runtime_context(
            &self.state,
            &persona.config,
            &provider,
            &session_key,
            session_entry.as_ref(),
        )
        .await;
        apply_chat_execution_context(
            &mut runtime_context.host,
            &context,
            persona
                .user
                .timezone
                .as_ref()
                .map(|timezone| timezone.name()),
        );

        // Load conversation history (excluding the message we just appended).
        let mut history = self
            .session_store
            .read(&session_key)
            .await
            .map_err(ServiceError::message)?;
        if !history.is_empty() {
            history.pop();
        }
        let chat_history = values_to_chat_messages(&history).map_err(ServiceError::message)?;

        let run_id = uuid::Uuid::new_v4().to_string();
        let cancellation_token = CancellationToken::new();
        let state = Arc::clone(&self.state);
        let tool_registry = if let Some(policy) = context.tool_policy.as_ref() {
            let registry_guard = self.tool_registry.read().await;
            Arc::new(RwLock::new(
                registry_guard.clone_allowed_by(|name| policy.is_allowed(name)),
            ))
        } else {
            Arc::clone(&self.tool_registry)
        };
        let hook_registry = self.hook_registry.clone();
        let accept_language = context.accept_language.clone();
        let conn_id = context.connection_id().map(str::to_string);
        let sender_name = context.channel.as_ref().and_then(|channel| {
            channel
                .sender_name
                .clone()
                .or_else(|| channel.username.clone())
        });
        let mcp_disabled = session_entry
            .as_ref()
            .and_then(|entry| entry.mcp_disabled)
            .unwrap_or(false);
        let tool_choice = request.tool_choice;
        let provider_name = provider.name().to_string();
        let model_id = provider.id().to_string();

        self.active_runs
            .write()
            .await
            .insert(run_id.clone(), cancellation_token.clone());
        self.active_runs_by_session
            .write()
            .await
            .insert(session_key.clone(), run_id.clone());
        self.active_reply_medium
            .write()
            .await
            .insert(session_key.clone(), desired_reply_medium);
        self.active_partial_assistant.write().await.insert(
            session_key.clone(),
            ActiveAssistantDraft::new(
                &run_id,
                &model_id,
                &provider_name,
                resolved_reasoning_effort.clone(),
                None,
            ),
        );

        info!(
            run_id = %run_id,
            user_message = %text,
            model = %model_id,
            stream_only,
            session = %session_key,
            reply_medium = ?desired_reply_medium,
            "chat.send_sync"
        );

        if desired_reply_medium == ReplyMedium::Voice {
            broadcast(
                &state,
                "chat",
                serde_json::json!({
                    "runId": run_id,
                    "sessionKey": session_key,
                    "state": "voice_pending",
                }),
                BroadcastOpts::default(),
            )
            .await;
        }

        // send_sync is text-only (used by API calls and channels).
        let user_content = UserContent::text(&text);
        let active_event_forwarders = Arc::new(RwLock::new(HashMap::new()));
        let terminal_runs = Arc::new(RwLock::new(HashSet::new()));
        let result = if stream_only {
            run_streaming(
                persona,
                &cancellation_token,
                &state,
                &run_id,
                provider,
                &model_id,
                &user_content,
                &provider_name,
                &chat_history,
                &session_key,
                &session_agent_id,
                resolved_reasoning_effort.clone(),
                desired_reply_medium,
                None,
                &[],
                Some(&runtime_context),
                sender_name,
                Some(&self.session_store),
                None, // send_sync: no client seq
                Some(Arc::clone(&self.active_partial_assistant)),
                &terminal_runs,
            )
            .await
        } else {
            run_with_tools(
                persona,
                runtime_limits,
                &cancellation_token,
                &state,
                &run_id,
                provider,
                MemoryForgetProviderResolver::new(
                    Arc::clone(&self.providers),
                    Arc::clone(&self.session_metadata),
                ),
                &tool_registry,
                &user_content,
                &provider_name,
                &history,
                &chat_history,
                &session_key,
                &session_agent_id,
                resolved_reasoning_effort.clone(),
                desired_reply_medium,
                None,
                Some(&runtime_context),
                &[],
                hook_registry,
                accept_language,
                conn_id,
                Some(&self.session_store),
                mcp_disabled,
                None, // send_sync: no client seq
                Some(Arc::clone(&self.active_tool_invocations)),
                Some(Arc::clone(&self.active_partial_assistant)),
                &active_event_forwarders,
                &terminal_runs,
                sender_name,
                tool_choice,
            )
            .await
        };

        self.active_runs.write().await.remove(&run_id);
        let mut runs_by_session = self.active_runs_by_session.write().await;
        if runs_by_session.get(&session_key) == Some(&run_id) {
            runs_by_session.remove(&session_key);
        }
        drop(runs_by_session);
        self.active_tool_invocations
            .write()
            .await
            .remove(&session_key);
        terminal_runs.write().await.remove(&run_id);
        self.active_partial_assistant
            .write()
            .await
            .remove(&session_key);
        self.active_reply_medium.write().await.remove(&session_key);

        if let Ok(count) = self.session_store.ui_message_count(&session_key).await {
            self.session_metadata
                .touch(&session_key, count)
                .await
                .map_err(ServiceError::message)?;
        }

        resolve_send_sync_outcome(result, || async {
            // Check the last broadcast for this run to get the actual error message.
            let error_msg = state
                .last_run_error(&run_id)
                .await
                .unwrap_or_else(|| "agent run failed (check server logs)".to_string());

            // Update metadata so the session shows in the UI.
            if let Ok(count) = self.session_store.ui_message_count(&session_key).await {
                self.session_metadata
                    .touch(&session_key, count)
                    .await
                    .map_err(ServiceError::message)?;
            }

            Err(error_msg.into())
        })
        .await
    }

    async fn abort(&self, params: Value) -> ServiceResult {
        let run_id = params.get("runId").and_then(|v| v.as_str());
        let session_key = params.get("sessionKey").and_then(|v| v.as_str());
        if run_id.is_none() && session_key.is_none() {
            return Err("missing 'runId' or 'sessionKey'".into());
        }

        let resolved_session_key =
            Self::resolve_session_key_for_run(&self.active_runs_by_session, run_id, session_key)
                .await;
        let (resolved_run_id, aborted) = Self::cancel_run(
            &self.active_runs,
            &self.active_runs_by_session,
            &self.terminal_runs,
            run_id,
            session_key,
        )
        .await;
        info!(
            requested_run_id = ?run_id,
            session_key = ?session_key,
            resolved_run_id = ?resolved_run_id,
            aborted,
            "chat.abort"
        );

        Ok(serde_json::json!({
            "aborted": aborted,
            "runId": resolved_run_id,
            "sessionKey": resolved_session_key,
        }))
    }

    async fn queued_prompts_status(
        &self,
        session_id: chelix_sessions::SessionKey,
    ) -> Result<chelix_sessions::QueuedPromptsStatus, ServiceError> {
        self.queued_prompts
            .status(session_id)
            .await
            .map_err(|error| ServiceError::message(error.to_string()))
    }

    async fn queued_prompts_remove(
        &self,
        id: i64,
    ) -> Result<chelix_sessions::QueuedPromptsStatus, ServiceError> {
        let status = self
            .queued_prompts
            .remove(id)
            .await
            .map_err(|error| ServiceError::message(error.to_string()))?;
        crate::prompt_queue::broadcast_queued_prompts_status(&self.state, &status)
            .await
            .map_err(|error| ServiceError::message(error.to_string()))?;
        Ok(status)
    }

    async fn history(&self, params: Value) -> ServiceResult {
        let session_key = self.resolve_session_key_from_params(&params).await;
        let history = self
            .session_store
            .ui_history
            .history(&session_key)
            .await
            .map_err(ServiceError::message)?;
        Ok(serde_json::json!(history))
    }

    async fn inject(&self, _params: Value) -> ServiceResult {
        Err("inject not yet implemented".into())
    }

    async fn clear(&self, params: Value) -> ServiceResult {
        let session_key = self.resolve_session_key_from_params(&params).await;

        self.session_store
            .clear(&session_key)
            .await
            .map_err(ServiceError::message)?;

        // Reset client sequence tracking for this session. A cleared chat starts
        // a fresh sequence from the web UI.
        {
            let mut seq_map = self.last_client_seq.write().await;
            seq_map.remove(&session_key);
        }

        // Reset metadata message count and preview.
        self.session_metadata
            .touch(&session_key, 0)
            .await
            .map_err(ServiceError::message)?;
        self.session_metadata
            .set_preview(&session_key, None)
            .await
            .map_err(ServiceError::message)?;

        // Notify all WebSocket clients so the web UI clears the session
        // even when /clear is issued from a channel (e.g. Telegram).
        broadcast(
            &self.state,
            "chat",
            serde_json::json!({
                "sessionKey": session_key,
                "state": "session_cleared",
            }),
            BroadcastOpts::default(),
        )
        .await;

        info!(session = %session_key, "chat.clear");
        Ok(serde_json::json!({ "ok": true }))
    }

    async fn compact(
        &self,
        _request: ChatCompactRequest,
        context: ChatExecutionContext,
    ) -> ServiceResult {
        let session_id = context.session_id.clone();
        let session_key = session_id.as_str();
        let resolved = self.resolve_chat_turn(&session_id, None).await?;
        let provider = Arc::clone(resolved.model.provider());

        let history = self
            .session_store
            .read(session_key)
            .await
            .map_err(ServiceError::message)?;

        if history.is_empty() {
            return Err("nothing to compact".into());
        }

        // Rebuild the session system prompt and tool schemas exactly as a
        // regular turn would, so the summarization request shares the
        // provider prompt-cache prefix with the previous turn.
        let (system_prompt, tools) = self
            .session_prompt_context(session_key, &history, &provider, &context)
            .await
            .map_err(ServiceError::message)?;

        let outcome = compaction::summarize_session(
            &self.session_store,
            session_key,
            &*provider,
            &system_prompt,
            &tools,
        )
        .await
        .map_err(|e| ServiceError::message(e.to_string()))?;

        let message_count = self
            .session_store
            .ui_message_count(session_key)
            .await
            .map_err(ServiceError::message)?;
        self.session_metadata
            .touch(session_key, message_count)
            .await
            .map_err(ServiceError::message)?;

        let compact_payload = serde_json::json!({
            "sessionKey": session_key,
            "state": "compact",
            "phase": "done",
        });
        broadcast(
            &self.state,
            "chat",
            compact_payload,
            BroadcastOpts::default(),
        )
        .await;

        // Notify any channel (Telegram, Matrix, WhatsApp, etc.)
        // that has pending reply targets on this session.
        notify_channels_of_compaction(&self.state, session_key, &outcome).await;

        info!(session = %session_key, "chat.compact: done");
        Ok(outcome.message)
    }

    async fn context(
        &self,
        _request: ChatContextRequest,
        context: ChatExecutionContext,
    ) -> ServiceResult {
        let session_id = context.session_id.clone();
        let session_key = session_id.as_str();
        let resolved = self.resolve_chat_turn(&session_id, None).await?;
        let model_id = resolved.model.model_reasoning().model_id().to_string();
        let provider = Arc::clone(resolved.model.provider());

        // Session info
        let message_count = self
            .session_store
            .ui_message_count(session_key)
            .await
            .map_err(ServiceError::message)?;
        let session_entry = self
            .session_metadata
            .get(session_key)
            .await
            .map_err(ServiceError::message)?;
        let prompt_persona = self
            .load_prompt_persona_for_agent_run(session_key, session_entry.as_ref(), None)
            .await
            .map_err(ServiceError::message)?;
        let messages = self
            .session_store
            .read(session_key)
            .await
            .map_err(ServiceError::message)?;
        let provider_name = provider.name().to_string();
        let tools_enabled = tool_mode_enables_tools(provider.tool_mode());
        let session_info = serde_json::json!({
            "key": session_key,
            "messageCount": message_count,
            "model": model_id,
            "provider": provider_name,
            "label": session_entry.as_ref().and_then(|e| e.label.as_deref()),
            "projectId": session_entry.as_ref().and_then(|e| e.project_id.as_deref()),
        });

        // Project info & context files
        let project_id = if let Some(connection_id) = context.connection_id() {
            self.state.active_project_id(connection_id).await
        } else {
            None
        };
        let project_id =
            project_id.or_else(|| session_entry.as_ref().and_then(|e| e.project_id.clone()));

        let project_info = if let Some(pid) = project_id {
            match self
                .state
                .project_service()
                .get(serde_json::json!({"id": pid}))
                .await
            {
                Ok(val) => {
                    let dir = val.get("directory").and_then(|v| v.as_str());
                    let context_files = if let Some(d) = dir {
                        match chelix_projects::context::load_context_files(Path::new(d)) {
                            Ok(files) => files
                                .iter()
                                .map(|f| {
                                    serde_json::json!({
                                        "path": f.path.display().to_string(),
                                        "size": f.content.len(),
                                    })
                                })
                                .collect::<Vec<_>>(),
                            Err(_) => vec![],
                        }
                    } else {
                        vec![]
                    };
                    serde_json::json!({
                        "id": val.get("id"),
                        "label": val.get("label"),
                        "directory": dir,
                        "systemPrompt": val.get("system_prompt").or(val.get("systemPrompt")),
                        "contextFiles": context_files,
                    })
                },
                Err(_) => serde_json::json!(null),
            }
        } else {
            serde_json::json!(null)
        };

        // Tools (only include when the configured tool mode enables them)
        let mcp_disabled = session_entry
            .as_ref()
            .and_then(|e| e.mcp_disabled)
            .unwrap_or(false);
        // `messages` is reused for token usage and lazy schema visibility.
        // `tools` is the UI discovery catalog (name + description of every
        // allowed public tool, plus `get_tool` in lazy mode). `toolSchemaCount`
        // separately reports how many parameter schemas are currently visible.
        let (tools, tool_schema_count): (Vec<Value>, usize) = if tools_enabled {
            let registry_guard = self.tool_registry.read().await;
            let list_agent_id = prompt_persona.agent_id.clone();
            let list_ctx = PolicyContext {
                agent_id: list_agent_id.clone(),
                ..Default::default()
            };
            let memory_setup = self.state.memory_manager().map(|manager| {
                (
                    manager,
                    MemoryForgetProviderResolver::new(
                        Arc::clone(&self.providers),
                        Arc::clone(&self.session_metadata),
                    ),
                )
            });
            let effective_registry = prepare_run_registry(
                &registry_guard,
                &prompt_persona.config,
                &[],
                mcp_disabled,
                &list_ctx,
                true,
                &list_agent_id,
                memory_setup,
                &messages,
            )
            .map_err(|error| ServiceError::message(error.to_string()))?;
            let catalog = effective_registry
                .list_catalog()
                .into_iter()
                .map(|entry| {
                    serde_json::json!({
                        "name": entry.name,
                        "description": entry.description,
                    })
                })
                .collect();
            (catalog, effective_registry.list_schemas().len())
        } else {
            (vec![], 0)
        };

        // Token usage from API-reported counts stored in messages.
        let usage = session_token_usage_from_messages(&messages);
        let total_tokens = usage.session_input_tokens
            + usage.session_output_tokens
            + usage.session_cache_read_tokens
            + usage.session_cache_write_tokens;
        let current_total_tokens = usage.current_request_input_tokens
            + usage.current_request_output_tokens
            + usage.current_request_cache_read_tokens
            + usage.current_request_cache_write_tokens;

        // Context window from the same resolved provider used for this request.
        let context_window = provider.context_window().ok_or_else(|| {
            ServiceError::message(format!(
                "model '{}' has no resolved context_length metadata",
                provider.id()
            ))
        })?;

        // Sandbox info
        let router = self.state.sandbox_router();
        let sandbox_enabled = router.enabled();
        let sandbox_config = router.config();
        let effective_image = router.default_image().await;
        let container_name = {
            let owner_key = session_entry
                .as_ref()
                .and_then(|entry| entry.sandbox_owner_key.as_deref())
                .unwrap_or(session_key);
            let id = router.sandbox_id_for(owner_key);
            format!(
                "{}-{}",
                sandbox_config
                    .container_prefix
                    .as_deref()
                    .unwrap_or("chelix-sandbox"),
                id.key
            )
        };
        let sandbox_info = serde_json::json!({
            "enabled": sandbox_enabled,
            "backend": router.backend_id(),
            "mode": sandbox_config.mode,
            "scope": sandbox_config.scope,
            "image": effective_image,
            "containerName": container_name,
        });
        // Discover enabled skills/plugins (only if the configured mode enables tools and
        // `[skills] enabled` is true — see #655).
        let skills_list: Vec<Value> = if tools_enabled {
            discover_skills_if_enabled(&prompt_persona.config)
                .await
                .iter()
                .map(|s| {
                    serde_json::json!({
                        "name": s.name,
                        "description": s.description,
                        "source": s.source,
                    })
                })
                .collect()
        } else {
            vec![]
        };

        // MCP servers (only if the configured mode enables tools)
        let mcp_servers = if tools_enabled {
            self.state
                .mcp_service()
                .list()
                .await
                .unwrap_or(serde_json::json!([]))
        } else {
            serde_json::json!([])
        };

        Ok(serde_json::json!({
            "session": session_info,
            "project": project_info,
            "tools": tools,
            "toolSchemaCount": tool_schema_count,
            "skills": skills_list,
            "mcpServers": mcp_servers,
            "mcpDisabled": mcp_disabled,
            "sandbox": sandbox_info,
            "promptMemory": prompt_persona.memory_status,
            "supportsTools": tools_enabled,
            "tokenUsage": {
                "inputTokens": usage.session_input_tokens,
                "outputTokens": usage.session_output_tokens,
                "cacheReadTokens": usage.session_cache_read_tokens,
                "cacheWriteTokens": usage.session_cache_write_tokens,
                "total": total_tokens,
                "currentInputTokens": usage.current_request_input_tokens,
                "currentOutputTokens": usage.current_request_output_tokens,
                "currentCacheReadTokens": usage.current_request_cache_read_tokens,
                "currentCacheWriteTokens": usage.current_request_cache_write_tokens,
                "currentTotal": current_total_tokens,
                "estimatedNextInputTokens": usage.current_request_input_tokens,
                "contextWindow": context_window,
            },
        }))
    }

    async fn raw_prompt(
        &self,
        _request: ChatRawPromptRequest,
        context: ChatExecutionContext,
    ) -> ServiceResult {
        let session_id = context.session_id.clone();
        let session_key = session_id.as_str();
        let resolved = self.resolve_chat_turn(&session_id, None).await?;
        let provider = Arc::clone(resolved.model.provider());
        let history = self
            .session_store
            .read(session_key)
            .await
            .map_err(ServiceError::message)?;
        let tool_mode = provider.tool_mode();
        let native_tools = matches!(tool_mode, ToolMode::Native);
        let tools_enabled = tool_mode_enables_tools(tool_mode);

        // Build runtime context.
        let session_entry = self
            .session_metadata
            .get(session_key)
            .await
            .map_err(ServiceError::message)?;
        let persona = self
            .load_prompt_persona_for_agent_run(session_key, session_entry.as_ref(), None)
            .await
            .map_err(ServiceError::message)?;
        let mut runtime_context = build_prompt_runtime_context(
            &self.state,
            &persona.config,
            &provider,
            session_key,
            session_entry.as_ref(),
        )
        .await;
        apply_chat_execution_context(
            &mut runtime_context.host,
            &context,
            persona
                .user
                .timezone
                .as_ref()
                .map(|timezone| timezone.name()),
        );

        // Resolve project context.
        let project_context = self
            .resolve_project_context(session_key, context.connection_id())
            .await
            .map_err(ServiceError::message)?;

        // Discover skills (gated on `[skills] enabled` — see #655).
        let discovered_skills = discover_skills_if_enabled(&persona.config).await;

        // Check MCP disabled.
        let mcp_disabled = session_entry
            .as_ref()
            .and_then(|entry| entry.mcp_disabled)
            .unwrap_or(false);

        let raw_prompt_agent_id = persona.agent_id.clone();

        // Apply per-agent skill policy.
        let discovered_skills = filter_skills_for_agent(discovered_skills, &persona.agent.skills);

        // Build filtered tool registry with the same preparation as the live
        // run (filter → memory tools → lazy wrap) so the debug prompt matches.
        let policy_ctx = build_policy_context(&raw_prompt_agent_id, Some(&runtime_context));
        let filtered_registry = {
            let registry_guard = self.tool_registry.read().await;
            let memory_setup = self.state.memory_manager().map(|manager| {
                (
                    manager,
                    MemoryForgetProviderResolver::new(
                        Arc::clone(&self.providers),
                        Arc::clone(&self.session_metadata),
                    ),
                )
            });
            prepare_run_registry(
                &registry_guard,
                &persona.config,
                &discovered_skills,
                mcp_disabled,
                &policy_ctx,
                tools_enabled,
                &raw_prompt_agent_id,
                memory_setup,
                &history,
            )
        }
        .map_err(|e| ServiceError::message(e.to_string()))?;

        // API-visible schema count (lazy mode: get_tool + revealed).
        let tool_count = filtered_registry.list_schemas().len();

        // Build the system prompt.
        let prompt_limits = prompt_build_limits_from_config(&persona.config);
        let prompt_build = if tools_enabled {
            build_system_prompt_with_session_runtime_details(
                &filtered_registry,
                native_tools,
                project_context.as_deref(),
                &discovered_skills,
                Some(&persona.agent),
                Some(&persona.user),
                persona.soul_text.as_deref(),
                persona.boot_text.as_deref(),
                persona.agents_text.as_deref(),
                persona.tools_text.as_deref(),
                Some(&runtime_context),
                persona.memory_text.as_deref(),
                prompt_limits,
                persona.guidelines_text.as_deref(),
            )
        } else {
            build_system_prompt_minimal_runtime_details(
                project_context.as_deref(),
                Some(&persona.agent),
                Some(&persona.user),
                persona.soul_text.as_deref(),
                persona.boot_text.as_deref(),
                persona.agents_text.as_deref(),
                persona.tools_text.as_deref(),
                Some(&runtime_context),
                persona.memory_text.as_deref(),
                prompt_limits,
                persona.guidelines_text.as_deref(),
            )
        };

        let truncated = prompt_build.metadata.truncated();
        let workspace_files = prompt_build.metadata.workspace_files.clone();
        let system_prompt = prompt_build.prompt;
        let char_count = system_prompt.len();

        Ok(serde_json::json!({
            "prompt": system_prompt,
            "charCount": char_count,
            "truncated": truncated,
            "workspaceFiles": workspace_files,
            "promptMemory": persona.memory_status,
            "native_tools": native_tools,
            "tools_enabled": tools_enabled,
            "tool_mode": format!("{:?}", tool_mode),
            "toolCount": tool_count,
        }))
    }

    /// Return the **full messages array** that would be sent to the LLM on the
    /// next call — system prompt + conversation history — in OpenAI format.
    async fn full_context(
        &self,
        _request: ChatFullContextRequest,
        context: ChatExecutionContext,
    ) -> ServiceResult {
        let session_id = context.session_id.clone();
        let session_key = session_id.as_str();
        let resolved = self.resolve_chat_turn(&session_id, None).await?;
        let provider = Arc::clone(resolved.model.provider());
        let history = self
            .session_store
            .read(session_key)
            .await
            .map_err(ServiceError::message)?;
        let tool_mode = provider.tool_mode();
        let native_tools = matches!(tool_mode, ToolMode::Native);
        let tools_enabled = tool_mode_enables_tools(tool_mode);

        // Build runtime context.
        let session_entry = self
            .session_metadata
            .get(session_key)
            .await
            .map_err(ServiceError::message)?;
        let persona = self
            .load_prompt_persona_for_agent_run(session_key, session_entry.as_ref(), None)
            .await
            .map_err(ServiceError::message)?;
        let mut runtime_context = build_prompt_runtime_context(
            &self.state,
            &persona.config,
            &provider,
            session_key,
            session_entry.as_ref(),
        )
        .await;
        apply_chat_execution_context(
            &mut runtime_context.host,
            &context,
            persona
                .user
                .timezone
                .as_ref()
                .map(|timezone| timezone.name()),
        );

        // Resolve project context.
        let project_context = self
            .resolve_project_context(session_key, context.connection_id())
            .await
            .map_err(ServiceError::message)?;

        // Discover skills (gated on `[skills] enabled` — see #655).
        let discovered_skills = discover_skills_if_enabled(&persona.config).await;

        // Check MCP disabled.
        let mcp_disabled = session_entry
            .as_ref()
            .and_then(|entry| entry.mcp_disabled)
            .unwrap_or(false);

        // Build filtered tool registry.
        let full_ctx_agent_id = persona.agent_id.clone();

        // Apply per-agent skill policy.
        let discovered_skills = filter_skills_for_agent(discovered_skills, &persona.agent.skills);
        let policy_ctx = build_policy_context(&full_ctx_agent_id, Some(&runtime_context));
        // Same preparation as the live run so the full-context prompt reflects
        // the lazy state of the current history.
        let filtered_registry = {
            let registry_guard = self.tool_registry.read().await;
            let memory_setup = self.state.memory_manager().map(|manager| {
                (
                    manager,
                    MemoryForgetProviderResolver::new(
                        Arc::clone(&self.providers),
                        Arc::clone(&self.session_metadata),
                    ),
                )
            });
            prepare_run_registry(
                &registry_guard,
                &persona.config,
                &discovered_skills,
                mcp_disabled,
                &policy_ctx,
                tools_enabled,
                &full_ctx_agent_id,
                memory_setup,
                &history,
            )
        }
        .map_err(|e| ServiceError::message(e.to_string()))?;

        // Build the system prompt.
        let prompt_limits = prompt_build_limits_from_config(&persona.config);
        let prompt_build = if tools_enabled {
            build_system_prompt_with_session_runtime_details(
                &filtered_registry,
                native_tools,
                project_context.as_deref(),
                &discovered_skills,
                Some(&persona.agent),
                Some(&persona.user),
                persona.soul_text.as_deref(),
                persona.boot_text.as_deref(),
                persona.agents_text.as_deref(),
                persona.tools_text.as_deref(),
                Some(&runtime_context),
                persona.memory_text.as_deref(),
                prompt_limits,
                persona.guidelines_text.as_deref(),
            )
        } else {
            build_system_prompt_minimal_runtime_details(
                project_context.as_deref(),
                Some(&persona.agent),
                Some(&persona.user),
                persona.soul_text.as_deref(),
                persona.boot_text.as_deref(),
                persona.agents_text.as_deref(),
                persona.tools_text.as_deref(),
                Some(&runtime_context),
                persona.memory_text.as_deref(),
                prompt_limits,
                persona.guidelines_text.as_deref(),
            )
        };

        let truncated = prompt_build.metadata.truncated();
        let workspace_files = prompt_build.metadata.workspace_files.clone();
        let system_prompt = prompt_build.prompt;
        let system_prompt_chars = system_prompt.len();

        // Keep raw assistant outputs (including provider/model/token metadata)
        // so the UI can show a debug view of what the LLM actually returned.
        let llm_outputs: Vec<Value> = history
            .iter()
            .filter(|entry| entry.get("role").and_then(|r| r.as_str()) == Some("assistant"))
            .cloned()
            .collect();

        // Build the full messages array: system prompt + conversation history.
        // `values_to_chat_messages` converts terminal tool lifecycle records to provider tool messages.
        let mut messages = Vec::with_capacity(1 + history.len());
        messages.push(ChatMessage::system(system_prompt));
        messages.extend(values_to_chat_messages(&history).map_err(ServiceError::message)?);

        let openai_messages: Vec<Value> = messages.iter().map(|m| m.to_openai_value()).collect();
        let message_count = openai_messages.len();
        let total_chars: usize = openai_messages
            .iter()
            .map(|v| serde_json::to_string(v).unwrap_or_default().len())
            .sum();

        Ok(serde_json::json!({
            "messages": openai_messages,
            "llmOutputs": llm_outputs,
            "messageCount": message_count,
            "systemPromptChars": system_prompt_chars,
            "totalChars": total_chars,
            "truncated": truncated,
            "workspaceFiles": workspace_files,
            "promptMemory": persona.memory_status,
        }))
    }

    async fn refresh_prompt_memory(&self, params: Value) -> ServiceResult {
        let session_key = self.resolve_session_key_from_params(&params).await;
        let session_entry = self
            .session_metadata
            .get(&session_key)
            .await
            .map_err(ServiceError::message)?;
        let runtime_config = self
            .load_runtime_config_for_agent_run()
            .await
            .map_err(ServiceError::message)?;
        let agent_id = resolve_prompt_agent_id(&runtime_config, session_entry.as_ref())
            .map_err(ServiceError::message)?;
        let snapshot_cleared = clear_prompt_memory_snapshot(
            &session_key,
            &agent_id,
            self.session_state_store.as_deref(),
        )
        .await;
        let persona = load_prompt_persona_for_session(
            &runtime_config,
            &session_key,
            session_entry.as_ref(),
            None,
            self.session_state_store.as_deref(),
        )
        .await
        .map_err(ServiceError::message)?;

        Ok(serde_json::json!({
            "ok": true,
            "sessionKey": session_key,
            "agentId": agent_id,
            "snapshotCleared": snapshot_cleared,
            "promptMemory": persona.memory_status,
        }))
    }

    async fn active(&self, params: Value) -> ServiceResult {
        let session_key = params
            .get("sessionKey")
            .or_else(|| params.get("session_key"))
            .and_then(|v| v.as_str())
            .ok_or_else(|| "missing 'sessionKey' parameter".to_string())?;
        let active = self
            .active_runs_by_session
            .read()
            .await
            .contains_key(session_key);
        Ok(serde_json::json!({ "active": active }))
    }

    async fn active_session_keys(&self) -> Vec<String> {
        self.active_runs_by_session
            .read()
            .await
            .keys()
            .cloned()
            .collect()
    }

    async fn active_voice_pending(&self, session_key: &str) -> bool {
        self.active_reply_medium
            .read()
            .await
            .get(session_key)
            .is_some_and(|m| *m == ReplyMedium::Voice)
    }

    async fn active_tool_invocations(&self, session_key: &str) -> Vec<ActiveToolInvocation> {
        self.active_tool_invocations
            .read()
            .await
            .get(session_key)
            .cloned()
            .unwrap_or_default()
    }

    async fn peek(&self, params: Value) -> ServiceResult {
        let session_key = params
            .get("sessionKey")
            .and_then(|v| v.as_str())
            .unwrap_or("main");

        let active = self
            .active_runs_by_session
            .read()
            .await
            .contains_key(session_key);

        if !active {
            return Ok(serde_json::json!({ "active": false }));
        }

        let tool_invocations: Vec<ActiveToolInvocation> = self
            .active_tool_invocations
            .read()
            .await
            .get(session_key)
            .cloned()
            .unwrap_or_default();

        Ok(serde_json::json!({
            "active": true,
            "sessionKey": session_key,
            "toolInvocations": tool_invocations,
        }))
    }
}

#[cfg(test)]
mod tests {
    use std::{
        collections::{HashMap, HashSet},
        pin::Pin,
        sync::{
            Arc, Mutex,
            atomic::{AtomicBool, Ordering},
        },
    };

    use {
        chelix_agents::model::{ChatMessage, CompletionOptions, LlmProvider, StreamEvent, Usage},
        chelix_common::{ModelMetadata, ModelModality, ModelOverride},
        chelix_config::ToolMode,
        chelix_providers::{ModelInfo, ProviderRegistry},
        chelix_service_traits::{
            ChatCompactRequest, ChatContextRequest, ChatExecutionContext, ChatFullContextRequest,
            ChatRawPromptRequest, ChatSendRequest, ChatSendSyncRequest, ChatService, McpService,
            NoopMcpService, NoopProjectService, NoopTtsService, ProjectService, ServiceError,
            TtsService,
        },
        chelix_sessions::{
            PersistedMessage, QueuedPrompts, SessionKey,
            metadata::{
                ExternalAgentKind, ExternalSessionIdentity, SessionBacking, SqliteSessionMetadata,
            },
            store::SessionStore,
        },
        serde_json::Value,
        tokio::sync::RwLock,
        tokio_stream::Stream,
        tokio_util::sync::CancellationToken,
    };

    use super::{
        ChatRunOutcome, LiveChatService, persist_send_sync_session_backing,
        resolve_send_sync_outcome, tool_mode_enables_tools,
    };

    struct ValidationTestRuntime {
        sandbox_router: Arc<chelix_tools::sandbox::SandboxRouter>,
        tts: NoopTtsService,
        project: NoopProjectService,
        mcp: NoopMcpService,
    }

    impl Default for ValidationTestRuntime {
        fn default() -> Self {
            Self {
                sandbox_router: Arc::new(chelix_tools::sandbox::SandboxRouter::disabled()),
                tts: NoopTtsService,
                project: NoopProjectService,
                mcp: NoopMcpService,
            }
        }
    }

    #[async_trait::async_trait]
    impl crate::runtime::ChatRuntime for ValidationTestRuntime {
        async fn broadcast(&self, _topic: &str, _payload: Value) {}

        async fn push_channel_reply(
            &self,
            _session_key: &str,
            _target: chelix_channels::ChannelReplyTarget,
        ) {
        }

        async fn drain_channel_replies(
            &self,
            _session_key: &str,
        ) -> Vec<chelix_channels::ChannelReplyTarget> {
            Vec::new()
        }

        async fn peek_channel_replies(
            &self,
            _session_key: &str,
        ) -> Vec<chelix_channels::ChannelReplyTarget> {
            Vec::new()
        }

        async fn push_channel_status_log(&self, _session_key: &str, _message: String) {}

        async fn drain_channel_status_log(&self, _session_key: &str) -> Vec<String> {
            Vec::new()
        }

        async fn set_run_error(&self, _run_id: &str, _error: String) {}

        async fn active_session_key(&self, _conn_id: &str) -> Option<String> {
            None
        }

        async fn active_project_id(&self, _conn_id: &str) -> Option<String> {
            None
        }

        fn hostname(&self) -> &str {
            "test"
        }

        fn sandbox_router(&self) -> &Arc<chelix_tools::sandbox::SandboxRouter> {
            &self.sandbox_router
        }

        fn memory_manager(&self) -> Option<&chelix_memory::runtime::DynMemoryRuntime> {
            None
        }

        async fn cached_location(&self) -> Option<chelix_config::GeoLocation> {
            None
        }

        async fn tts_overrides(
            &self,
            _session_key: &str,
            _channel_key: &str,
        ) -> (
            Option<crate::runtime::TtsOverride>,
            Option<crate::runtime::TtsOverride>,
        ) {
            (None, None)
        }

        fn channel_outbound(&self) -> Option<Arc<dyn chelix_channels::ChannelOutbound>> {
            None
        }

        fn channel_stream_outbound(
            &self,
        ) -> Option<Arc<dyn chelix_channels::ChannelStreamOutbound>> {
            None
        }

        fn tts_service(&self) -> &dyn TtsService {
            &self.tts
        }

        fn project_service(&self) -> &dyn ProjectService {
            &self.project
        }

        fn mcp_service(&self) -> &dyn McpService {
            &self.mcp
        }

        async fn chat_service(&self) -> Arc<dyn ChatService> {
            panic!("chat service is not used by validation tests")
        }

        async fn last_run_error(&self, _run_id: &str) -> Option<String> {
            None
        }

        async fn send_push_notification(
            &self,
            _title: &str,
            _body: &str,
            _url: Option<&str>,
            _session_key: Option<&str>,
        ) -> crate::error::Result<usize> {
            Ok(0)
        }
    }

    struct ValidationProvider {
        selected_effort: Option<chelix_common::ReasoningEffort>,
        resolved_efforts: Arc<Mutex<Vec<String>>>,
    }

    impl LlmProvider for ValidationProvider {
        fn name(&self) -> &str {
            "test"
        }

        fn id(&self) -> &str {
            "model"
        }

        fn stream_with_tools_and_options(
            &self,
            _messages: Vec<ChatMessage>,
            _tools: Vec<Value>,
            _options: CompletionOptions,
        ) -> Pin<Box<dyn Stream<Item = StreamEvent> + Send + '_>> {
            Box::pin(tokio_stream::iter(vec![
                StreamEvent::Delta("summary".to_string()),
                StreamEvent::Done(Usage::default()),
            ]))
        }

        fn stream(
            &self,
            messages: Vec<ChatMessage>,
        ) -> Pin<Box<dyn Stream<Item = StreamEvent> + Send + '_>> {
            self.stream_with_tools_and_options(messages, Vec::new(), CompletionOptions::default())
        }

        fn tool_mode(&self) -> ToolMode {
            ToolMode::Off
        }

        fn reasoning_effort(&self) -> Option<chelix_common::ReasoningEffort> {
            self.selected_effort.clone()
        }

        fn with_reasoning_effort(
            self: Arc<Self>,
            effort: chelix_common::ReasoningEffort,
        ) -> Option<Arc<dyn LlmProvider>> {
            self.resolved_efforts
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .push(effort.as_str().to_string());
            Some(Arc::new(Self {
                selected_effort: Some(effort),
                resolved_efforts: Arc::clone(&self.resolved_efforts),
            }))
        }
    }

    fn validation_model_metadata() -> ModelMetadata {
        ModelMetadata {
            context_length: 8_192,
            max_input_tokens: 4_096,
            max_output_tokens: 1_024,
            input_modalities: vec![ModelModality::Text],
            output_modalities: vec![ModelModality::Text],
            tool_calling: false,
            zero_data_retention_enabled: false,
            reasoning_supported_efforts: vec![chelix_common::ReasoningEffort::from("off")],
            reasoning_summary: None,
            reasoning_include: None,
        }
    }

    #[test]
    fn tool_mode_enables_tools_except_when_off() {
        assert!(tool_mode_enables_tools(ToolMode::Native));
        assert!(tool_mode_enables_tools(ToolMode::Text));
        assert!(!tool_mode_enables_tools(ToolMode::Off));
    }

    async fn validation_test_service() -> (
        tempfile::TempDir,
        LiveChatService,
        Arc<SqliteSessionMetadata>,
        Arc<SessionStore>,
        Arc<Mutex<Vec<String>>>,
    ) {
        let directory = tempfile::tempdir()
            .unwrap_or_else(|error| panic!("temporary session directory: {error}"));
        let pool = sqlx::SqlitePool::connect("sqlite::memory:")
            .await
            .unwrap_or_else(|error| panic!("test database connection: {error}"));
        sqlx::query("CREATE TABLE projects (id TEXT PRIMARY KEY)")
            .execute(&pool)
            .await
            .unwrap_or_else(|error| panic!("projects table setup: {error}"));
        chelix_sessions::run_migrations(&pool)
            .await
            .unwrap_or_else(|error| panic!("session migrations: {error}"));
        let metadata = Arc::new(SqliteSessionMetadata::new(pool.clone()));
        let session_store = Arc::new(SessionStore::new(directory.path().to_path_buf()));
        let original_pair = chelix_common::ResolvedModelReasoning::try_new(
            "test::model".to_string(),
            chelix_common::ReasoningEffort::from("off"),
        )
        .unwrap_or_else(|error| panic!("original test pair: {error}"));
        metadata
            .create_llm_session("main", Some("Main"), &original_pair, Some("main"))
            .await
            .unwrap_or_else(|error| panic!("original session setup: {error}"));

        let mut agents = chelix_config::AgentsConfig {
            default: "main".to_string(),
            ..chelix_config::AgentsConfig::default()
        };
        agents.entries.insert(
            "main".to_string(),
            chelix_config::AgentConfig::new(
                "Main",
                "test::model",
                chelix_common::ReasoningEffort::from("off"),
            ),
        );
        agents.entries.insert(
            "other".to_string(),
            chelix_config::AgentConfig::new(
                "Other",
                "test::model",
                chelix_common::ReasoningEffort::from("off"),
            ),
        );
        let config = chelix_config::ChelixConfig {
            agents: agents.clone(),
            ..chelix_config::ChelixConfig::default()
        };
        let agents_config = Arc::new(RwLock::new(agents));
        let resolved_efforts = Arc::new(Mutex::new(Vec::new()));
        let mut registry = ProviderRegistry::empty();
        for model_id in ["model", "other"] {
            registry.register(
                ModelInfo {
                    id: model_id.to_string(),
                    provider: "test".to_string(),
                    metadata: validation_model_metadata(),
                },
                Arc::new(ValidationProvider {
                    selected_effort: None,
                    resolved_efforts: Arc::clone(&resolved_efforts),
                }),
            );
        }
        let runtime: Arc<dyn crate::runtime::ChatRuntime> =
            Arc::new(ValidationTestRuntime::default());
        let service = LiveChatService::new(
            Arc::new(RwLock::new(registry)),
            runtime,
            Arc::clone(&session_store),
            Arc::clone(&metadata),
            Arc::new(QueuedPrompts::new(pool)),
            config.clone(),
            agents_config,
            chelix_config::ToolsConfigSource::snapshot(config.tools),
        );
        (
            directory,
            service,
            metadata,
            session_store,
            resolved_efforts,
        )
    }

    #[tokio::test]
    async fn chat_turn_resolution_uses_complete_request_or_persisted_session_pair() {
        let (_directory, service, _metadata, _session_store, _resolved_efforts) =
            validation_test_service().await;
        let explicit = ModelOverride {
            model: "test::other".to_string(),
            reasoning_effort: chelix_common::ReasoningEffort::from("off"),
        };

        let request_resolved = service
            .resolve_chat_turn(&SessionKey::new("main"), Some(&explicit))
            .await
            .unwrap_or_else(|error| panic!("request pair should resolve: {error}"));
        assert_eq!(
            request_resolved.model.model_reasoning().model_id(),
            "test::other"
        );
        assert_eq!(
            request_resolved
                .model
                .model_reasoning()
                .reasoning_effort()
                .as_str(),
            "off"
        );
        assert!(request_resolved.stream_only);

        let session_resolved = service
            .resolve_chat_turn(&SessionKey::new("main"), None)
            .await
            .unwrap_or_else(|error| panic!("persisted pair should resolve: {error}"));
        assert_eq!(
            session_resolved.model.model_reasoning().model_id(),
            "test::model"
        );
        assert_eq!(
            session_resolved
                .model
                .model_reasoning()
                .reasoning_effort()
                .as_str(),
            "off"
        );
        assert!(session_resolved.stream_only);
    }

    #[tokio::test]
    async fn chat_auxiliary_surfaces_use_the_persisted_pair_and_one_resolved_provider() {
        let (_directory, service, _metadata, session_store, resolved_efforts) =
            validation_test_service().await;
        session_store
            .append("main", &PersistedMessage::user("hello").to_value())
            .await
            .unwrap_or_else(|error| panic!("seed auxiliary history: {error}"));

        let context_payload = service
            .context(
                ChatContextRequest::default(),
                ChatExecutionContext::internal(SessionKey::new("main")),
            )
            .await
            .unwrap_or_else(|error| panic!("chat.context should succeed: {error}"));
        assert_eq!(context_payload["session"]["model"], "test::model");
        assert_eq!(context_payload["session"]["provider"], "test");
        assert_eq!(context_payload["supportsTools"], false);
        assert_eq!(context_payload["tokenUsage"]["contextWindow"], 8_192);

        let raw_prompt = service
            .raw_prompt(
                ChatRawPromptRequest::default(),
                ChatExecutionContext::internal(SessionKey::new("main")),
            )
            .await
            .unwrap_or_else(|error| panic!("chat.raw_prompt should succeed: {error}"));
        assert_eq!(raw_prompt["tools_enabled"], false);
        assert_eq!(raw_prompt["tool_mode"], "Off");

        let full_context = service
            .full_context(
                ChatFullContextRequest::default(),
                ChatExecutionContext::internal(SessionKey::new("main")),
            )
            .await
            .unwrap_or_else(|error| panic!("chat.full_context should succeed: {error}"));
        assert!(full_context["messages"].is_array());

        service
            .compact(
                ChatCompactRequest::default(),
                ChatExecutionContext::internal(SessionKey::new("main")),
            )
            .await
            .unwrap_or_else(|error| panic!("chat.compact should succeed: {error}"));

        let efforts = resolved_efforts
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone();
        assert_eq!(efforts, vec!["off", "off", "off", "off"]);
    }

    #[tokio::test]
    async fn chat_auxiliary_surfaces_reject_invalid_persisted_pairs() {
        let (_directory, service, metadata, _session_store, resolved_efforts) =
            validation_test_service().await;
        metadata
            .bind_external(
                "external",
                None,
                &ExternalSessionIdentity::new(ExternalAgentKind::Codex, None),
            )
            .await
            .unwrap_or_else(|error| panic!("external session setup: {error}"));
        for (session_key, model, effort) in [
            ("unknown", "test::missing", "off"),
            ("unsupported", "test::model", "high"),
        ] {
            let pair = chelix_common::ResolvedModelReasoning::try_new(
                model.to_string(),
                chelix_common::ReasoningEffort::from(effort),
            )
            .unwrap_or_else(|error| {
                panic!("invalid registry pair must remain storage-valid: {error}")
            });
            metadata
                .create_llm_session(session_key, None, &pair, Some("main"))
                .await
                .unwrap_or_else(|error| panic!("invalid registry session setup: {error}"));
        }

        for session_key in ["external", "unknown", "unsupported"] {
            let results = [
                service
                    .compact(
                        ChatCompactRequest::default(),
                        ChatExecutionContext::internal(SessionKey::new(session_key)),
                    )
                    .await,
                service
                    .context(
                        ChatContextRequest::default(),
                        ChatExecutionContext::internal(SessionKey::new(session_key)),
                    )
                    .await,
                service
                    .raw_prompt(
                        ChatRawPromptRequest::default(),
                        ChatExecutionContext::internal(SessionKey::new(session_key)),
                    )
                    .await,
                service
                    .full_context(
                        ChatFullContextRequest::default(),
                        ChatExecutionContext::internal(SessionKey::new(session_key)),
                    )
                    .await,
            ];
            for result in results {
                assert!(
                    result.is_err(),
                    "auxiliary surface unexpectedly accepted session {session_key}"
                );
            }
        }
        assert!(
            resolved_efforts
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .is_empty(),
            "invalid pairs must fail before applying reasoning to a provider"
        );
    }

    #[tokio::test]
    async fn chat_turn_resolution_rejects_invalid_complete_pairs_or_missing_session_pair() {
        let (_directory, service, metadata, _session_store, _resolved_efforts) =
            validation_test_service().await;
        metadata
            .bind_external(
                "external",
                None,
                &ExternalSessionIdentity::new(ExternalAgentKind::Codex, None),
            )
            .await
            .unwrap_or_else(|error| panic!("external session setup: {error}"));
        let cases = [
            (
                "empty model",
                SessionKey::new("main"),
                Some(ModelOverride {
                    model: String::new(),
                    reasoning_effort: chelix_common::ReasoningEffort::from("off"),
                }),
            ),
            (
                "empty effort",
                SessionKey::new("main"),
                Some(ModelOverride {
                    model: "test::model".to_string(),
                    reasoning_effort: chelix_common::ReasoningEffort::from(""),
                }),
            ),
            (
                "unknown model",
                SessionKey::new("main"),
                Some(ModelOverride {
                    model: "test::missing".to_string(),
                    reasoning_effort: chelix_common::ReasoningEffort::from("off"),
                }),
            ),
            (
                "noncanonical model",
                SessionKey::new("main"),
                Some(ModelOverride {
                    model: "model".to_string(),
                    reasoning_effort: chelix_common::ReasoningEffort::from("off"),
                }),
            ),
            (
                "unsupported effort",
                SessionKey::new("main"),
                Some(ModelOverride {
                    model: "test::model".to_string(),
                    reasoning_effort: chelix_common::ReasoningEffort::from("high"),
                }),
            ),
            ("missing persisted pair", SessionKey::new("external"), None),
        ];

        for (name, session_id, model_override) in cases {
            let result = service
                .resolve_chat_turn(&session_id, model_override.as_ref())
                .await;
            assert!(result.is_err(), "{name} unexpectedly resolved");
        }
    }

    #[tokio::test]
    async fn rejected_send_preserves_entry_and_history() {
        let (_directory, service, metadata, session_store, _resolved_efforts) =
            validation_test_service().await;
        let cases = [
            ("empty effort", ModelOverride {
                model: "test::model".to_string(),
                reasoning_effort: chelix_common::ReasoningEffort::from(""),
            }),
            ("unknown explicit model", ModelOverride {
                model: "test::missing".to_string(),
                reasoning_effort: chelix_common::ReasoningEffort::from("off"),
            }),
            ("unsupported effort", ModelOverride {
                model: "test::model".to_string(),
                reasoning_effort: chelix_common::ReasoningEffort::from("high"),
            }),
        ];

        for (name, model_override) in cases {
            let before = metadata
                .get("main")
                .await
                .unwrap_or_else(|error| panic!("{name}: load entry before: {error}"))
                .unwrap_or_else(|| panic!("{name}: entry exists before"));
            let history_before = session_store
                .read("main")
                .await
                .unwrap_or_else(|error| panic!("{name}: read history before: {error}"));

            let mut request = ChatSendRequest::text("hello");
            request.model_override = Some(model_override);
            let result = service
                .send(
                    request,
                    ChatExecutionContext::internal(SessionKey::new("main")),
                )
                .await;
            assert!(result.is_err(), "{name} unexpectedly succeeded");

            let after = metadata
                .get("main")
                .await
                .unwrap_or_else(|error| panic!("{name}: load entry after: {error}"))
                .unwrap_or_else(|| panic!("{name}: entry exists after"));
            let history_after = session_store
                .read("main")
                .await
                .unwrap_or_else(|error| panic!("{name}: read history after: {error}"));
            assert_eq!(after.agent_id, before.agent_id, "{name}: agent changed");
            assert_eq!(after.backing, before.backing, "{name}: backing changed");
            assert_eq!(after.version, before.version, "{name}: version changed");
            assert_eq!(
                after.updated_at, before.updated_at,
                "{name}: timestamp changed"
            );
            assert_eq!(history_after, history_before, "{name}: history changed");
        }
    }

    #[tokio::test]
    async fn rejected_send_sync_agent_selection_preserves_entry_and_history() {
        let (_directory, service, metadata, session_store, _resolved_efforts) =
            validation_test_service().await;
        let cases = [
            ("empty effort", ModelOverride {
                model: "test::model".to_string(),
                reasoning_effort: chelix_common::ReasoningEffort::from(""),
            }),
            ("unknown explicit model", ModelOverride {
                model: "test::missing".to_string(),
                reasoning_effort: chelix_common::ReasoningEffort::from("off"),
            }),
            ("unsupported effort", ModelOverride {
                model: "test::model".to_string(),
                reasoning_effort: chelix_common::ReasoningEffort::from("high"),
            }),
        ];

        for (name, model_override) in cases {
            let before = metadata
                .get("main")
                .await
                .unwrap_or_else(|error| panic!("{name}: load entry before: {error}"))
                .unwrap_or_else(|| panic!("{name}: entry exists before"));
            let history_before = session_store
                .read("main")
                .await
                .unwrap_or_else(|error| panic!("{name}: read history before: {error}"));

            let request = ChatSendSyncRequest {
                text: "hello".to_string(),
                model_override: Some(model_override),
                tool_choice: None,
                input_medium: None,
            };
            let mut context = ChatExecutionContext::internal(SessionKey::new("main"));
            context.agent_id = Some("other".to_string());
            let result = service.send_sync(request, context).await;
            assert!(result.is_err(), "{name} unexpectedly succeeded");

            let after = metadata
                .get("main")
                .await
                .unwrap_or_else(|error| panic!("{name}: load entry after: {error}"))
                .unwrap_or_else(|| panic!("{name}: entry exists after"));
            let history_after = session_store
                .read("main")
                .await
                .unwrap_or_else(|error| panic!("{name}: read history after: {error}"));
            assert_eq!(after.agent_id, before.agent_id, "{name}: agent changed");
            assert_eq!(after.backing, before.backing, "{name}: backing changed");
            assert_eq!(after.version, before.version, "{name}: version changed");
            assert_eq!(
                after.updated_at, before.updated_at,
                "{name}: timestamp changed"
            );
            assert_eq!(history_after, history_before, "{name}: history changed");
        }
    }

    #[tokio::test]
    async fn busy_send_persists_turn_settings_before_prompt_only_enqueue() {
        let (_directory, service, metadata, _session_store, _resolved_efforts) =
            validation_test_service().await;
        let persisted_pair = chelix_common::ResolvedModelReasoning::try_new(
            "test::model".to_string(),
            chelix_common::ReasoningEffort::from("off"),
        )
        .unwrap_or_else(|error| panic!("persisted test pair: {error}"));
        metadata
            .create_llm_session("agentless", None, &persisted_pair, None)
            .await
            .unwrap_or_else(|error| panic!("agentless session setup: {error}"));

        let agentless_permit = service
            .session_mutations
            .try_acquire_turn("agentless")
            .await
            .unwrap_or_else(|error| panic!("agentless active turn setup: {error}"));
        let mut agent_context = ChatExecutionContext::internal(SessionKey::new("agentless"));
        agent_context.agent_id = Some("other".to_string());
        let agentless_result = service
            .send(ChatSendRequest::text("queued for agent"), agent_context)
            .await
            .unwrap_or_else(|error| panic!("agentless queued send: {error}"));
        assert_eq!(agentless_result["queued"], true);
        let agentless_entry = metadata
            .get("agentless")
            .await
            .unwrap_or_else(|error| panic!("agentless entry load: {error}"))
            .unwrap_or_else(|| panic!("agentless entry should exist"));
        assert_eq!(agentless_entry.agent_id.as_deref(), Some("other"));
        assert_eq!(agentless_entry.model_reasoning(), Some(&persisted_pair));
        let agentless_status = service
            .queued_prompts
            .status(SessionKey::new("agentless"))
            .await
            .unwrap_or_else(|error| panic!("agentless queue status: {error}"));
        assert_eq!(agentless_status.prompts.len(), 1);
        drop(agentless_permit);

        let existing_permit = service
            .session_mutations
            .try_acquire_turn("main")
            .await
            .unwrap_or_else(|error| panic!("existing active turn setup: {error}"));
        let mut override_request = ChatSendRequest::text("queued with override");
        override_request.model_override = Some(ModelOverride {
            model: "test::other".to_string(),
            reasoning_effort: chelix_common::ReasoningEffort::from("off"),
        });
        let mut existing_context = ChatExecutionContext::internal(SessionKey::new("main"));
        existing_context.agent_id = Some("other".to_string());
        let override_result = service
            .send(override_request, existing_context)
            .await
            .unwrap_or_else(|error| panic!("override queued send: {error}"));
        assert_eq!(override_result["queued"], true);
        let existing_entry = metadata
            .get("main")
            .await
            .unwrap_or_else(|error| panic!("existing entry load: {error}"))
            .unwrap_or_else(|| panic!("existing entry should exist"));
        assert_eq!(existing_entry.agent_id.as_deref(), Some("main"));
        assert_eq!(existing_entry.model(), Some("test::other"));
        assert_eq!(
            existing_entry
                .reasoning_effort()
                .map(chelix_common::ReasoningEffort::as_str),
            Some("off")
        );
        let existing_status = service
            .queued_prompts
            .status(SessionKey::new("main"))
            .await
            .unwrap_or_else(|error| panic!("existing queue status: {error}"));
        assert_eq!(existing_status.prompts.len(), 1);
        drop(existing_permit);
    }

    #[tokio::test]
    async fn send_sync_persistence_promotes_external_only_with_identity_and_agent() {
        const KEY: &str = "session:external-send-sync";
        let pool = sqlx::SqlitePool::connect("sqlite::memory:")
            .await
            .unwrap_or_else(|error| panic!("test database connection: {error}"));
        sqlx::query("CREATE TABLE projects (id TEXT PRIMARY KEY)")
            .execute(&pool)
            .await
            .unwrap_or_else(|error| panic!("projects table setup: {error}"));
        SqliteSessionMetadata::init(&pool)
            .await
            .unwrap_or_else(|error| panic!("session metadata setup: {error}"));
        let metadata = SqliteSessionMetadata::new(pool);
        metadata
            .bind_external(
                KEY,
                None,
                &ExternalSessionIdentity::new(
                    ExternalAgentKind::Codex,
                    Some("external-1".to_string()),
                ),
            )
            .await
            .unwrap_or_else(|error| panic!("external session setup: {error}"));
        let existing = metadata
            .get(KEY)
            .await
            .unwrap_or_else(|error| panic!("external session load: {error}"));
        let model_reasoning = chelix_common::ResolvedModelReasoning::try_new(
            "test::model".to_string(),
            chelix_common::ReasoningEffort::from("high"),
        )
        .unwrap_or_else(|error| panic!("test model/reasoning pair: {error}"));

        let entry =
            persist_send_sync_session_backing(&metadata, existing, KEY, &model_reasoning, "main")
                .await
                .unwrap_or_else(|error| panic!("send_sync backing persistence: {error}"));

        assert!(matches!(entry.backing, SessionBacking::LlmExternal { .. }));
        assert_eq!(entry.model(), Some("test::model"));
        assert_eq!(
            entry
                .reasoning_effort()
                .map(chelix_common::ReasoningEffort::as_str),
            Some("high")
        );
        assert_eq!(entry.external_agent_kind(), Some(ExternalAgentKind::Codex));
        assert_eq!(entry.external_session_id(), Some("external-1"));
        assert_eq!(entry.agent_id.as_deref(), Some("main"));
    }

    #[tokio::test]
    async fn send_sync_failure_returns_the_provider_error() {
        let result = resolve_send_sync_outcome(ChatRunOutcome::Failed, || async {
            Err(ServiceError::message("provider request failed"))
        })
        .await;

        match result {
            Err(error) => assert_eq!(error.to_string(), "provider request failed"),
            Ok(value) => panic!("provider failure unexpectedly succeeded: {value}"),
        }
    }

    #[tokio::test]
    async fn send_sync_cancellation_skips_failure_persistence() {
        let failure_called = Arc::new(AtomicBool::new(false));
        let failure_called_by_callback = Arc::clone(&failure_called);

        let result = resolve_send_sync_outcome(ChatRunOutcome::Cancelled, move || async move {
            failure_called_by_callback.store(true, Ordering::SeqCst);
            Err("failure callback must not run".into())
        })
        .await;

        match result {
            Err(error) => assert_eq!(error.to_string(), "agent run cancelled"),
            Ok(value) => panic!("cancellation unexpectedly succeeded: {value}"),
        }
        assert!(!failure_called.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn cancel_run_signals_registered_token_without_removing_run_state() {
        let active_runs = Arc::new(RwLock::new(HashMap::new()));
        let active_runs_by_session = Arc::new(RwLock::new(HashMap::new()));
        let terminal_runs = Arc::new(RwLock::new(HashSet::new()));
        let cancellation_token = CancellationToken::new();
        active_runs
            .write()
            .await
            .insert("run-1".to_owned(), cancellation_token.clone());
        active_runs_by_session
            .write()
            .await
            .insert("session-1".to_owned(), "run-1".to_owned());

        let (resolved_run_id, cancelled) = LiveChatService::cancel_run(
            &active_runs,
            &active_runs_by_session,
            &terminal_runs,
            None,
            Some("session-1"),
        )
        .await;

        assert_eq!(resolved_run_id.as_deref(), Some("run-1"));
        assert!(cancelled);
        assert!(cancellation_token.is_cancelled());
        assert!(active_runs.read().await.contains_key("run-1"));
        assert_eq!(
            active_runs_by_session
                .read()
                .await
                .get("session-1")
                .map(String::as_str),
            Some("run-1")
        );

        let (_, cancelled_again) = LiveChatService::cancel_run(
            &active_runs,
            &active_runs_by_session,
            &terminal_runs,
            Some("run-1"),
            None,
        )
        .await;
        assert!(!cancelled_again);
    }

    #[tokio::test]
    async fn cancel_run_does_not_signal_a_terminal_run() {
        let active_runs = Arc::new(RwLock::new(HashMap::new()));
        let active_runs_by_session = Arc::new(RwLock::new(HashMap::new()));
        let terminal_runs = Arc::new(RwLock::new(HashSet::from(["run-1".to_owned()])));
        let cancellation_token = CancellationToken::new();
        active_runs
            .write()
            .await
            .insert("run-1".to_owned(), cancellation_token.clone());
        active_runs_by_session
            .write()
            .await
            .insert("session-1".to_owned(), "run-1".to_owned());

        let (resolved_run_id, cancelled) = LiveChatService::cancel_run(
            &active_runs,
            &active_runs_by_session,
            &terminal_runs,
            None,
            Some("session-1"),
        )
        .await;

        assert_eq!(resolved_run_id.as_deref(), Some("run-1"));
        assert!(!cancelled);
        assert!(!cancellation_token.is_cancelled());
    }
}
