//! `ChatService` trait implementation for `LiveChatService`.

mod send;

const STOPPED_BY_USER: &str = "Stopped by user.";

use std::{
    collections::{HashMap, HashSet},
    path::Path,
    sync::Arc,
};

use {
    async_trait::async_trait,
    serde_json::Value,
    tokio::sync::RwLock,
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
    chelix_sessions::{ContentBlock, MessageContent, PersistedMessage},
    chelix_tools::policy::{PolicyContext, ToolPolicy},
};

use crate::{
    agent_loop::effective_tool_mode,
    channels::notify_channels_of_compaction,
    compaction,
    message::{
        infer_reply_medium, user_audio_path_from_params, user_documents_for_persistence,
        user_documents_from_params,
    },
    prompt::{
        apply_request_runtime_context, build_policy_context, build_prompt_runtime_context,
        clear_prompt_memory_snapshot, discover_skills_if_enabled, filter_skills_for_agent,
        load_prompt_persona_for_session, prepare_run_registry, prompt_build_limits_from_config,
        resolve_prompt_agent_id, resolve_prompt_mode_context,
    },
    run_with_tools::run_with_tools,
    streaming::run_streaming,
    types::*,
};

use super::*;

pub(super) fn resolved_turn_reasoning_effort(
    session_entry: Option<&chelix_sessions::metadata::SessionEntry>,
    persona: &PromptPersona,
    agent_id: &str,
) -> Option<String> {
    if let Some(reasoning_effort) = session_entry.and_then(|entry| entry.reasoning_effort.clone()) {
        return Some(reasoning_effort);
    }
    persona
        .config
        .agents
        .get_preset(agent_id)
        .and_then(|preset| preset.reasoning_effort.as_ref())
        .map(|effort| effort.as_str().to_string())
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
    session_entry: Option<&'a chelix_sessions::metadata::SessionEntry>,
) -> Option<&'a str> {
    explicit_model.or_else(|| session_entry.and_then(|entry| entry.model.as_deref()))
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
        let ephemeral = params
            .get("_ephemeral")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        let explicit_model = params.get("model").and_then(|v| v.as_str());
        let requested_reasoning_effort_override = requested_reasoning_effort(&params);
        let tool_controls =
            chelix_config::schema::AgentToolControls::from_tool_context(Some(&params));
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
        if !ephemeral {
            if let Err(e) = self
                .session_store
                .append(&session_key, &user_msg.to_value())
                .await
            {
                warn!("send_sync: failed to persist user message: {e}");
            }

            // Ensure this session appears in the sessions list.
            let _ = self.session_metadata.upsert(&session_key, None).await;
        }
        if let Some(agent_id) = requested_agent_id.as_deref()
            && let Err(error) = self
                .session_metadata
                .set_agent_id(&session_key, Some(agent_id))
                .await
        {
            warn!(
                session = %session_key,
                agent_id,
                error = %error,
                "send_sync: failed to assign requested agent to session"
            );
        }
        if !ephemeral {
            self.session_metadata.touch(&session_key, 1).await;
        }

        let session_entry = self.session_metadata.get(&session_key).await;
        let model_id = send_sync_model_id(explicit_model, session_entry.as_ref()).ok_or_else(|| {
            format!("session '{session_key}' has no model; pass 'model' explicitly or set the session model")
        })?;
        let provider: Arc<dyn chelix_agents::model::LlmProvider> = {
            let reg = self.providers.read().await;
            reg.get(model_id)
                .ok_or_else(|| format!("model '{model_id}' not found"))?
        };
        let session_agent_id = resolve_prompt_agent_id(session_entry.as_ref());
        let persona = load_prompt_persona_for_session(
            &self.config,
            &session_key,
            session_entry.as_ref(),
            self.session_state_store.as_deref(),
        )
        .await;
        let resolved_reasoning_effort = requested_reasoning_effort_override.or_else(|| {
            resolved_turn_reasoning_effort(session_entry.as_ref(), &persona, &session_agent_id)
        });
        let provider =
            apply_reasoning_effort_to_provider(provider, resolved_reasoning_effort.as_deref())?;
        let mut runtime_context = build_prompt_runtime_context(
            &self.state,
            &persona.config,
            &provider,
            &session_key,
            session_entry.as_ref(),
        )
        .await;
        runtime_context.mode = resolve_prompt_mode_context(&persona.config, session_entry.as_ref());
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
            .unwrap_or_default();
        if !ephemeral && !history.is_empty() {
            history.pop();
        }

        let run_id = uuid::Uuid::new_v4().to_string();
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
        let model_store = Arc::clone(&self.model_store);
        let user_message_index = history.len();

        if !ephemeral {
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
        }

        if !ephemeral {
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
        }

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
                &state,
                &model_store,
                &run_id,
                provider,
                &model_id,
                &user_content,
                &provider_name,
                &history,
                &session_key,
                &session_agent_id,
                resolved_reasoning_effort.clone(),
                desired_reply_medium,
                None,
                &[],
                Some(&runtime_context),
                None, // send_sync: no sender name
                (!ephemeral).then_some(&self.session_store),
                None, // send_sync: no client seq
                (!ephemeral).then(|| Arc::clone(&self.active_partial_assistant)),
                &terminal_runs,
            )
            .await
        } else {
            run_with_tools(
                persona,
                &state,
                &model_store,
                &run_id,
                provider,
                &model_id,
                &tool_registry,
                &user_content,
                &provider_name,
                &history,
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
                (!ephemeral).then_some(&self.session_store),
                false, // send_sync: MCP tools always enabled for API calls
                None,  // send_sync: no client seq
                (!ephemeral).then(|| Arc::clone(&self.active_thinking_text)),
                (!ephemeral).then(|| Arc::clone(&self.active_tool_calls)),
                (!ephemeral).then(|| Arc::clone(&self.active_partial_assistant)),
                &active_event_forwarders,
                &terminal_runs,
                None, // send_sync: no sender name
                Some(tool_controls),
            )
            .await
        };

        if !ephemeral {
            let mut runs_by_session = self.active_runs_by_session.write().await;
            if runs_by_session.get(&session_key) == Some(&run_id) {
                runs_by_session.remove(&session_key);
            }
            drop(runs_by_session);
            self.active_thinking_text.write().await.remove(&session_key);
            self.active_tool_calls.write().await.remove(&session_key);
            terminal_runs.write().await.remove(&run_id);
            self.active_partial_assistant
                .write()
                .await
                .remove(&session_key);
            self.active_reply_medium.write().await.remove(&session_key);
        }

        if !ephemeral && let Ok(count) = self.session_store.count(&session_key).await {
            self.session_metadata.touch(&session_key, count).await;
        }

        match result {
            Some(assistant_output) => Ok(serde_json::json!({
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
            None => {
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
                if let Ok(count) = self.session_store.count(&session_key).await {
                    self.session_metadata.touch(&session_key, count).await;
                }

                Err(error_msg.into())
            },
        }
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

        let (resolved_run_id, aborted) = Self::abort_run_handle(
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

        if aborted && let Some(key) = resolved_session_key.as_deref() {
            let interrupted_tool_calls = self
                .active_tool_calls
                .write()
                .await
                .remove(key)
                .unwrap_or_default();
            let event_result =
                Self::wait_for_event_forwarder(&self.active_event_forwarders, key).await;
            let partial = self.persist_partial_assistant_on_abort(key).await;
            let finalized_tool_segment = if partial.is_none() {
                self.finalize_active_tool_segment_on_abort(key, &event_result.tool_segment_indices)
                    .await
            } else {
                None
            };
            self.active_thinking_text.write().await.remove(key);
            self.active_reply_medium.write().await.remove(key);
            for tool_call in interrupted_tool_calls {
                let tool_result_index = match self.session_store.count(key).await {
                    Ok(count) => count as usize,
                    Err(error) => {
                        warn!(session = %key, error = %error, "failed to count history before persisting stopped tool call");
                        continue;
                    },
                };
                let tool_result = PersistedMessage::ToolResult {
                    tool_call_id: tool_call.id.clone(),
                    tool_name: tool_call.name.clone(),
                    arguments: Some(tool_call.arguments.clone()),
                    success: false,
                    result: None,
                    error: Some(STOPPED_BY_USER.to_string()),
                    reasoning: None,
                    context_budget: None,
                    created_at: Some(now_ms()),
                    run_id: Some(tool_call.run_id.clone()),
                };
                if let Err(error) = self
                    .session_store
                    .append(key, &tool_result.to_value())
                    .await
                {
                    warn!(session = %key, error = %error, "failed to persist stopped tool call");
                    continue;
                }
                broadcast(
                    &self.state,
                    "chat",
                    serde_json::json!({
                        "state": "tool_call_end",
                        "runId": tool_call.run_id,
                        "sessionKey": key,
                        "toolCallId": tool_call.id,
                        "toolName": tool_call.name,
                        "arguments": tool_call.arguments,
                        "success": false,
                        "error": { "detail": STOPPED_BY_USER },
                        "messageIndex": tool_result_index,
                    }),
                    BroadcastOpts::default(),
                )
                .await;
            }
            if let Ok(count) = self.session_store.count(key).await {
                self.session_metadata.touch(key, count).await;
            }
            let mut payload = serde_json::json!({
                "state": "aborted",
                "runId": resolved_run_id,
                "sessionKey": key,
            });
            if let Some((partial_message, message_index)) = partial.or(finalized_tool_segment) {
                payload["partialMessage"] = partial_message;
                if let Some(index) = message_index {
                    payload["messageIndex"] = serde_json::json!(index);
                }
            }
            broadcast(&self.state, "chat", payload, BroadcastOpts::default()).await;
            if let Some(run_id) = resolved_run_id.as_deref() {
                self.terminal_runs.write().await.remove(run_id);
            }
        }

        Ok(serde_json::json!({
            "aborted": aborted,
            "runId": resolved_run_id,
            "sessionKey": resolved_session_key,
        }))
    }

    async fn cancel_queued(&self, params: Value) -> ServiceResult {
        let session_key = params
            .get("sessionKey")
            .and_then(|v| v.as_str())
            .ok_or_else(|| "missing 'sessionKey'".to_string())?;

        let removed = self
            .message_queue
            .write()
            .await
            .remove(session_key)
            .unwrap_or_default();
        let count = removed.len();
        info!(session = %session_key, count, "cancel_queued: cleared message queue");

        broadcast(
            &self.state,
            "chat",
            serde_json::json!({
                "sessionKey": session_key,
                "state": "queue_cleared",
                "count": count,
            }),
            BroadcastOpts::default(),
        )
        .await;

        Ok(serde_json::json!({ "cleared": count }))
    }

    async fn history(&self, params: Value) -> ServiceResult {
        let session_key = self.resolve_session_key_from_params(&params).await;
        let messages = self
            .session_store
            .read(&session_key)
            .await
            .map_err(ServiceError::message)?;
        // Filter out empty assistant messages — they are kept in storage for LLM
        // history coherence but should not be shown in the UI.
        let visible: Vec<Value> = messages
            .into_iter()
            .filter(assistant_message_is_visible)
            .collect();
        Ok(serde_json::json!(visible))
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
        self.session_metadata.touch(&session_key, 0).await;
        self.session_metadata.set_preview(&session_key, None).await;

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

        let message_count = self.session_store.count(&session_key).await.unwrap_or(0);
        self.session_metadata
            .touch(&session_key, message_count)
            .await;

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
        let message_count = self.session_store.count(&session_key).await.unwrap_or(0);
        let session_entry = self.session_metadata.get(&session_key).await;
        let prompt_persona = load_prompt_persona_for_session(
            &self.config,
            &session_key,
            session_entry.as_ref(),
            self.session_state_store.as_deref(),
        )
        .await;
        let (provider_arc, provider_name, supports_tools) = {
            let reg = self.providers.read().await;
            let session_model = session_entry.as_ref().and_then(|e| e.model.as_deref());
            let provider = match session_model {
                Some(id) => reg.get(id),
                None => reg.first(),
            };
            (
                provider.clone(),
                provider.as_ref().map(|p| p.name().to_string()),
                provider
                    .as_ref()
                    .map(|p| p.supports_tools())
                    .unwrap_or(true),
            )
        };
        let session_info = serde_json::json!({
            "key": session_key,
            "messageCount": message_count,
            "model": session_entry.as_ref().and_then(|e| e.model.as_deref()),
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

        // Tools (only include if the provider supports tool calling)
        let mcp_disabled = session_entry
            .as_ref()
            .and_then(|e| e.mcp_disabled)
            .unwrap_or(false);
        let config = chelix_config::discover_and_load().map_err(ServiceError::message)?;
        // Read history once: the token usage below reuses it, and the tool
        // catalog needs it to restore lazy schema visibility.
        let messages = self
            .session_store
            .read(&session_key)
            .await
            .unwrap_or_default();
        // `tools` is the UI discovery catalog (name + description of every
        // allowed public tool, plus `get_tool` in lazy mode). `toolSchemaCount`
        // separately reports how many parameter schemas are currently visible.
        let (tools, tool_schema_count): (Vec<Value>, usize) = if supports_tools {
            let registry_guard = self.tool_registry.read().await;
            let list_agent_id = resolve_prompt_agent_id(session_entry.as_ref());
            let list_ctx = PolicyContext {
                agent_id: list_agent_id.clone(),
                ..Default::default()
            };
            let memory_setup = provider_arc.as_ref().and_then(|provider| {
                self.state
                    .memory_manager()
                    .map(|manager| (manager, Arc::clone(provider)))
            });
            match prepare_run_registry(
                &registry_guard,
                &config,
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
            let session_model = session_entry.as_ref().and_then(|e| e.model.as_deref());
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
            let id = router.sandbox_id_for(&session_key);
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
        let host_is_root = detect_host_root_user().await;
        // Sandbox containers currently run as root by default.
        let command_is_root = if sandbox_enabled {
            Some(true)
        } else {
            host_is_root
        };
        let command_prompt_symbol = command_is_root.map(|is_root| {
            if is_root {
                "#"
            } else {
                "$"
            }
        });
        let execution_info = serde_json::json!({
            "mode": if sandbox_enabled { "sandbox" } else { "host" },
            "hostIsRoot": host_is_root,
            "isRoot": command_is_root,
            "promptSymbol": command_prompt_symbol,
        });

        // Discover enabled skills/plugins (only if provider supports tools and
        // `[skills] enabled` is true — see #655).
        let skills_list: Vec<Value> = if supports_tools {
            discover_skills_if_enabled(&config)
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

        // MCP servers (only if provider supports tools)
        let mcp_servers = if supports_tools {
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
            "execution": execution_info,
            "promptMemory": prompt_persona.memory_status,
            "supportsTools": supports_tools,
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
        let tool_mode = effective_tool_mode(&*provider);
        let native_tools = matches!(tool_mode, ToolMode::Native);
        let tools_enabled = !matches!(tool_mode, ToolMode::Off);

        // Build runtime context.
        let session_entry = self.session_metadata.get(&session_key).await;
        let persona = load_prompt_persona_for_session(
            &self.config,
            &session_key,
            session_entry.as_ref(),
            self.session_state_store.as_deref(),
        )
        .await;
        let mut runtime_context = build_prompt_runtime_context(
            &self.state,
            &persona.config,
            &provider,
            &session_key,
            session_entry.as_ref(),
        )
        .await;
        runtime_context.mode = resolve_prompt_mode_context(&persona.config, session_entry.as_ref());
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
            .await;

        // Discover skills (gated on `[skills] enabled` — see #655).
        let discovered_skills = discover_skills_if_enabled(&persona.config).await;

        // Check MCP disabled.
        let mcp_disabled = session_entry
            .as_ref()
            .and_then(|entry| entry.mcp_disabled)
            .unwrap_or(false);

        let raw_prompt_agent_id = resolve_prompt_agent_id(session_entry.as_ref());

        // Apply per-agent skill policy.
        let discovered_skills =
            if let Some(preset) = persona.config.agents.get_preset(&raw_prompt_agent_id) {
                filter_skills_for_agent(discovered_skills, &preset.skills)
            } else {
                discovered_skills
            };

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
                Some(&persona.identity),
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
                Some(&persona.identity),
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
        let tool_mode = effective_tool_mode(&*provider);
        let native_tools = matches!(tool_mode, ToolMode::Native);
        let tools_enabled = !matches!(tool_mode, ToolMode::Off);

        // Build runtime context.
        let session_entry = self.session_metadata.get(&session_key).await;
        let persona = load_prompt_persona_for_session(
            &self.config,
            &session_key,
            session_entry.as_ref(),
            self.session_state_store.as_deref(),
        )
        .await;
        let mut runtime_context = build_prompt_runtime_context(
            &self.state,
            &persona.config,
            &provider,
            &session_key,
            session_entry.as_ref(),
        )
        .await;
        runtime_context.mode = resolve_prompt_mode_context(&persona.config, session_entry.as_ref());
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
            .await;

        // Discover skills (gated on `[skills] enabled` — see #655).
        let discovered_skills = discover_skills_if_enabled(&persona.config).await;

        // Check MCP disabled.
        let mcp_disabled = session_entry
            .as_ref()
            .and_then(|entry| entry.mcp_disabled)
            .unwrap_or(false);

        // Build filtered tool registry.
        let full_ctx_agent_id = resolve_prompt_agent_id(session_entry.as_ref());

        // Apply per-agent skill policy.
        let discovered_skills =
            if let Some(preset) = persona.config.agents.get_preset(&full_ctx_agent_id) {
                filter_skills_for_agent(discovered_skills, &preset.skills)
            } else {
                discovered_skills
            };
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
                Some(&persona.identity),
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
                Some(&persona.identity),
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
        // `values_to_chat_messages` handles `tool_result` → `tool` conversion.
        let mut messages = Vec::with_capacity(1 + history.len());
        messages.push(ChatMessage::system(system_prompt));
        messages.extend(values_to_chat_messages(&history));

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
        let session_entry = self.session_metadata.get(&session_key).await;
        let agent_id = resolve_prompt_agent_id(session_entry.as_ref());
        let snapshot_cleared = clear_prompt_memory_snapshot(
            &session_key,
            &agent_id,
            self.session_state_store.as_deref(),
        )
        .await;
        let persona = load_prompt_persona_for_session(
            &self.config,
            &session_key,
            session_entry.as_ref(),
            self.session_state_store.as_deref(),
        )
        .await;

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

    async fn active_thinking_text(&self, session_key: &str) -> Option<String> {
        self.active_thinking_text
            .read()
            .await
            .get(session_key)
            .cloned()
    }

    async fn active_voice_pending(&self, session_key: &str) -> bool {
        self.active_reply_medium
            .read()
            .await
            .get(session_key)
            .is_some_and(|m| *m == ReplyMedium::Voice)
    }

    async fn active_tool_calls(&self, session_key: &str) -> Vec<Value> {
        self.active_tool_calls
            .read()
            .await
            .get(session_key)
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .filter_map(|call| serde_json::to_value(call).ok())
            .collect()
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

        let thinking_text = self
            .active_thinking_text
            .read()
            .await
            .get(session_key)
            .cloned();

        let tool_calls: Vec<ActiveToolCall> = self
            .active_tool_calls
            .read()
            .await
            .get(session_key)
            .cloned()
            .unwrap_or_default();

        Ok(serde_json::json!({
            "active": true,
            "sessionKey": session_key,
            "thinkingText": thinking_text,
            "toolCalls": tool_calls,
        }))
    }
}

#[cfg(test)]
mod tests {
    use chelix_sessions::metadata::SessionEntry;

    use super::send_sync_model_id;

    fn session_entry_with_model(model: Option<&str>) -> SessionEntry {
        SessionEntry {
            key: "session:test".to_string(),
            id: "test".to_string(),
            label: None,
            model: model.map(str::to_string),
            created_at: 0,
            updated_at: 0,
            message_count: 0,
            project_id: None,
            archived: false,
            worktree_branch: None,
            channel_binding: None,
            parent_session_key: None,
            fork_point: None,
            mcp_disabled: None,
            preview: None,
            last_seen_message_count: 0,
            version: 0,
            agent_id: None,
            node_id: None,
            mode_id: None,
            external_agent_kind: None,
            external_session_id: None,
            reasoning_effort: None,
        }
    }

    #[test]
    fn send_sync_prefers_explicit_model_over_session_model() {
        let entry = session_entry_with_model(Some("preset-model"));

        assert_eq!(
            send_sync_model_id(Some("override-model"), Some(&entry)),
            Some("override-model")
        );
    }

    #[test]
    fn send_sync_uses_session_model_without_override() {
        let entry = session_entry_with_model(Some("preset-model"));

        assert_eq!(send_sync_model_id(None, Some(&entry)), Some("preset-model"));
    }
}
