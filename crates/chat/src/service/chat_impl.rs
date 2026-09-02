//! `ChatService` trait implementation for `LiveChatService`.

mod send;

use std::{
    collections::{HashMap, HashSet},
    future::Future,
    path::Path,
    sync::Arc,
};

use {
    async_trait::async_trait,
    serde_json::Value,
    tokio::sync::RwLock,
    tokio_util::sync::CancellationToken,
    tracing::{info, warn},
};

use {
    chelix_agents::{
        ChatMessage, UserContent,
        model::{ReasoningEffort, values_to_chat_messages},
        prompt::{
            build_system_prompt_minimal_runtime_details,
            build_system_prompt_with_session_runtime_details,
        },
    },
    chelix_config::ToolMode,
    chelix_service_traits::{ChatService, ServiceError, ServiceResult},
    chelix_sessions::{MessageContent, PersistedMessage, filter_ui_history},
    chelix_tools::policy::{PolicyContext, ToolPolicy},
};

use crate::{
    channels::notify_channels_of_compaction,
    compaction,
    message::{
        infer_reply_medium, user_audio_path_from_params, user_documents_for_persistence,
        user_documents_from_params,
    },
    prompt::{
        apply_request_runtime_context, build_policy_context, build_prompt_runtime_context,
        clear_prompt_memory_snapshot, discover_skills_if_enabled, filter_skills_for_agent,
        load_prompt_persona_for_agent, load_prompt_persona_for_session, prepare_run_registry,
        prompt_build_limits_from_config, resolve_prompt_agent_id,
    },
    run_with_tools::run_with_tools,
    streaming::run_streaming,
    types::*,
};

use super::*;

pub(super) fn resolved_turn_reasoning_effort(
    session_entry: Option<&chelix_sessions::metadata::SessionEntry>,
    agent: &chelix_config::AgentConfig,
) -> Option<String> {
    session_entry
        .and_then(|entry| entry.reasoning_effort())
        .map(|effort| effort.as_str().to_string())
        .or_else(|| Some(agent.reasoning_effort.as_str().to_owned()))
}

