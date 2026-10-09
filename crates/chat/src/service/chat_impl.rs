//! `ChatService` trait implementation for `LiveChatService`.

mod send;

use std::{
    collections::{HashMap, HashSet},
    future::Future,
    path::Path,
    sync::Arc,
    time::Duration,
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
        prompt::{
            build_system_prompt_minimal_runtime_details,
            build_system_prompt_with_session_runtime_details,
        },
    },
    chelix_config::ToolMode,
    chelix_service_traits::{
        ChatCompactRequest, ChatContextRequest, ChatExecutionContext, ChatFullContextRequest,
        ChatRawPromptRequest, ChatService, ServiceError, ServiceResult, SessionTerminal,
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

use super::{session_gate::terminal_from_outcome, *};

fn skills_context_entries(
    skills: Vec<chelix_skills::types::SkillMetadata>,
    agent_id: &str,
    policy: &chelix_config::AgentSkillPolicy,
) -> Vec<Value> {
    filter_skills_for_agent(skills, agent_id, policy)
        .iter()
        .map(|skill| {
            serde_json::json!({
                "name": skill.name,
                "description": skill.description,
                "source": skill.source,
            })
        })
        .collect()
}

#[cfg(test)]
mod skills_context_tests {
    use {
        super::*,
        chelix_skills::types::{SkillMetadata, SkillSource},
    };

    #[test]
    fn context_skills_filters_agent_access_and_serializes_fields() {
        let entries = skills_context_entries(
            vec![
                SkillMetadata {
                    name: "demo".into(),
                    deny: vec!["agent1".into()],
                    source: Some(SkillSource::Project),
                    ..Default::default()
                },
                SkillMetadata {
                    name: "visible".into(),
                    description: "Visible skill".into(),
                    source: Some(SkillSource::Personal),
                    ..Default::default()
                },
            ],
            "agent1",
            &chelix_config::AgentSkillPolicy::default(),
        );
        assert_eq!(entries, [
            serde_json::json!({ "name": "visible", "description": "Visible skill", "source": "personal" })
        ]);
    }
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

fn parse_tool_permission_decision(
    params: &Value,
) -> Result<chelix_agents::runner::ToolPermissionDecision, ServiceError> {
    let decision = params
        .get("decision")
        .and_then(Value::as_str)
        .ok_or_else(|| ServiceError::message("missing 'decision'"))?;
    match decision {
        "approve" => Ok(chelix_agents::runner::ToolPermissionDecision::Approve),
        "skip" => Ok(chelix_agents::runner::ToolPermissionDecision::Skip),
        "deny" => {
            let feedback = params
                .get("feedback")
                .and_then(Value::as_str)
                .ok_or_else(|| ServiceError::message("missing 'feedback'"))?;
            Ok(chelix_agents::runner::ToolPermissionDecision::Deny {
                feedback: feedback.to_owned(),
            })
        },
        _ => Err(ServiceError::message(format!(
            "invalid decision: {decision}"
        ))),
    }
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

async fn wait_until_session_run_unmapped(
    service: &LiveChatService,
    session_key: &str,
    run_id: &str,
) {
    let mut version = service.session_gates.subscribe();
    let mut warned = false;
    loop {
        {
            let runs_by_session = service.active_runs_by_session.read().await;
            if runs_by_session.get(session_key).map(String::as_str) != Some(run_id) {
                return;
            }
        }
        if warned {
            if version.changed().await.is_err() {
                return;
            }
            continue;
        }
        tokio::select! {
            biased;
            result = version.changed() => {
                if result.is_err() {
                    return;
                }
            },
            () = tokio::time::sleep(Duration::from_secs(5)) => {
                warn!(
                    session_key,
                    run_id,
                    "chat.abort still waiting for the run to unmap"
                );
                warned = true;
            },
        }
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
        let mut guard = self.stop_gate.begin_send(&session_key)?;
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

        let (chat_history, history_tail) =
            crate::active_context::load_active_messages(&self.session_store, &session_key)
                .await
                .map_err(ServiceError::message)?;
        self.session_store
            .append_at_index(&session_key, &user_msg.to_value(), history_tail)
            .await
            .map_err(ServiceError::message)?;
        let (runtime_context, compaction_reminder) = match async {
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

            let pointers = self
                .session_store
                .pointers(&session_key)
                .await
                .map_err(ServiceError::message)?;
            let first_user = if let Some(index) = pointers.first_user_index {
                Some(
                    self.session_store
                        .read_record(&session_key, index)
                        .await
                        .map_err(ServiceError::message)?,
                )
            } else {
                None
            };
            let compaction_reminder = crate::compaction_reminder::CompactionReminder::from_parts(
                persona.agent.compaction_reminder,
                pointers.last_checkpoint_index.is_some(),
                first_user.as_ref(),
            )
            .map_err(ServiceError::message)?;
            Ok((runtime_context, compaction_reminder))
        }
        .await
        {
            Ok(value) => value,
            Err(error) => {
                self.session_gates.begin_turn(&session_key).await;
                return Err(error);
            },
        };

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
        let tool_choice = request.tool_choice;
        let provider_name = provider.name().to_string();
        let model_id = provider.id().to_string();

        self.stop_gate.confirm_start(&mut guard)?;
        self.activate_session_turn(&session_key, &run_id).await;
        {
            let mut active = self.active_runs.write().await;
            self.stop_gate.publish_run(
                &mut active,
                &mut guard,
                &session_key,
                &run_id,
                cancellation_token.clone(),
            );
        }
        let mut run_finish = stop_gate::RunFinishGuard::arm(
            Arc::clone(&self.stop_gate),
            Arc::clone(&self.session_gates),
            Arc::clone(&self.active_runs),
            Arc::clone(&self.active_runs_by_session),
            session_key.clone(),
            run_id.clone(),
        );
        if let Some(gate) = self.after_publish_run.clone() {
            let release = gate.release.notified();
            gate.arrived.notify_one();
            release.await;
        }
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
                compaction_reminder,
                &cancellation_token,
                &state,
                &run_id,
                provider,
                &model_id,
                &user_content,
                &provider_name,
                chat_history,
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
                compaction_reminder,
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
                chat_history,
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
                None, // send_sync: no client seq
                Some(Arc::clone(&self.active_tool_invocations)),
                Some(Arc::clone(&self.active_partial_assistant)),
                &active_event_forwarders,
                &terminal_runs,
                sender_name,
                tool_choice,
                Some(crate::tool_permission::ToolPermissionRuntime {
                    manager: Arc::clone(&self.tool_permissions),
                    metadata: Arc::clone(&self.session_metadata),
                }),
            )
            .await
        };

        self.session_gates
            .finish_turn(&session_key, terminal_from_outcome(&result))
            .await;
        let stop_detail = if matches!(result, ChatRunOutcome::Failed) {
            self.state.last_run_error(&run_id).await
        } else {
            None
        };
        {
            let mut active = self.active_runs.write().await;
            self.stop_gate.finish_run(
                &mut active,
                &run_id,
                stop_gate::StopGate::report_outcome(&result, stop_detail),
            );
        }
        let mut runs_by_session = self.active_runs_by_session.write().await;
        if runs_by_session.get(&session_key) == Some(&run_id) {
            runs_by_session.remove(&session_key);
        }
        drop(runs_by_session);
        self.session_gates.notify();
        run_finish.disarm();
        let _ = self.tool_permissions.drop_session(&session_key).await;
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

        if let Some(session_key) = resolved_session_key.as_deref() {
            let dropped = self.tool_permissions.drop_session(session_key).await;
            for view in dropped {
                self.state
                    .broadcast(
                        "tool.permission.resolved",
                        serde_json::json!({
                            "sessionKey": view.session_key,
                            "runId": view.run_id,
                            "toolCallId": view.tool_call_id,
                            "toolName": view.tool_name,
                            "phase": view.phase,
                            "decision": "cancelled",
                        }),
                    )
                    .await;
            }
        }
        if let (Some(session_key), Some(run_id)) =
            (resolved_session_key.as_deref(), resolved_run_id.as_deref())
        {
            wait_until_session_run_unmapped(self, session_key, run_id).await;
        }

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
        self.session_gates.forget(&session_key).await;

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

        let tail = match self.session_store.pointers(session_key).await {
            Ok(pointers) => pointers.canonical_tail,
            Err(chelix_sessions::Error::NoCanonicalJournal { .. }) => 0,
            Err(error) => return Err(ServiceError::message(error)),
        };
        if tail == 0 {
            return Err("nothing to compact".into());
        }

        // Rebuild the session system prompt and tool schemas exactly as a
        // regular turn would, so the summarization request shares the
        // provider prompt-cache prefix with the previous turn.
        let (system_prompt, tools) = self
            .session_prompt_context(session_key, &provider, &context)
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
        // Token totals come from `token_totals`. Lazy names come from `visible_tools`.
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
            let visible_tools = crate::active_context::visible_tools(
                &self.session_store,
                session_key,
                true,
                matches!(
                    prompt_persona.config.tools.registry_mode,
                    chelix_config::ToolRegistryMode::Lazy
                ),
            )
            .await
            .map_err(ServiceError::message)?;
            let effective_registry = prepare_run_registry(
                &registry_guard,
                &prompt_persona.config,
                &[],
                &list_ctx,
                true,
                &list_agent_id,
                memory_setup,
                visible_tools,
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

        // Token usage from API-reported counts stored in canonical records.
        let usage = {
            let totals = self
                .session_store
                .token_totals(session_key)
                .await
                .map_err(ServiceError::message)?;
            session_token_usage_from_totals(&totals)
        };
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
            skills_context_entries(
                discover_skills_if_enabled(&prompt_persona.config).await,
                &prompt_persona.agent_id,
                &prompt_persona.agent.skills,
            )
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

        let raw_prompt_agent_id = persona.agent_id.clone();

        // Apply per-agent skill policy.
        let discovered_skills = filter_skills_for_agent(
            discovered_skills,
            &raw_prompt_agent_id,
            &persona.agent.skills,
        );

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
            let visible_tools = crate::active_context::visible_tools(
                &self.session_store,
                session_key,
                tools_enabled,
                matches!(
                    persona.config.tools.registry_mode,
                    chelix_config::ToolRegistryMode::Lazy
                ),
            )
            .await
            .map_err(ServiceError::message)?;
            prepare_run_registry(
                &registry_guard,
                &persona.config,
                &discovered_skills,
                &policy_ctx,
                tools_enabled,
                &raw_prompt_agent_id,
                memory_setup,
                visible_tools,
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
        let compaction_reminder = crate::active_context::reminder_from_journal(
            &self.session_store,
            session_key,
            persona.agent.compaction_reminder,
        )
        .await
        .map_err(ServiceError::message)?;
        let system_prompt = compaction_reminder.render(&prompt_build.prompt);
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

        // Build filtered tool registry.
        let full_ctx_agent_id = persona.agent_id.clone();

        // Apply per-agent skill policy.
        let discovered_skills =
            filter_skills_for_agent(discovered_skills, &full_ctx_agent_id, &persona.agent.skills);
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
            let visible_tools = crate::active_context::visible_tools(
                &self.session_store,
                session_key,
                tools_enabled,
                matches!(
                    persona.config.tools.registry_mode,
                    chelix_config::ToolRegistryMode::Lazy
                ),
            )
            .await
            .map_err(ServiceError::message)?;
            prepare_run_registry(
                &registry_guard,
                &persona.config,
                &discovered_skills,
                &policy_ctx,
                tools_enabled,
                &full_ctx_agent_id,
                memory_setup,
                visible_tools,
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
        let compaction_reminder = crate::active_context::reminder_from_journal(
            &self.session_store,
            session_key,
            persona.agent.compaction_reminder,
        )
        .await
        .map_err(ServiceError::message)?;
        let system_prompt = compaction_reminder.render(&prompt_build.prompt);
        let system_prompt_chars = system_prompt.len();

        // Keep raw assistant outputs (including provider/model/token metadata)
        // so the UI can show a debug view of what the LLM actually returned.
        let llm_outputs = self
            .session_store
            .assistant_payloads(session_key)
            .await
            .map_err(ServiceError::message)?;

        let (active_messages, _) =
            crate::active_context::load_active_messages(&self.session_store, session_key)
                .await
                .map_err(ServiceError::message)?;
        let mut messages = Vec::with_capacity(1 + active_messages.len());
        messages.push(ChatMessage::system(system_prompt));
        messages.extend(active_messages);

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

    async fn tool_permission_pending(&self, session_key: &str) -> ServiceResult {
        let requests = self.tool_permissions.pending_for_session(session_key).await;
        serde_json::to_value(serde_json::json!({ "requests": requests }))
            .map_err(|error| ServiceError::message(error.to_string()))
    }

    async fn tool_permission_resolve(&self, params: Value) -> ServiceResult {
        let session_key = params
            .get("sessionKey")
            .and_then(|value| value.as_str())
            .ok_or_else(|| ServiceError::message("missing 'sessionKey'"))?;
        let tool_call_id = params
            .get("toolCallId")
            .and_then(|value| value.as_str())
            .ok_or_else(|| ServiceError::message("missing 'toolCallId'"))?;
        let phase: chelix_agents::runner::ToolPermissionPhase =
            serde_json::from_value(params.get("phase").cloned().unwrap_or(Value::Null))
                .map_err(|error| ServiceError::message(format!("invalid phase: {error}")))?;
        let decision = parse_tool_permission_decision(&params)?;
        let view = self
            .tool_permissions
            .resolve(session_key, tool_call_id, phase, decision)
            .await
            .map_err(ServiceError::message)?;
        Ok(serde_json::json!({
            "ok": true,
            "sessionKey": view.session_key,
            "runId": view.run_id,
            "toolCallId": view.tool_call_id,
            "phase": view.phase,
        }))
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

    async fn wait_for_session_gate(
        &self,
        session_key: &str,
    ) -> Result<Option<SessionTerminal>, ServiceError> {
        let session_key = session_key.to_string();
        let active_runs_by_session = Arc::clone(&self.active_runs_by_session);
        Ok(self
            .session_gates
            .wait_for_gate(&session_key, || {
                let active_runs_by_session = Arc::clone(&active_runs_by_session);
                let session_key = session_key.clone();
                async move {
                    active_runs_by_session
                        .read()
                        .await
                        .contains_key(&session_key)
                }
            })
            .await)
    }

    async fn session_terminal(
        &self,
        session_key: &str,
    ) -> Result<Option<SessionTerminal>, ServiceError> {
        Ok(self.session_gates.last_terminal(session_key).await)
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
        time::Duration,
    };

    use {
        chelix_agents::{
            UserContent,
            model::{ChatMessage, CompletionOptions, LlmProvider, StreamEvent, Usage},
        },
        chelix_common::{ModelMetadata, ModelModality, ModelOverride},
        chelix_config::ToolMode,
        chelix_providers::{ModelInfo, ProviderRegistry},
        chelix_service_traits::{
            ChatCompactRequest, ChatContextRequest, ChatExecutionContext, ChatFullContextRequest,
            ChatRawPromptRequest, ChatSendRequest, ChatSendSyncRequest, ChatService, McpService,
            NoopMcpService, NoopProjectService, NoopTtsService, ProjectService, ServiceError,
            SessionTerminal, TtsService,
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
        broadcasts: Arc<Mutex<Vec<(String, Value)>>>,
    }

    impl Default for ValidationTestRuntime {
        fn default() -> Self {
            Self {
                sandbox_router: Arc::new(chelix_tools::sandbox::SandboxRouter::disabled()),
                tts: NoopTtsService,
                project: NoopProjectService,
                mcp: NoopMcpService,
                broadcasts: Arc::new(Mutex::new(Vec::new())),
            }
        }
    }

    #[async_trait::async_trait]
    impl crate::runtime::ChatRuntime for ValidationTestRuntime {
        async fn broadcast(&self, topic: &str, payload: Value) {
            self.broadcasts
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .push((topic.to_string(), payload));
        }

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
        captured_messages: Arc<Mutex<Vec<Vec<ChatMessage>>>>,
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
            messages: Vec<ChatMessage>,
            _tools: Vec<Value>,
            _options: CompletionOptions,
        ) -> Pin<Box<dyn Stream<Item = StreamEvent> + Send + '_>> {
            self.captured_messages
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .push(messages);
            Box::pin(tokio_stream::iter(vec![
                StreamEvent::Delta("summary".to_string()),
                StreamEvent::Done(Usage::default()),
            ]))
        }

        fn stream(
            &self,
            messages: Vec<ChatMessage>,
        ) -> Pin<Box<dyn Stream<Item = StreamEvent> + Send + '_>> {
            self.captured_messages
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .push(messages);
            let segment_id = chelix_common::ProviderSegmentId::new("validation");
            Box::pin(tokio_stream::iter(vec![
                StreamEvent::SegmentStart {
                    segment_id: segment_id.clone(),
                },
                StreamEvent::Delta("summary".to_string()),
                StreamEvent::Done(Usage::default()),
            ]))
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
                captured_messages: Arc::clone(&self.captured_messages),
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
        Arc<Mutex<Vec<Vec<ChatMessage>>>>,
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
        let captured_messages = Arc::new(Mutex::new(Vec::new()));
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
                    captured_messages: Arc::clone(&captured_messages),
                }),
            );
        }
        let broadcasts = Arc::new(Mutex::new(Vec::new()));
        let runtime = ValidationTestRuntime {
            broadcasts: Arc::clone(&broadcasts),
            ..ValidationTestRuntime::default()
        };
        let runtime: Arc<dyn crate::runtime::ChatRuntime> = Arc::new(runtime);
        let mut service = LiveChatService::new(
            Arc::new(RwLock::new(registry)),
            runtime,
            Arc::clone(&session_store),
            Arc::clone(&metadata),
            Arc::new(QueuedPrompts::new(pool)),
            config.clone(),
            agents_config,
            chelix_config::ToolsConfigSource::snapshot(config.tools),
        );
        service.test_broadcasts = broadcasts;
        (
            directory,
            service,
            metadata,
            session_store,
            resolved_efforts,
            captured_messages,
        )
    }

    #[tokio::test]
    async fn chat_turn_resolution_uses_complete_request_or_persisted_session_pair() {
        let (_directory, service, _metadata, _session_store, _resolved_efforts, _captured_messages) =
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
        let (_directory, service, _metadata, session_store, resolved_efforts, _captured_messages) =
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
        let (_directory, service, metadata, _session_store, resolved_efforts, _captured_messages) =
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
        let (_directory, service, metadata, _session_store, _resolved_efforts, _captured_messages) =
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
        let (_directory, service, metadata, session_store, _resolved_efforts, _captured_messages) =
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
        let (_directory, service, metadata, session_store, _resolved_efforts, _captured_messages) =
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
    async fn send_sync_keeps_unclosed_provider_segment_and_sends_user_once() {
        let (_directory, service, _metadata, session_store, _resolved_efforts, captured_messages) =
            validation_test_service().await;
        session_store
            .append(
                "main",
                &serde_json::json!({
                    "role": "provider_update",
                    "segmentId": "seg-1",
                    "itemId": "msg_0",
                    "position": 1,
                    "updateSeq": 1,
                    "payload": {"update_type": "message_done", "text": "partial answer"}
                }),
            )
            .await
            .unwrap_or_else(|error| panic!("append open segment: {error}"));

        service
            .send_sync(
                ChatSendSyncRequest {
                    text: "hello".to_string(),
                    model_override: None,
                    tool_choice: None,
                    input_medium: None,
                },
                ChatExecutionContext::internal(SessionKey::new("main")),
            )
            .await
            .unwrap_or_else(|error| panic!("send_sync: {error}"));

        let captured = captured_messages
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone();
        assert_eq!(captured.len(), 1, "provider was called once");
        let messages = &captured[0];
        let user_indexes = messages
            .iter()
            .enumerate()
            .filter_map(|(index, message)| match message {
                ChatMessage::User {
                    content: UserContent::Text(text),
                    ..
                } if text == "hello" || text.ends_with("\n\nhello") => Some(index),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(user_indexes.len(), 1);
        assert_eq!(user_indexes[0], messages.len() - 1);
        let assistant_index = messages
            .iter()
            .position(|message| {
                matches!(
                    message,
                    ChatMessage::Assistant {
                        content: Some(text),
                        ..
                    } if text == "partial answer"
                )
            })
            .unwrap_or_else(|| panic!("replayed assistant missing"));
        assert!(assistant_index < user_indexes[0]);
    }

    #[tokio::test]
    async fn busy_send_persists_turn_settings_before_prompt_only_enqueue() {
        let (_directory, service, metadata, _session_store, _resolved_efforts, _captured_messages) =
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

    async fn unmap_session_run(service: &LiveChatService, session_key: &str, run_id: &str) {
        service.active_runs.write().await.remove(run_id);
        service
            .active_runs_by_session
            .write()
            .await
            .remove(session_key);
        service.session_gates.notify();
    }

    #[tokio::test]
    async fn abort_waits_until_the_session_run_is_unmapped() {
        let (_directory, service, _metadata, _session_store, _resolved_efforts, _captured_messages) =
            validation_test_service().await;
        let session_key = "session:child";
        let run_id = "run-1";
        let cancellation_token = CancellationToken::new();
        service
            .active_runs
            .write()
            .await
            .insert(run_id.to_owned(), cancellation_token.clone());
        service
            .active_runs_by_session
            .write()
            .await
            .insert(session_key.to_owned(), run_id.to_owned());

        let unmap_service = service.clone();
        let unmap_token = cancellation_token.clone();
        let unmap = tokio::spawn(async move {
            unmap_token.cancelled().await;
            unmap_session_run(&unmap_service, session_key, run_id).await;
        });

        let response = service
            .abort(serde_json::json!({ "sessionKey": session_key }))
            .await
            .unwrap_or_else(|error| panic!("abort should succeed: {error}"));
        unmap
            .await
            .unwrap_or_else(|error| panic!("unmap task: {error}"));
        assert_eq!(response["aborted"], serde_json::json!(true));
        assert!(cancellation_token.is_cancelled());

        let active = service
            .active(serde_json::json!({ "sessionKey": session_key }))
            .await
            .unwrap_or_else(|error| panic!("chat.active should succeed: {error}"));
        assert_eq!(active["active"], serde_json::json!(false));
        assert!(!service.active_runs.read().await.contains_key(run_id));
    }

    #[tokio::test]
    async fn abort_does_not_overwrite_a_terminal_run_outcome() {
        let (_directory, service, _metadata, _session_store, _resolved_efforts, _captured_messages) =
            validation_test_service().await;
        let session_key = "session:child";
        let run_id = "run-1";
        let cancellation_token = CancellationToken::new();
        service
            .session_gates
            .finish_turn(session_key, SessionTerminal::Completed)
            .await;
        service
            .terminal_runs
            .write()
            .await
            .insert(run_id.to_owned());
        service
            .active_runs
            .write()
            .await
            .insert(run_id.to_owned(), cancellation_token.clone());
        service
            .active_runs_by_session
            .write()
            .await
            .insert(session_key.to_owned(), run_id.to_owned());

        let abort_service = service.clone();
        let abort = tokio::spawn(async move {
            abort_service
                .abort(serde_json::json!({ "sessionKey": session_key }))
                .await
        });
        tokio::task::yield_now().await;
        unmap_session_run(&service, session_key, run_id).await;
        let response = abort
            .await
            .unwrap_or_else(|error| panic!("abort task: {error}"))
            .unwrap_or_else(|error| panic!("abort should succeed: {error}"));
        assert_eq!(response["aborted"], serde_json::json!(false));
        assert!(!cancellation_token.is_cancelled());
        let active = service
            .active(serde_json::json!({ "sessionKey": session_key }))
            .await
            .unwrap_or_else(|error| panic!("chat.active should succeed: {error}"));
        assert_eq!(active["active"], serde_json::json!(false));
        assert_eq!(
            service
                .session_terminal(session_key)
                .await
                .unwrap_or_else(|error| panic!("session terminal should succeed: {error}")),
            Some(SessionTerminal::Completed)
        );
    }

    #[tokio::test]
    async fn abort_stays_pending_when_version_bumps_without_unmapping() {
        let (_directory, service, _metadata, _session_store, _resolved_efforts, _captured_messages) =
            validation_test_service().await;
        let session_key = "session:child";
        let run_id = "run-1";
        let cancellation_token = CancellationToken::new();
        service
            .active_runs
            .write()
            .await
            .insert(run_id.to_owned(), cancellation_token.clone());
        service
            .active_runs_by_session
            .write()
            .await
            .insert(session_key.to_owned(), run_id.to_owned());

        let abort_service = service.clone();
        let abort = tokio::spawn(async move {
            abort_service
                .abort(serde_json::json!({ "sessionKey": session_key }))
                .await
        });
        cancellation_token.cancelled().await;
        service.session_gates.notify();
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert!(
            !abort.is_finished(),
            "abort must keep waiting while the session run is still mapped"
        );
        unmap_session_run(&service, session_key, run_id).await;
        let response = abort
            .await
            .unwrap_or_else(|error| panic!("abort task: {error}"))
            .unwrap_or_else(|error| panic!("abort should succeed: {error}"));
        assert_eq!(response["aborted"], serde_json::json!(true));
        let active = service
            .active(serde_json::json!({ "sessionKey": session_key }))
            .await
            .unwrap_or_else(|error| panic!("chat.active should succeed: {error}"));
        assert_eq!(active["active"], serde_json::json!(false));
    }

    #[tokio::test]
    async fn wait_for_session_gate_returns_immediately_when_inactive() {
        let (_directory, service, _metadata, _session_store, _resolved_efforts, _captured_messages) =
            validation_test_service().await;
        let terminal = service
            .wait_for_session_gate("session:idle")
            .await
            .unwrap_or_else(|error| panic!("inactive wait should succeed: {error}"));
        assert_eq!(terminal, None);
        assert_eq!(
            service
                .session_terminal("session:idle")
                .await
                .unwrap_or_else(|error| panic!("session terminal should succeed: {error}")),
            None
        );
    }

    #[tokio::test]
    async fn wait_for_session_gate_wakes_on_finish_without_a_run_id() {
        let (_directory, service, _metadata, _session_store, _resolved_efforts, _captured_messages) =
            validation_test_service().await;
        let session_key = "session:child";
        service.session_gates.begin_turn(session_key).await;
        service
            .active_runs_by_session
            .write()
            .await
            .insert(session_key.to_string(), "run-internal".to_string());
        let waiter = {
            let service = service.clone();
            tokio::spawn(async move { service.wait_for_session_gate(session_key).await })
        };
        tokio::task::yield_now().await;
        service
            .session_gates
            .finish_turn(session_key, SessionTerminal::Cancelled)
            .await;
        service
            .active_runs_by_session
            .write()
            .await
            .remove(session_key);
        let terminal = waiter
            .await
            .unwrap_or_else(|error| panic!("wait task: {error}"))
            .unwrap_or_else(|error| panic!("session gate wait should succeed: {error}"));
        assert_eq!(terminal, Some(SessionTerminal::Cancelled));
        assert_eq!(
            service
                .session_terminal(session_key)
                .await
                .unwrap_or_else(|error| panic!("session terminal should succeed: {error}")),
            Some(SessionTerminal::Cancelled)
        );
    }

    #[tokio::test]
    async fn begin_turn_clears_last_terminal_for_the_next_send_sync_insert() {
        let (_directory, service, _metadata, _session_store, _resolved_efforts, _captured_messages) =
            validation_test_service().await;
        let session_key = "session:child";
        service
            .session_gates
            .finish_turn(session_key, SessionTerminal::Completed)
            .await;
        service.session_gates.begin_turn(session_key).await;
        assert_eq!(
            service
                .session_terminal(session_key)
                .await
                .unwrap_or_else(|error| panic!("session terminal should succeed: {error}")),
            None
        );
    }

    #[tokio::test]
    async fn activate_session_turn_clears_terminal_before_session_is_active() {
        let (_directory, service, _metadata, _session_store, _resolved_efforts, _captured_messages) =
            validation_test_service().await;
        let session_key = "session:child";
        service
            .session_gates
            .finish_turn(session_key, SessionTerminal::Completed)
            .await;
        service
            .activate_session_turn(session_key, "run-internal")
            .await;
        assert_eq!(
            service
                .session_terminal(session_key)
                .await
                .unwrap_or_else(|error| panic!("session terminal should succeed: {error}")),
            None
        );
        assert_eq!(
            service
                .active_runs_by_session
                .read()
                .await
                .get(session_key)
                .map(String::as_str),
            Some("run-internal")
        );
    }

    fn queued_text(text: &str) -> chelix_sessions::QueuedPromptContent {
        chelix_sessions::QueuedPromptContent {
            content: chelix_sessions::QueuedPromptMessageContent::Text(text.to_string()),
            documents: Vec::new(),
            audio: None,
            client_sequence: None,
            client_message_id: None,
            input_medium: chelix_common::MessageMedium::Text,
            reply_medium: chelix_common::MessageMedium::Text,
            channel: None,
            channel_reply_target: None,
        }
    }

    async fn publish_stop_run(
        service: &LiveChatService,
        session_key: &str,
        run_id: &str,
        token: CancellationToken,
    ) {
        let mut guard = service
            .stop_gate
            .begin_send(session_key)
            .unwrap_or_else(|error| panic!("begin send: {error}"));
        let mut active = service.active_runs.write().await;
        service
            .stop_gate
            .publish_run(&mut active, &mut guard, session_key, run_id, token);
    }

    async fn finish_stop_run(service: &LiveChatService, run_id: &str) {
        let mut active = service.active_runs.write().await;
        service.stop_gate.finish_run(&mut active, run_id, Ok(()));
    }

    #[tokio::test]
    async fn stop_run_returns_after_the_run_finishes_not_at_cancel() {
        let (_directory, service, _metadata, _session_store, _resolved_efforts, _captured_messages) =
            validation_test_service().await;
        let token = CancellationToken::new();
        publish_stop_run(&service, "main", "run-1", token.clone()).await;
        let stopping = service.clone();
        let stop = tokio::spawn(async move { stopping.stop_run("run-1".to_string()).await });
        token.cancelled().await;
        assert!(!stop.is_finished(), "stop returned at token cancellation");
        finish_stop_run(&service, "run-1").await;
        let outcome = stop
            .await
            .unwrap_or_else(|error| panic!("stop task: {error}"))
            .unwrap_or_else(|error| panic!("stop run: {error}"));
        assert!(outcome.cancelled);
        assert_eq!(outcome.run_id.as_deref(), Some("run-1"));
    }

    #[tokio::test]
    async fn stop_run_waits_when_the_token_was_already_cancelled() {
        let (_directory, service, _metadata, _session_store, _resolved_efforts, _captured_messages) =
            validation_test_service().await;
        let token = CancellationToken::new();
        publish_stop_run(&service, "main", "run-1", token.clone()).await;
        token.cancel();
        let stopping = service.clone();
        let stop = tokio::spawn(async move { stopping.stop_run("run-1".to_string()).await });
        for _ in 0..20 {
            tokio::task::yield_now().await;
        }
        assert!(
            !stop.is_finished(),
            "stop returned before the already-cancelled run finished"
        );
        finish_stop_run(&service, "run-1").await;
        let outcome = stop
            .await
            .unwrap_or_else(|error| panic!("stop task: {error}"))
            .unwrap_or_else(|error| panic!("stop run: {error}"));
        assert!(!outcome.cancelled);
        assert_eq!(outcome.run_id.as_deref(), Some("run-1"));
    }

    #[tokio::test]
    async fn stop_before_publish_prevents_the_provider_call() {
        let (
            _directory,
            mut service,
            _metadata,
            _session_store,
            _resolved_efforts,
            captured_messages,
        ) = validation_test_service().await;
        let gate = Arc::new(super::super::types::TestGate {
            arrived: tokio::sync::Notify::new(),
            release: tokio::sync::Notify::new(),
        });
        service.before_publish_run = Some(Arc::clone(&gate));
        let sending = service.clone();
        let send = tokio::spawn(async move {
            sending
                .send(
                    ChatSendRequest::text("hello"),
                    ChatExecutionContext::internal(SessionKey::new("main")),
                )
                .await
        });
        gate.arrived.notified().await;
        assert!(service.active_runs.read().await.is_empty());
        let stopping = service.clone();
        let stop =
            tokio::spawn(async move { stopping.stop_current_session("main", None, false).await });
        for _ in 0..100 {
            if service.stop_gate.is_suppressed("main") {
                break;
            }
            tokio::task::yield_now().await;
        }
        assert!(service.stop_gate.is_suppressed("main"));
        gate.release.notify_one();
        let send_result = send
            .await
            .unwrap_or_else(|error| panic!("send task: {error}"));
        assert!(send_result.is_err());
        let _ = stop
            .await
            .unwrap_or_else(|error| panic!("stop task: {error}"))
            .unwrap_or_else(|error| panic!("stop session: {error}"));
        assert!(
            captured_messages
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .is_empty()
        );
    }

    #[tokio::test]
    async fn drained_batch_does_not_start_or_return_to_the_queue() {
        let (
            _directory,
            mut service,
            _metadata,
            _session_store,
            _resolved_efforts,
            captured_messages,
        ) = validation_test_service().await;
        let gate = Arc::new(super::super::types::TestGate {
            arrived: tokio::sync::Notify::new(),
            release: tokio::sync::Notify::new(),
        });
        service.queue_after_drain = Some(Arc::clone(&gate));
        service
            .queued_prompts
            .enqueue(SessionKey::new("main"), queued_text("later"))
            .await
            .unwrap_or_else(|error| panic!("enqueue: {error}"));
        service
            .send(
                ChatSendRequest::text("now"),
                ChatExecutionContext::internal(SessionKey::new("main")),
            )
            .await
            .unwrap_or_else(|error| panic!("send: {error}"));
        gate.arrived.notified().await;
        assert_eq!(
            captured_messages
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .len(),
            1
        );
        let stopping = service.clone();
        let stop =
            tokio::spawn(async move { stopping.stop_current_session("main", None, false).await });
        for _ in 0..100 {
            if service.stop_gate.is_suppressed("main") {
                break;
            }
            tokio::task::yield_now().await;
        }
        assert!(service.stop_gate.is_suppressed("main"));
        gate.release.notify_one();
        let _ = stop
            .await
            .unwrap_or_else(|error| panic!("stop task: {error}"))
            .unwrap_or_else(|error| panic!("stop session: {error}"));
        assert_eq!(
            captured_messages
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .len(),
            1
        );
        let status = service
            .queued_prompts
            .status(SessionKey::new("main"))
            .await
            .unwrap_or_else(|error| panic!("status: {error}"));
        assert!(status.prompts.is_empty());
    }

    #[tokio::test]
    async fn stop_session_broadcasts_an_empty_queue_for_the_open_session() {
        let (_directory, service, _metadata, _session_store, _resolved_efforts, _captured_messages) =
            validation_test_service().await;
        service
            .queued_prompts
            .enqueue(SessionKey::new("main"), queued_text("later"))
            .await
            .unwrap_or_else(|error| panic!("enqueue: {error}"));
        let _ = service
            .stop_current_session("main", None, false)
            .await
            .unwrap_or_else(|error| panic!("stop session: {error}"));
        let events = service
            .test_broadcasts
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone();
        assert!(events.iter().any(|(topic, payload)| {
            topic == "chat"
                && payload["state"] == "prompt_queue"
                && payload["status"]["sessionKey"] == "main"
                && payload["status"]["prompts"]
                    .as_array()
                    .is_some_and(|prompts| prompts.is_empty())
        }));
        let status = service
            .queued_prompts
            .status(SessionKey::new("main"))
            .await
            .unwrap_or_else(|error| panic!("status: {error}"));
        assert!(status.prompts.is_empty());
    }

    #[tokio::test]
    async fn stop_run_leaves_the_other_run_and_the_queue() {
        let (_directory, service, _metadata, _session_store, _resolved_efforts, _captured_messages) =
            validation_test_service().await;
        let token_a = CancellationToken::new();
        let token_b = CancellationToken::new();
        publish_stop_run(&service, "main", "run-a", token_a.clone()).await;
        publish_stop_run(&service, "main", "run-b", token_b.clone()).await;
        service
            .active_runs_by_session
            .write()
            .await
            .insert("main".to_string(), "run-b".to_string());
        service
            .queued_prompts
            .enqueue(SessionKey::new("main"), queued_text("keep"))
            .await
            .unwrap_or_else(|error| panic!("enqueue: {error}"));
        let stopping = service.clone();
        let stop = tokio::spawn(async move { stopping.stop_run("run-a".to_string()).await });
        token_a.cancelled().await;
        assert!(!token_b.is_cancelled());
        finish_stop_run(&service, "run-a").await;
        let outcome = stop
            .await
            .unwrap_or_else(|error| panic!("stop task: {error}"))
            .unwrap_or_else(|error| panic!("stop run: {error}"));
        assert!(outcome.cancelled);
        assert!(!token_b.is_cancelled());
        let status = service
            .queued_prompts
            .status(SessionKey::new("main"))
            .await
            .unwrap_or_else(|error| panic!("status: {error}"));
        assert_eq!(status.prompts.len(), 1);
    }

    #[tokio::test]
    async fn stop_run_without_a_token_reports_the_requested_run() {
        let (_directory, service, _metadata, _session_store, _resolved_efforts, _captured_messages) =
            validation_test_service().await;
        let outcome = service
            .stop_run("missing".to_string())
            .await
            .unwrap_or_else(|error| panic!("stop run: {error}"));
        assert!(!outcome.cancelled);
        assert_eq!(outcome.run_id.as_deref(), Some("missing"));
    }

    #[tokio::test]
    async fn stale_session_run_does_not_cancel_the_new_run_or_clear_the_queue() {
        let (_directory, service, _metadata, _session_store, _resolved_efforts, _captured_messages) =
            validation_test_service().await;
        let token_old = CancellationToken::new();
        let token_new = CancellationToken::new();
        publish_stop_run(&service, "main", "run-old", token_old).await;
        publish_stop_run(&service, "main", "run-new", token_new.clone()).await;
        service
            .queued_prompts
            .enqueue(SessionKey::new("main"), queued_text("keep"))
            .await
            .unwrap_or_else(|error| panic!("enqueue: {error}"));
        let outcome = service
            .stop_current_session("main", Some("run-old"), false)
            .await
            .unwrap_or_else(|error| panic!("stop session: {error}"));
        assert!(!outcome.cancelled);
        assert_eq!(outcome.run_id.as_deref(), Some("run-old"));
        assert!(!token_new.is_cancelled());
        let status = service
            .queued_prompts
            .status(SessionKey::new("main"))
            .await
            .unwrap_or_else(|error| panic!("status: {error}"));
        assert_eq!(status.prompts.len(), 1);
    }

    #[tokio::test]
    async fn dropped_run_finish_guard_wakes_the_waiting_stop() {
        let (_directory, service, _metadata, _session_store, _resolved_efforts, _captured_messages) =
            validation_test_service().await;
        let token = CancellationToken::new();
        publish_stop_run(&service, "main", "run-1", token.clone()).await;
        service
            .active_runs_by_session
            .write()
            .await
            .insert("main".to_string(), "run-1".to_string());
        let guard = super::super::stop_gate::RunFinishGuard::arm(
            Arc::clone(&service.stop_gate),
            Arc::clone(&service.session_gates),
            Arc::clone(&service.active_runs),
            Arc::clone(&service.active_runs_by_session),
            "main".to_string(),
            "run-1".to_string(),
        );
        let stopping = service.clone();
        let stop = tokio::spawn(async move { stopping.stop_run("run-1".to_string()).await });
        token.cancelled().await;
        drop(guard);
        let error = match stop
            .await
            .unwrap_or_else(|error| panic!("stop task: {error}"))
        {
            Err(error) => error,
            Ok(outcome) => panic!("expected drop error, cancelled={}", outcome.cancelled),
        };
        assert!(error.to_string().contains("run dropped before completion"));
        let outcome = service
            .stop_run("run-1".to_string())
            .await
            .unwrap_or_else(|error| panic!("second stop: {error}"));
        assert!(!outcome.cancelled);
        service
            .stop_current_session("main", None, false)
            .await
            .unwrap_or_else(|error| panic!("stop session: {error}"));
    }

    #[tokio::test]
    async fn dropped_send_sync_wakes_the_waiting_stop() {
        let (
            _directory,
            mut service,
            _metadata,
            _session_store,
            _resolved_efforts,
            _captured_messages,
        ) = validation_test_service().await;
        let gate = Arc::new(super::super::types::TestGate {
            arrived: tokio::sync::Notify::new(),
            release: tokio::sync::Notify::new(),
        });
        service.after_publish_run = Some(Arc::clone(&gate));
        let sending = service.clone();
        let send = tokio::spawn(async move {
            sending
                .send_sync(
                    ChatSendSyncRequest::text("hello"),
                    ChatExecutionContext::internal(SessionKey::new("main")),
                )
                .await
        });
        gate.arrived.notified().await;
        let run_id = service
            .active_runs
            .read()
            .await
            .keys()
            .next()
            .cloned()
            .unwrap_or_else(|| panic!("run was not published"));
        let token = service
            .active_runs
            .read()
            .await
            .get(&run_id)
            .cloned()
            .unwrap_or_else(|| panic!("run token missing"));
        let stopping = service.clone();
        let run_for_stop = run_id.clone();
        let stop = tokio::spawn(async move { stopping.stop_run(run_for_stop).await });
        token.cancelled().await;
        send.abort();
        let error = match stop
            .await
            .unwrap_or_else(|error| panic!("stop task: {error}"))
        {
            Err(error) => error,
            Ok(outcome) => panic!("expected drop error, cancelled={}", outcome.cancelled),
        };
        assert!(error.to_string().contains("run dropped before completion"));
        let outcome = service
            .stop_run(run_id)
            .await
            .unwrap_or_else(|error| panic!("second stop: {error}"));
        assert!(!outcome.cancelled);
        service
            .stop_current_session("main", None, false)
            .await
            .unwrap_or_else(|error| panic!("stop session: {error}"));
        gate.release.notify_one();
    }

    #[tokio::test]
    async fn session_stop_keeps_a_finished_run_error_for_every_waiter() {
        let (_directory, service, _metadata, _session_store, _resolved_efforts, _captured_messages) =
            validation_test_service().await;
        let token = CancellationToken::new();
        publish_stop_run(&service, "main", "run-1", token).await;
        let mut mapping = service.active_runs_by_session.write().await;
        mapping.insert("main".to_string(), "run-1".to_string());
        let first = service.clone();
        let second = service.clone();
        let left =
            tokio::spawn(async move { first.stop_current_session("main", None, false).await });
        let right =
            tokio::spawn(async move { second.stop_current_session("main", None, false).await });
        for _ in 0..100 {
            if service.stop_gate.stop_depth("main") == 2 {
                break;
            }
            tokio::task::yield_now().await;
        }
        assert_eq!(service.stop_gate.stop_depth("main"), 2);
        {
            let mut active = service.active_runs.write().await;
            service
                .stop_gate
                .finish_run(&mut active, "run-1", Err("save failed".to_string()));
        }
        mapping.remove("main");
        drop(mapping);
        let left = left
            .await
            .unwrap_or_else(|error| panic!("left task: {error}"));
        let right = right
            .await
            .unwrap_or_else(|error| panic!("right task: {error}"));
        match left {
            Err(error) => assert!(error.to_string().contains("save failed")),
            Ok(outcome) => panic!("left stop succeeded, cancelled={}", outcome.cancelled),
        }
        match right {
            Err(error) => assert!(error.to_string().contains("save failed")),
            Ok(outcome) => panic!("right stop succeeded, cancelled={}", outcome.cancelled),
        }
        service
            .stop_current_session("main", None, false)
            .await
            .unwrap_or_else(|error| panic!("later stop: {error}"));
    }
}