pub(super) fn requested_reasoning_effort(params: &Value) -> Option<String> {
    params
        .get("reasoningEffort")
        .or_else(|| params.get("reasoning_effort"))
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

pub(super) fn apply_reasoning_effort_to_provider(
    provider: Arc<dyn chelix_agents::model::LlmProvider>,
    reasoning_effort: Option<&str>,
) -> Result<Arc<dyn chelix_agents::model::LlmProvider>, String> {
    let Some(reasoning_effort) = reasoning_effort else {
        return Ok(provider);
    };
    Arc::clone(&provider)
        .with_reasoning_effort(ReasoningEffort::from(reasoning_effort))
        .ok_or_else(|| {
            format!(
                "model '{}' does not support reasoning_effort '{reasoning_effort}'",
                provider.id(),
            )
        })
}

fn send_sync_model_id<'a>(
    explicit_model: Option<&'a str>,
    requested_agent_pair: Option<&'a chelix_common::ResolvedModelReasoning>,
    session_entry: Option<&'a chelix_sessions::metadata::SessionEntry>,
) -> Option<&'a str> {
    explicit_model
        .or_else(|| requested_agent_pair.map(chelix_common::ResolvedModelReasoning::model_id))
        .or_else(|| session_entry.and_then(|entry| entry.model()))
}

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
    async fn send(&self, params: Value) -> ServiceResult {
        self.send_impl(params).await
    }

    async fn send_sync(&self, params: Value) -> ServiceResult {
        let text = params
            .get("text")
            .and_then(|v| v.as_str())
            .ok_or_else(|| "missing 'text' parameter".to_string())?
            .to_string();
        let desired_reply_medium = infer_reply_medium(&params, &text);
        let requested_agent_id = params
            .get("agent_id")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string);
        let request_tool_policy = params
            .get("_tool_policy")
            .cloned()
            .map(serde_json::from_value::<ToolPolicy>)
            .transpose()
            .map_err(|e| format!("invalid '_tool_policy' parameter: {e}"))?;
        let explicit_model = params.get("model").and_then(|v| v.as_str());
        let requested_reasoning_effort_override = requested_reasoning_effort(&params);
        let tool_choice = chelix_config::schema::tool_choice_from_request_params(&params)
            .map_err(|error| format!("invalid 'tool_choice' parameter: {error}"))?;
        let stream_only = !self.has_tools_sync();

        // Resolve session key from explicit override.
        let session_key = match params.get("_session_key").and_then(|v| v.as_str()) {
            Some(sk) => sk.to_string(),
            None => "main".to_string(),
        };

        let user_audio = user_audio_path_from_params(&params, &session_key);
        let user_documents =
            user_documents_from_params(&params, &session_key, self.session_store.as_ref());
        // Persist the user message.
        let user_msg = PersistedMessage::User {
            content: MessageContent::Text(text.clone()),
            created_at: Some(now_ms()),
            audio: user_audio,
            documents: user_documents
                .as_deref()
                .and_then(user_documents_for_persistence),
            channel: None,
            seq: None,
            run_id: None,
        };
        let mut session_entry = self
            .session_metadata
            .get(&session_key)
            .await
            .map_err(ServiceError::message)?;
        let runtime_config = self
            .load_runtime_config_for_agent_run()
            .await
            .map_err(ServiceError::message)?;
        let requested_agent_pair = if let Some(agent_id) = requested_agent_id.as_deref() {
            let agent = runtime_config.agents.get(agent_id).ok_or_else(|| {
                ServiceError::message(format!("agent '{agent_id}' is not configured"))
            })?;
            let registry = self.providers.read().await;
            Some(
                registry
                    .resolve_model_reasoning(Some(&agent.model), Some(&agent.reasoning_effort))
                    .map_err(|error| ServiceError::message(error.to_string()))?
                    .model_reasoning()
                    .clone(),
            )
        } else {
            None
        };
        let model_id = send_sync_model_id(
            explicit_model,
            requested_agent_pair.as_ref(),
            session_entry.as_ref(),
        )
            .ok_or_else(|| {
                format!(
                    "session '{session_key}' has no model; pass 'model' explicitly or set the session model"
                )
            })?;
        let provider: Arc<dyn chelix_agents::model::LlmProvider> = {
            let registry = self.providers.read().await;
            registry
                .get(model_id)
                .ok_or_else(|| format!("model '{model_id}' not found"))?
        };
        if !stream_only {
            validate_tool_mode_compatibility(
                provider.tool_mode(),
                provider.supports_tools(),
                provider.id(),
            )
            .map_err(ServiceError::message)?;
        }
        let prompt_profile = session_entry
            .as_ref()
            .map(|entry| entry.prompt_profile)
            .unwrap_or_default();
        let persona = if let Some(agent_id) = requested_agent_id.as_deref() {
            load_prompt_persona_for_agent(
                &runtime_config,
                &session_key,
                agent_id,
                prompt_profile,
                self.session_state_store.as_deref(),
            )
            .await
        } else {
            load_prompt_persona_for_session(
                &runtime_config,
                &session_key,
                session_entry.as_ref(),
                self.session_state_store.as_deref(),
            )
            .await
        }
        .map_err(ServiceError::message)?;
        let session_agent_id = persona.agent_id.clone();
        let resolved_reasoning_effort = requested_reasoning_effort_override
            .or_else(|| {
                requested_agent_pair
                    .as_ref()
                    .map(chelix_common::ResolvedModelReasoning::reasoning_effort)
                    .map(ReasoningEffort::as_str)
                    .map(str::to_string)
            })
            .or_else(|| resolved_turn_reasoning_effort(session_entry.as_ref(), &persona.agent));
        let provider =
            apply_reasoning_effort_to_provider(provider, resolved_reasoning_effort.as_deref())?;
        let reasoning_effort = resolved_reasoning_effort.as_deref().ok_or_else(|| {
            ServiceError::message(format!("session '{session_key}' has no reasoning effort"))
        })?;
        let model_reasoning = chelix_common::ResolvedModelReasoning::try_new(
            provider.id().to_string(),
            ReasoningEffort::from(reasoning_effort),
        )
        .map_err(|error| ServiceError::message(error.to_string()))?;
        let runtime_limits = persona
            .config
            .agent_runtime_limits(&session_agent_id)
            .map_err(ServiceError::message)?;

        if let (Some(agent_id), Some(agent_pair)) =
            (requested_agent_id.as_deref(), requested_agent_pair.as_ref())
        {
            session_entry = Some(
                self.session_metadata
                    .create_or_assign_agent(&session_key, agent_id, agent_pair)
                    .await
                    .map_err(ServiceError::message)?,
            );
        }
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
        apply_request_runtime_context(
            &mut runtime_context.host,
            &params,
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
        let tool_registry = if let Some(policy) = request_tool_policy.as_ref() {
            let registry_guard = self.tool_registry.read().await;
            Arc::new(RwLock::new(
                registry_guard.clone_allowed_by(|name| policy.is_allowed(name)),
            ))
        } else {
            Arc::clone(&self.tool_registry)
        };
        let hook_registry = self.hook_registry.clone();
        let provider_name = provider.name().to_string();
        let model_id = provider.id().to_string();
        let user_message_index = history.len();

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

        broadcast(
            &self.state,
            "chat",
            serde_json::json!({
                "state": "user_message",
                "text": text,
                "sessionKey": session_key,
                "messageIndex": user_message_index,
            }),
            BroadcastOpts::default(),
        )
        .await;

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
                None, // send_sync: no sender name
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
                None,
                None, // send_sync: no conn_id
                Some(&self.session_store),
                false, // send_sync: MCP tools always enabled for API calls
                None,  // send_sync: no client seq
                Some(Arc::clone(&self.active_tool_invocations)),
                Some(Arc::clone(&self.active_partial_assistant)),
                &active_event_forwarders,
                &terminal_runs,
                None, // send_sync: no sender name
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

            // Persist the error in the session so it's visible in session history.
            let error_entry = PersistedMessage::system(format!("[error] {error_msg}"));
            let _ = self
                .session_store
                .append(&session_key, &error_entry.to_value())
                .await;
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

    async fn prompt_queue_list(&self, params: Value) -> ServiceResult {
        let session_key = self.resolve_session_key_from_params(&params).await;
        let prompts = self
            .prompt_queue
            .list(&session_key)
            .await
            .map_err(|error| ServiceError::message(error.to_string()))?;
        Ok(serde_json::json!({
            "sessionKey": session_key,
            "prompts": prompts,
        }))
    }

    async fn prompt_queue_cancel(&self, params: Value) -> ServiceResult {
        let session_key = self.resolve_session_key_from_params(&params).await;
        let prompt_id = params
            .get("promptId")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|value| !value.is_empty());

        let prompts = match prompt_id {
            Some(prompt_id) => self.prompt_queue.cancel_one(&session_key, prompt_id).await,
            None => self
                .prompt_queue
                .cancel_all(&session_key)
                .await
                .map(|_| Vec::new()),
        }
        .map_err(|error| ServiceError::message(error.to_string()))?;

        Ok(serde_json::json!({
            "sessionKey": session_key,
            "prompts": prompts,
        }))
    }

    async fn history(&self, params: Value) -> ServiceResult {
        let session_key = self.resolve_session_key_from_params(&params).await;
        let messages = self
            .session_store
            .read(&session_key)
            .await
            .map_err(ServiceError::message)?;
        let history = filter_ui_history(messages).map_err(ServiceError::message)?;
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

        // Prompts queued for the cleared conversation must not resurface.
        self.prompt_queue
            .cancel_all(&session_key)
            .await
            .map_err(|error| ServiceError::message(error.to_string()))?;

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

    async fn compact(&self, params: Value) -> ServiceResult {
        let session_key = self.resolve_session_key_from_params(&params).await;

        let history = self
            .session_store
            .read(&session_key)
            .await
            .map_err(ServiceError::message)?;

        if history.is_empty() {
            return Err("nothing to compact".into());
        }

        // Summarize with the session's own model and append a checkpoint.
        // The stored history is never mutated.
        let provider = self
            .resolve_provider(&session_key, &history)
            .await
            .map_err(ServiceError::message)?;

        // Rebuild the session system prompt and tool schemas exactly as a
        // regular turn would, so the summarization request shares the
        // provider prompt-cache prefix with the previous turn.
        let (system_prompt, tools) = self
            .session_prompt_context(&session_key, &history, &provider, &params)
            .await
            .map_err(ServiceError::message)?;

        let outcome = compaction::summarize_session(
            &self.session_store,
            &session_key,
            &*provider,
            &system_prompt,
            &tools,
        )
        .await
        .map_err(|e| ServiceError::message(e.to_string()))?;

        let message_count = self
            .session_store
            .ui_message_count(&session_key)
            .await
            .unwrap_or(0);
        self.session_metadata
            .touch(&session_key, message_count)
            .await
            .map_err(ServiceError::message)?;

        // Broadcast the checkpoint so all connected clients render the
        // persistent checkpoint card without a reload.
        let mut compact_payload = serde_json::json!({
            "sessionKey": session_key,
            "state": "compact",
            "phase": "done",
        });
        if let (Some(obj), Some(meta)) = (
            compact_payload.as_object_mut(),
            outcome.broadcast_metadata().as_object().cloned(),
        ) {
            obj.extend(meta);
        }
        broadcast(
            &self.state,
            "chat",
            compact_payload,
            BroadcastOpts::default(),
        )
        .await;

        // Notify any channel (Telegram, Discord, Matrix, WhatsApp, etc.)
        // that has pending reply targets on this session.
        notify_channels_of_compaction(&self.state, &session_key, &outcome).await;

        info!(session = %session_key, "chat.compact: done");
        Ok(outcome.message)
    }

    async fn context(&self, params: Value) -> ServiceResult {
        let session_key = self.resolve_session_key_from_params(&params).await;

        // Session info
        let message_count = self
            .session_store
            .ui_message_count(&session_key)
            .await
            .unwrap_or(0);
        let session_entry = self
            .session_metadata
            .get(&session_key)
            .await
            .map_err(ServiceError::message)?;
        let prompt_persona = self
            .load_prompt_persona_for_agent_run(&session_key, session_entry.as_ref())
            .await
            .map_err(ServiceError::message)?;
        let messages = self
            .session_store
            .read(&session_key)
            .await
            .unwrap_or_default();
        let provider = self
            .resolve_provider(&session_key, &messages)
            .await
            .map_err(ServiceError::message)?;
        let provider_name = provider.name().to_string();
        let tools_enabled = tool_mode_enables_tools(provider.tool_mode());
        let session_info = serde_json::json!({
            "key": session_key,
            "messageCount": message_count,
            "model": session_entry.as_ref().and_then(|entry| entry.model()),
            "provider": provider_name,
            "label": session_entry.as_ref().and_then(|e| e.label.as_deref()),
            "projectId": session_entry.as_ref().and_then(|e| e.project_id.as_deref()),
        });

        // Project info & context files
        let conn_id = params
            .get("_conn_id")
            .and_then(|v| v.as_str())
            .map(String::from);
        let project_id = if let Some(cid) = conn_id.as_deref() {
            self.state.active_project_id(cid).await
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
            let memory_setup = self
                .state
                .memory_manager()
                .map(|manager| (manager, Arc::clone(&provider)));
            match prepare_run_registry(
                &registry_guard,
                &prompt_persona.config,
                &[],
                mcp_disabled,
                &list_ctx,
                true,
                &list_agent_id,
                memory_setup,
                &messages,
            ) {
                Ok(effective_registry) => {
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
                },
                Err(error) => {
                    warn!(session = %session_key, error = %error, "context: failed to prepare tool registry");
                    (vec![], 0)
                },
            }
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

        // Context window from the session's provider
        let context_window = {
            let reg = self.providers.read().await;
            let session_model = session_entry.as_ref().and_then(|entry| entry.model());
            let provider = if let Some(id) = session_model {
                reg.get(id)
                    .ok_or_else(|| format!("model '{id}' is not registered"))?
            } else {
                reg.first()
                    .ok_or_else(|| "no model is registered for this session".to_string())?
            };
            provider.context_window().ok_or_else(|| {
                format!(
                    "model '{}' has no resolved context_length metadata",
                    provider.id()
                )
            })?
        };

        // Sandbox info
        let router = self.state.sandbox_router();
        let sandbox_enabled = router.enabled();
        let sandbox_config = router.config();
        let effective_image = router.default_image().await;
        let container_name = {
            let owner_key = session_entry
                .as_ref()
                .and_then(|entry| entry.sandbox_owner_key.as_deref())
                .unwrap_or(&session_key);
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

    async fn raw_prompt(&self, params: Value) -> ServiceResult {
        let session_key = self.resolve_session_key_from_params(&params).await;

        let conn_id = params
            .get("_conn_id")
            .and_then(|v| v.as_str())
            .map(String::from);

        // Resolve provider.
        let history = self
            .session_store
            .read(&session_key)
            .await
            .unwrap_or_default();
        let provider = self
            .resolve_provider(&session_key, &history)
            .await
            .map_err(ServiceError::message)?;
        let tool_mode = provider.tool_mode();
        let native_tools = matches!(tool_mode, ToolMode::Native);
        let tools_enabled = tool_mode_enables_tools(tool_mode);

        // Build runtime context.
        let session_entry = self
            .session_metadata
            .get(&session_key)
            .await
            .map_err(ServiceError::message)?;
        let persona = self
            .load_prompt_persona_for_agent_run(&session_key, session_entry.as_ref())
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
        apply_request_runtime_context(
            &mut runtime_context.host,
            &params,
            persona
                .user
                .timezone
                .as_ref()
                .map(|timezone| timezone.name()),
        );

        // Resolve project context.
        let project_context = self
            .resolve_project_context(&session_key, conn_id.as_deref())
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
        let policy_ctx =
            build_policy_context(&raw_prompt_agent_id, Some(&runtime_context), Some(&params));
        let filtered_registry = {
            let registry_guard = self.tool_registry.read().await;
            let memory_setup = self
                .state
                .memory_manager()
                .map(|manager| (manager, Arc::clone(&provider)));
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
    async fn full_context(&self, params: Value) -> ServiceResult {
        let session_key = self.resolve_session_key_from_params(&params).await;

        let conn_id = params
            .get("_conn_id")
            .and_then(|v| v.as_str())
            .map(String::from);

        // Resolve provider.
        let history = self
            .session_store
            .read(&session_key)
            .await
            .unwrap_or_default();
        let provider = self
            .resolve_provider(&session_key, &history)
            .await
            .map_err(ServiceError::message)?;
        let tool_mode = provider.tool_mode();
        let native_tools = matches!(tool_mode, ToolMode::Native);
        let tools_enabled = tool_mode_enables_tools(tool_mode);

        // Build runtime context.
        let session_entry = self
            .session_metadata
            .get(&session_key)
            .await
            .map_err(ServiceError::message)?;
        let persona = self
            .load_prompt_persona_for_agent_run(&session_key, session_entry.as_ref())
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
        apply_request_runtime_context(
            &mut runtime_context.host,
            &params,
            persona
                .user
                .timezone
                .as_ref()
                .map(|timezone| timezone.name()),
        );

        // Resolve project context.
        let project_context = self
            .resolve_project_context(&session_key, conn_id.as_deref())
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
        let policy_ctx =
            build_policy_context(&full_ctx_agent_id, Some(&runtime_context), Some(&params));
        // Same preparation as the live run so the full-context prompt reflects
        // the lazy state of the current history.
        let filtered_registry = {
            let registry_guard = self.tool_registry.read().await;
            let memory_setup = self
                .state
                .memory_manager()
                .map(|manager| (manager, Arc::clone(&provider)));
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
            Arc,
            atomic::{AtomicBool, Ordering},
        },
    };

    use {
        chelix_agents::model::{ChatMessage, CompletionResponse, LlmProvider, StreamEvent},
        chelix_common::{ModelMetadata, ModelModality},
        chelix_config::ToolMode,
        chelix_providers::{ModelInfo, ProviderRegistry},
        chelix_service_traits::{
            ChatService, McpService, NoopMcpService, NoopProjectService, NoopTtsService,
            ProjectService, TtsService,
        },
        chelix_sessions::{
            SessionPromptQueueStore,
            metadata::{
                ExternalAgentKind, ExternalSessionIdentity, SessionBacking, SessionEntry,
                SqliteSessionMetadata,
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
        resolve_send_sync_outcome, send_sync_model_id, tool_mode_enables_tools,
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

    struct ValidationProvider;

    #[async_trait::async_trait]
    impl LlmProvider for ValidationProvider {
        fn name(&self) -> &str {
            "test"
        }

        fn id(&self) -> &str {
            "model"
        }

        async fn complete(
            &self,
            _messages: &[ChatMessage],
            _tools: &[Value],
        ) -> anyhow::Result<CompletionResponse> {
            panic!("provider must not run for rejected validation cases")
        }

        fn stream(
            &self,
            _messages: Vec<ChatMessage>,
        ) -> Pin<Box<dyn Stream<Item = StreamEvent> + Send + '_>> {
            Box::pin(tokio_stream::empty())
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
            streaming: true,
            zero_data_retention_enabled: false,
            reasoning_supported_efforts: vec![chelix_common::ReasoningEffort::from("off")],
            reasoning_summary: None,
            reasoning_include: None,
        }
    }

    fn session_entry_with_model(model: Option<&str>) -> SessionEntry {
        let backing = match model {
            Some(model) => SessionBacking::llm(
                chelix_common::ResolvedModelReasoning::try_new(
                    model.to_string(),
                    chelix_common::ReasoningEffort::from("off"),
                )
                .unwrap_or_else(|error| panic!("valid test pair: {error}")),
            ),
            None => SessionBacking::external(ExternalSessionIdentity::new(
                ExternalAgentKind::Codex,
                None,
            )),
        };
        SessionEntry {
            key: "session:test".to_string(),
            id: "test".to_string(),
            label: None,
            backing,
            created_at: 0,
            updated_at: 0,
            message_count: 0,
            project_id: None,
            archived: false,
            worktree_branch: None,
            channel_binding: None,
            parent_session_key: None,
            sandbox_owner_key: None,
            fork_point: None,
            mcp_disabled: None,
            preview: None,
            last_seen_message_count: 0,
            version: 0,
            agent_id: None,
            prompt_profile: chelix_sessions::metadata::PromptProfile::Chat,
        }
    }

    #[test]
    fn tool_mode_enables_tools_except_when_off() {
        assert!(tool_mode_enables_tools(ToolMode::Native));
        assert!(tool_mode_enables_tools(ToolMode::Text));
        assert!(!tool_mode_enables_tools(ToolMode::Off));
    }

    #[test]
    fn send_sync_prefers_explicit_model_over_session_model() {
        let entry = session_entry_with_model(Some("preset-model"));

        assert_eq!(
            send_sync_model_id(Some("override-model"), None, Some(&entry)),
            Some("override-model")
        );
    }

    #[test]
    fn send_sync_uses_session_model_without_override() {
        let entry = session_entry_with_model(Some("preset-model"));

        assert_eq!(
            send_sync_model_id(None, None, Some(&entry)),
            Some("preset-model")
        );
    }

    async fn validation_test_service() -> (
        tempfile::TempDir,
        LiveChatService,
        Arc<SqliteSessionMetadata>,
        Arc<SessionStore>,
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
        SqliteSessionMetadata::init(&pool)
            .await
            .unwrap_or_else(|error| panic!("session metadata setup: {error}"));
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
        let mut registry = ProviderRegistry::empty();
        registry.register(
            ModelInfo {
                id: "model".to_string(),
                provider: "test".to_string(),
                metadata: validation_model_metadata(),
            },
            Arc::new(ValidationProvider),
        );
        let runtime: Arc<dyn crate::runtime::ChatRuntime> =
            Arc::new(ValidationTestRuntime::default());
        let service = LiveChatService::new(
            Arc::new(RwLock::new(registry)),
            runtime,
            Arc::clone(&session_store),
            Arc::clone(&metadata),
            Arc::new(SessionPromptQueueStore::new(pool)),
            config.clone(),
            agents_config,
            chelix_config::ToolsConfigSource::snapshot(config.tools),
        );
        (directory, service, metadata, session_store)
    }

    #[tokio::test]
    async fn rejected_send_sync_agent_selection_preserves_entry_and_history() {
        let (_directory, service, metadata, session_store) = validation_test_service().await;
        let cases = [
            (
                "unknown explicit model",
                serde_json::json!({
                    "_session_key": "main",
                    "text": "hello",
                    "agent_id": "other",
                    "model": "test::missing",
                    "reasoningEffort": "off",
                }),
            ),
            (
                "unsupported effort",
                serde_json::json!({
                    "_session_key": "main",
                    "text": "hello",
                    "agent_id": "other",
                    "model": "test::model",
                    "reasoningEffort": "high",
                }),
            ),
        ];

        for (name, params) in cases {
            let before = metadata
                .get("main")
                .await
                .unwrap_or_else(|error| panic!("{name}: load entry before: {error}"))
                .unwrap_or_else(|| panic!("{name}: entry exists before"));
            let history_before = session_store
                .read("main")
                .await
                .unwrap_or_else(|error| panic!("{name}: read history before: {error}"));

            let result = service.send_sync(params).await;
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
