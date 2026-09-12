//! `ChatService` trait implementation for `LiveChatService`.

use std::{future::Future, pin::Pin, sync::Arc, time::Duration};

use {
    serde_json::Value,
    tokio_util::sync::CancellationToken,
    tracing::{debug, info, warn},
};

use {
    chelix_common::ModelOverride,
    chelix_providers::ResolvedModel,
    chelix_service_traits::{
        ChatChannelMetadata, ChatExecutionContext, ChatRequestOrigin, ChatSendRequest,
        ServiceError, ServiceResult, SessionBusyReason, SessionTurnPermit,
    },
    chelix_sessions::{QueuedPromptContent, SessionKey},
};

use crate::{
    channels::deliver_channel_error,
    chat_error::parse_chat_error,
    message::{
        apply_message_received_rewrite, chat_message_parts, to_user_content,
        user_documents_for_persistence,
    },
    prompt::{
        apply_chat_execution_context, build_prompt_runtime_context, discover_skills_if_enabled,
        filter_skills_for_agent, resolve_channel_runtime_context, validate_prompt_agent_id,
    },
    prompt_queue::{
        broadcast_queued_prompts_status, normalize_queued_prompt_content,
        queued_channel_reply_target, queued_channel_value, queued_documents,
        queued_message_content, queued_message_text, queued_reply_medium, queued_sender_name,
    },
    run_with_tools::run_with_tools,
    streaming::run_streaming,
    types::*,
};

use super::{super::session_gate::terminal_from_outcome, *};

use {
    crate::memory_tools::{AgentScopedMemoryWriter, MemoryForgetProviderResolver},
    chelix_agents::{ChatMessage, model::values_to_chat_messages},
};

struct PreparedUserBatchPrefix {
    messages: Vec<ChatMessage>,
    records: Vec<Value>,
}

struct SessionTurnOwner {
    service: LiveChatService,
    _permit: SessionTurnPermit,
}

pub(super) struct ResolvedChatTurn {
    pub(super) model: ResolvedModel,
    pub(super) stream_only: bool,
}

impl SessionTurnOwner {
    fn start_queued_batch(
        self,
        session_id: SessionKey,
        mut prompts: Vec<QueuedPromptContent>,
    ) -> Pin<Box<dyn Future<Output = Result<(), ServiceError>> + Send>> {
        Box::pin(async move {
            let tail = prompts
                .pop()
                .ok_or_else(|| ServiceError::message("queued prompt batch must not be empty"))?;
            let service = self.service.clone();
            let resolved = service.resolve_chat_turn(&session_id, None).await?;
            let context = queued_execution_context(session_id.clone(), &tail);
            service
                .start_turn_impl(
                    session_id, prompts, tail, context, None, true, resolved, self,
                )
                .await?;
            Ok(())
        })
    }
}

fn queued_execution_context(
    session_id: SessionKey,
    prompt: &QueuedPromptContent,
) -> ChatExecutionContext {
    let mut context = ChatExecutionContext::internal(session_id);
    context.channel = prompt.channel.as_ref().map(|metadata| ChatChannelMetadata {
        channel_type: metadata.channel_type,
        sender_name: metadata.sender_name.clone(),
        username: metadata.username.clone(),
        sender_id: metadata.sender_id.clone(),
        message_kind: metadata.message_kind,
    });
    context.channel_reply_target = prompt.channel_reply_target.clone();
    context
}

impl LiveChatService {
    pub(super) async fn resolve_chat_turn(
        &self,
        session_id: &SessionKey,
        request_override: Option<&ModelOverride>,
    ) -> Result<ResolvedChatTurn, ServiceError> {
        let selected = if let Some(request_override) = request_override {
            (
                request_override.model.clone(),
                request_override.reasoning_effort.clone(),
            )
        } else {
            let entry = self
                .session_metadata
                .get(session_id.as_str())
                .await
                .map_err(ServiceError::message)?
                .ok_or_else(|| {
                    ServiceError::message(format!(
                        "session '{}' has no model/reasoning pair",
                        session_id.as_str()
                    ))
                })?;
            let pair = entry.model_reasoning().ok_or_else(|| {
                ServiceError::message(format!(
                    "session '{}' has no model/reasoning pair",
                    session_id.as_str()
                ))
            })?;
            (pair.model_id().to_string(), pair.reasoning_effort().clone())
        };
        let model = {
            let registry = self.providers.read().await;
            registry
                .resolve_model_reasoning(Some(&selected.0), Some(&selected.1))
                .map_err(|error| ServiceError::message(error.to_string()))?
        };
        validate_tool_mode_compatibility(
            model.provider().tool_mode(),
            model.provider().supports_tools(),
            model.provider().id(),
        )
        .map_err(ServiceError::message)?;
        Ok(ResolvedChatTurn {
            model,
            stream_only: !self.has_tools_sync(),
        })
    }

    async fn persist_queued_turn_settings(
        &self,
        session_key: &str,
        model_reasoning: &chelix_common::ResolvedModelReasoning,
        persist_request_override: bool,
        requested_agent_id: Option<&str>,
    ) -> Result<(), ServiceError> {
        if !persist_request_override && requested_agent_id.is_none() {
            return Ok(());
        }
        let entry = self
            .session_metadata
            .get(session_key)
            .await
            .map_err(ServiceError::message)?
            .ok_or_else(|| ServiceError::message(format!("session '{session_key}' not found")))?;
        if let Some(agent_id) = requested_agent_id
            && entry
                .agent_id
                .as_deref()
                .is_none_or(|value| value.trim().is_empty())
        {
            if entry.model_reasoning().is_none() {
                self.session_metadata
                    .promote_external_to_llm(session_key, model_reasoning, agent_id)
                    .await
                    .map_err(ServiceError::message)?;
            } else {
                self.session_metadata
                    .assign_agent(session_key, agent_id, model_reasoning)
                    .await
                    .map_err(ServiceError::message)?;
            }
            return Ok(());
        }
        if persist_request_override {
            self.session_metadata
                .set_model_reasoning(session_key, model_reasoning)
                .await
                .map_err(ServiceError::message)?;
        }
        Ok(())
    }

    fn build_user_batch_prefix(
        &self,
        session_id: &SessionKey,
        prompts: Vec<QueuedPromptContent>,
        run_id: &str,
    ) -> Result<PreparedUserBatchPrefix, ServiceError> {
        let mut leading = PreparedUserBatchPrefix {
            messages: Vec::with_capacity(prompts.len()),
            records: Vec::with_capacity(prompts.len()),
        };

        for prompt in prompts {
            let message_content = queued_message_content(&prompt);
            let documents = queued_documents(&prompt, session_id, self.session_store.as_ref());
            let user_content = to_user_content(&message_content, &documents);
            let user_msg = PersistedMessage::User {
                content: message_content,
                created_at: Some(now_ms()),
                audio: prompt.audio.clone(),
                documents: user_documents_for_persistence(&documents),
                channel: queued_channel_value(&prompt)
                    .map_err(|error| ServiceError::message(error.to_string()))?,
                seq: prompt.client_sequence,
                run_id: Some(run_id.to_string()),
            };
            let mut record = user_msg.to_value();
            if let Some(id) = &prompt.client_message_id {
                record["clientMessageId"] = serde_json::json!(id);
            }
            leading.records.push(record);
            leading.messages.push(ChatMessage::User {
                content: user_content,
                name: None,
            });
        }

        Ok(leading)
    }

    async fn web_channel_reply_target(
        &self,
        context: &ChatExecutionContext,
    ) -> Result<Option<chelix_channels::ChannelReplyTarget>, ServiceError> {
        if !matches!(&context.origin, ChatRequestOrigin::Client { .. }) || context.channel.is_some()
        {
            return Ok(None);
        }
        let session_key = context.session_id.as_str();
        let Some(entry) = self
            .session_metadata
            .get(session_key)
            .await
            .map_err(ServiceError::message)?
        else {
            return Ok(None);
        };
        let Some(binding_json) = entry.channel_binding.as_deref() else {
            return Ok(None);
        };
        let target = serde_json::from_str::<chelix_channels::ChannelReplyTarget>(binding_json)
            .map_err(|error| ServiceError::message(error.to_string()))?;
        let is_active = self
            .session_metadata
            .get_active_session(
                target.channel_type.as_str(),
                &target.account_id,
                &target.chat_id,
                target.thread_id.as_deref(),
            )
            .await
            .map_err(ServiceError::message)?
            .is_none_or(|key| key == session_key);
        Ok(is_active.then_some(target))
    }

    #[tracing::instrument(skip(self, request, context), fields(session_id = %context.session_id))]
    pub(super) async fn send_impl(
        &self,
        request: ChatSendRequest,
        context: ChatExecutionContext,
    ) -> ServiceResult {
        let session_id = context.session_id.clone();
        let session_key = session_id.as_str().to_string();
        if session_key.is_empty() {
            return Err(ServiceError::message("session ID must not be empty"));
        }
        if let Some(agent_id) = context.agent_id.as_deref() {
            let runtime_config = self
                .load_runtime_config_for_agent_run()
                .await
                .map_err(ServiceError::message)?;
            validate_prompt_agent_id(&runtime_config, agent_id).map_err(ServiceError::message)?;
        }
        let (text, _) = chat_message_parts(&request.message).map_err(ServiceError::message)?;
        let mut content =
            normalize_queued_prompt_content(&request, &context, self.session_store.as_ref())
                .map_err(|error| ServiceError::message(error.to_string()))?;
        let resolved = self
            .resolve_chat_turn(&session_id, request.model_override.as_ref())
            .await?;
        if content.channel_reply_target.is_none()
            && let Some(target) = self.web_channel_reply_target(&context).await?
        {
            content.channel_reply_target = Some(target);
        }
        let client_seq = request.client_sequence;

        if let Some(seq) = client_seq {
            let mut seq_map = self.last_client_seq.write().await;
            let last = seq_map.entry(session_key.clone()).or_insert(0);
            if *last == 0 {
                debug!(session = %session_key, seq, "client seq initialized");
            } else if seq == 1 && *last > 1 {
                debug!(
                    session = %session_key,
                    prev_seq = *last,
                    "client seq reset (page reload)"
                );
            } else if seq <= *last {
                warn!(
                    session = %session_key,
                    seq,
                    last_seq = *last,
                    "client seq out of order (duplicate or reorder)"
                );
            } else if seq > *last + 1 {
                warn!(
                    session = %session_key,
                    seq,
                    last_seq = *last,
                    gap = seq - *last - 1,
                    "client seq gap detected (missing messages)"
                );
            }
            *last = seq;
        }

        info!(
            session = %session_key,
            text_len = text.len(),
            has_content = matches!(
                &request.message,
                chelix_service_traits::ChatSendMessage::Content(_)
            ),
            model = resolved.model.model_reasoning().model_id(),
            client_seq = ?client_seq,
            "chat.send: received"
        );

        let permit = match self.session_mutations.try_acquire_turn(&session_key).await {
            Ok(permit) => {
                info!(
                    session = %session_key,
                    client_seq = ?client_seq,
                    "chat.send: acquired session permit"
                );
                permit
            },
            Err(error) if error.reason() == SessionBusyReason::ReservedMutation => {
                info!(
                    session = %session_key,
                    client_seq = ?client_seq,
                    "chat.send: rejected because session mutation is in progress"
                );
                return Err(ServiceError::message(
                    "Session history is being updated; please try again.",
                ));
            },
            Err(_) => {
                self.persist_queued_turn_settings(
                    &session_key,
                    resolved.model.model_reasoning(),
                    request.model_override.is_some(),
                    context.agent_id.as_deref(),
                )
                .await?;
                let status = self
                    .queued_prompts
                    .enqueue(session_id, content)
                    .await
                    .map_err(|error| ServiceError::message(error.to_string()))?;
                broadcast_queued_prompts_status(&self.state, &status)
                    .await
                    .map_err(|error| ServiceError::message(error.to_string()))?;
                info!(
                    session = %session_key,
                    queued = status.prompts.len(),
                    client_seq = ?client_seq,
                    "chat.send: queued because session is active"
                );
                return Ok(serde_json::json!({
                    "ok": true,
                    "queued": true,
                    "status": status,
                }));
            },
        };

        let owner = SessionTurnOwner {
            service: self.clone(),
            _permit: permit,
        };
        self.start_turn_impl(
            session_id,
            Vec::new(),
            content,
            context,
            request.tool_choice,
            false,
            resolved,
            owner,
        )
        .await
    }

    #[allow(clippy::too_many_arguments)]
    async fn start_turn_impl(
        &self,
        session_id: SessionKey,
        queued_leading_prompts: Vec<QueuedPromptContent>,
        prompt: QueuedPromptContent,
        context: ChatExecutionContext,
        tool_choice: Option<chelix_config::schema::ToolChoice>,
        queued_batch: bool,
        resolved: ResolvedChatTurn,
        owner: SessionTurnOwner,
    ) -> ServiceResult {
        let session_key = session_id.as_str().to_string();
        let mut text = queued_message_text(&prompt);
        let mut message_content = queued_message_content(&prompt);
        let desired_reply_medium = queued_reply_medium(&prompt);
        let conn_id = context.connection_id().map(str::to_string);
        let client_seq = prompt.client_sequence;
        let client_message_id = prompt.client_message_id.clone();
        let stream_only = resolved.stream_only;
        let provider = Arc::clone(resolved.model.provider());
        tracing::debug!(stream_only, queued_batch, "send() mode decision");

        let mut session_entry = self
            .session_metadata
            .get(&session_key)
            .await
            .map_err(ServiceError::message)?;
        info!(
            session = %session_key,
            provider = provider.name(),
            model = provider.id(),
            stream_only,
            client_seq = ?client_seq,
            "chat.send: provider resolved"
        );

        // Resolve project context for this connection's active project.
        let project_context = self
            .resolve_project_context(&session_key, conn_id.as_deref())
            .await
            .map_err(ServiceError::message)?;

        // Generate run_id early so we can link the user message to its agent run.
        let run_id = uuid::Uuid::new_v4().to_string();

        // Load conversation history (the current user message is NOT yet
        // persisted — run_streaming / run_agent_loop add it themselves).
        let history = self
            .session_store
            .read(&session_key)
            .await
            .map_err(ServiceError::message)?;
        info!(
            session = %session_key,
            history_len = history.len(),
            client_seq = ?client_seq,
            "chat.send: history loaded"
        );
        let chat_history = values_to_chat_messages(&history).map_err(ServiceError::message)?;
        let deferred_channel_target = queued_channel_reply_target(&prompt);

        // Dispatch the `MessageReceived` hook before the turn starts. The
        // hook can:
        //   - return `Continue` → proceed normally;
        //   - return `ModifyPayload({"content": "..."})` → rewrite the
        //     inbound text before it is persisted or sent to the model;
        //   - return `Block(reason)` → abort this turn entirely. The user
        //     message is NOT persisted, no run is started, and the reason
        //     is surfaced to the channel/web sender.
        //
        // Hook errors are treated as fail-open: a broken hook must not be
        // able to wedge every inbound message. See GH #639.
        // Drained content is already canonical batch input and must remain unchanged.
        if !queued_batch && let Some(ref hooks) = self.hook_registry {
            info!(
                session = %session_key,
                client_seq = ?client_seq,
                "chat.send: dispatching MessageReceived hook"
            );
            let channel = context
                .channel
                .as_ref()
                .map(|channel| channel.channel_type.as_str().to_string());
            let channel_binding = Some(resolve_channel_runtime_context(
                &session_key,
                session_entry.as_ref(),
            ))
            .filter(|binding| !binding.is_empty());
            let payload = chelix_common::hooks::HookPayload::MessageReceived {
                session_key: session_key.clone(),
                content: text.clone(),
                channel,
                channel_binding,
            };
            match hooks.dispatch(&payload).await {
                Ok(chelix_common::hooks::HookAction::Continue) => {},
                Ok(chelix_common::hooks::HookAction::ModifyPayload(new_payload)) => {
                    match new_payload.get("content").and_then(|v| v.as_str()) {
                        Some(new_text) => {
                            info!(
                                session = %session_key,
                                "MessageReceived hook rewrote inbound content"
                            );
                            text = new_text.to_string();
                            apply_message_received_rewrite(&mut message_content, new_text);
                        },
                        None => {
                            warn!(
                                session = %session_key,
                                "MessageReceived hook ModifyPayload ignored: expected object with `content` string"
                            );
                        },
                    }
                },
                Ok(chelix_common::hooks::HookAction::Block(reason)) => {
                    info!(
                        session = %session_key,
                        reason = %reason,
                        "MessageReceived hook blocked inbound message"
                    );

                    // Surface the rejection to channel senders via the
                    // existing channel-error delivery path. If the caller
                    // attached a reply target (web-UI-on-bound-session or an
                    // inbound channel message), re-register it so
                    // `deliver_channel_error` has a destination to drain.
                    if let Some(target) = deferred_channel_target.clone() {
                        self.state.push_channel_reply(&session_key, target).await;
                        let error_obj = serde_json::json!({
                            "type": "message_rejected",
                            "message": reason,
                        });
                        deliver_channel_error(&self.state, &session_key, &error_obj).await;
                    }

                    // Broadcast a rejection event so web UI clients see it.
                    broadcast(
                        &self.state,
                        "chat",
                        serde_json::json!({
                            "state": "rejected",
                            "sessionKey": session_key,
                            "reason": reason,
                        }),
                        BroadcastOpts::default(),
                    )
                    .await;

                    return Ok(serde_json::json!({
                        "ok": false,
                        "rejected": true,
                        "reason": reason,
                    }));
                },
                Err(e) => {
                    warn!(
                        session = %session_key,
                        error = %e,
                        "MessageReceived hook failed; proceeding fail-open"
                    );
                },
            }
            info!(
                session = %session_key,
                client_seq = ?client_seq,
                "chat.send: MessageReceived hook complete"
            );
        }

        let user_documents = queued_documents(&prompt, &session_id, self.session_store.as_ref());
        let user_content = to_user_content(&message_content, &user_documents);
        let channel_meta = queued_channel_value(&prompt)
            .map_err(|error| ServiceError::message(error.to_string()))?;
        let sender_name = queued_sender_name(&prompt);
        let user_audio = prompt.audio.clone();
        let user_msg = PersistedMessage::User {
            content: message_content,
            created_at: Some(now_ms()),
            audio: user_audio,
            documents: user_documents_for_persistence(&user_documents),
            channel: channel_meta,
            seq: client_seq,
            run_id: Some(run_id.clone()),
        };

        // Load one live agent-registry snapshot for the whole run.
        let persona = self
            .load_prompt_persona_for_agent_run(
                &session_key,
                session_entry.as_ref(),
                context.agent_id.as_deref(),
            )
            .await
            .map_err(ServiceError::message)?;
        let session_agent_id = persona.agent_id.clone();

        // Discover enabled skills/plugins and apply the live per-agent policy.
        let discovered_skills = discover_skills_if_enabled(&persona.config).await;
        let discovered_skills = filter_skills_for_agent(discovered_skills, &persona.agent.skills);
        info!(
            session = %session_key,
            skills_len = discovered_skills.len(),
            agent_id = %session_agent_id,
            client_seq = ?client_seq,
            "chat.send: skills discovered"
        );
        info!(
            session = %session_key,
            agent_id = %session_agent_id,
            client_seq = ?client_seq,
            "chat.send: persona loaded"
        );
        let model_reasoning = resolved.model.model_reasoning().clone();
        let resolved_reasoning_effort =
            Some(model_reasoning.reasoning_effort().as_str().to_string());
        session_entry = Some(match session_entry {
            Some(entry) if entry.model_reasoning().is_none() => self
                .session_metadata
                .promote_external_to_llm(&session_key, &model_reasoning, &session_agent_id)
                .await
                .map_err(ServiceError::message)?
                .into_entry(),
            Some(entry)
                if context.agent_id.is_some()
                    && entry
                        .agent_id
                        .as_deref()
                        .is_none_or(|value| value.trim().is_empty()) =>
            {
                self.session_metadata
                    .assign_agent(&session_key, &session_agent_id, &model_reasoning)
                    .await
                    .map_err(ServiceError::message)?
            },
            Some(entry)
                if entry.model() != Some(model_reasoning.model_id())
                    || entry.reasoning_effort() != Some(model_reasoning.reasoning_effort()) =>
            {
                self.session_metadata
                    .set_model_reasoning(&session_key, &model_reasoning)
                    .await
                    .map_err(ServiceError::message)?
            },
            Some(entry) => entry,
            None => match self
                .session_metadata
                .ensure_llm_session(
                    &session_key,
                    None,
                    &model_reasoning,
                    Some(&session_agent_id),
                )
                .await
                .map_err(ServiceError::message)?
            {
                chelix_sessions::metadata::EnsureLlmSessionOutcome::Created(entry) => entry,
                chelix_sessions::metadata::EnsureLlmSessionOutcome::ExistingLlm(entry) => {
                    if entry
                        .agent_id
                        .as_deref()
                        .is_some_and(|id| !id.trim().is_empty())
                    {
                        entry
                    } else {
                        let persisted_model_reasoning =
                            entry.model_reasoning().cloned().ok_or_else(|| {
                                ServiceError::message(format!(
                                    "session '{session_key}' has no model/reasoning pair"
                                ))
                            })?;
                        self.session_metadata
                            .assign_agent(
                                &session_key,
                                &session_agent_id,
                                &persisted_model_reasoning,
                            )
                            .await
                            .map_err(ServiceError::message)?
                    }
                },
                chelix_sessions::metadata::EnsureLlmSessionOutcome::ExistingExternal(_) => self
                    .session_metadata
                    .promote_external_to_llm(&session_key, &model_reasoning, &session_agent_id)
                    .await
                    .map_err(ServiceError::message)?
                    .into_entry(),
            },
        });
        let ui_message_count = self
            .session_store
            .ui_message_count(&session_key)
            .await
            .map_err(ServiceError::message)?;
        self.session_metadata
            .touch(&session_key, ui_message_count)
            .await
            .map_err(ServiceError::message)?;

        let runtime_limits = persona.config.agent_runtime_limits(&session_agent_id);
        match &runtime_limits {
            Ok(limits) => info!(
                session = %session_key,
                agent_id = %session_agent_id,
                timeout_secs = limits.timeout_secs,
                timeout_source = limits.timeout_source.as_str(),
                max_tools_threshold = limits.max_tools_threshold,
                client_seq = ?client_seq,
                "chat.send: persona loaded"
            ),
            Err(error) => warn!(
                session = %session_key,
                agent_id = %session_agent_id,
                client_seq = ?client_seq,
                error = %error,
                "chat.send: failed to resolve agent runtime limits"
            ),
        }
        let mcp_disabled = session_entry
            .as_ref()
            .and_then(|entry| entry.mcp_disabled)
            .unwrap_or(false);
        info!(
            session = %session_key,
            client_seq = ?client_seq,
            "chat.send: building runtime context"
        );
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
        info!(
            session = %session_key,
            agent_id = %session_agent_id,
            mcp_disabled,
            has_project_context = project_context.is_some(),
            client_seq = ?client_seq,
            "chat.send: runtime context built"
        );

        let state = Arc::clone(&self.state);
        let active_runs = Arc::clone(&self.active_runs);
        let active_runs_by_session = Arc::clone(&self.active_runs_by_session);
        let session_gates = Arc::clone(&self.session_gates);
        let active_tool_invocations = Arc::clone(&self.active_tool_invocations);
        let active_partial_assistant = Arc::clone(&self.active_partial_assistant);
        let active_reply_medium = Arc::clone(&self.active_reply_medium);
        let run_id_clone = run_id.clone();
        let tool_registry = if let Some(policy) = context.tool_policy.as_ref() {
            let registry_guard = self.tool_registry.read().await;
            Arc::new(RwLock::new(
                registry_guard.clone_allowed_by(|name| policy.is_allowed(name)),
            ))
        } else {
            Arc::clone(&self.tool_registry)
        };
        let hook_registry = self.hook_registry.clone();

        info!(
            run_id = %run_id,
            user_message = %text,
            model = provider.id(),
            stream_only,
            session = %session_key,
            reply_medium = ?desired_reply_medium,
            client_seq = ?client_seq,
            "chat.send"
        );

        let provider_name = provider.name().to_string();
        let model_id = provider.id().to_string();
        let session_store = Arc::clone(&self.session_store);
        let session_metadata = Arc::clone(&self.session_metadata);
        let session_agent_id_clone = session_agent_id.clone();
        let session_key_clone = session_key.clone();
        let accept_language = context.accept_language.clone();
        let mut chat_history = chat_history;
        let PreparedUserBatchPrefix {
            messages,
            records: leading_records,
        } = self.build_user_batch_prefix(&session_id, queued_leading_prompts, &run_id)?;
        chat_history.extend(messages);

        let mut records = leading_records;
        let mut user_record = user_msg.to_value();
        if let Some(id) = &client_message_id {
            user_record["clientMessageId"] = serde_json::json!(id);
        }
        records.push(user_record);
        let mut reminder_history = history.clone();
        reminder_history.extend(records.iter().cloned());
        let compaction_reminder = crate::compaction_reminder::CompactionReminder::from_history(
            persona.agent.compaction_reminder,
            &reminder_history,
        )
        .map_err(ServiceError::message)?;
        self.session_store
            .append_batch_at_index(&session_key, &records, history.len())
            .await
            .map_err(ServiceError::message)?;

        // Set preview from the first user message if not already set.
        if let Err(error) = async {
            if let Some(entry) = self
                .session_metadata
                .get(&session_key)
                .await
                .map_err(ServiceError::message)?
                && entry.preview.is_none()
            {
                let preview_text = extract_preview_from_value(&user_msg.to_value());
                if let Some(preview) = preview_text {
                    self.session_metadata
                        .set_preview(&session_key, Some(&preview))
                        .await
                        .map_err(ServiceError::message)?;
                }
            }
            Ok(())
        }
        .await
        {
            self.session_gates.begin_turn(&session_key).await;
            return Err(error);
        }

        let runtime_limits = match runtime_limits {
            Ok(limits) => limits,
            Err(error) => {
                let payload = async {
                    if let Some(target) = deferred_channel_target.clone() {
                        self.state.push_channel_reply(&session_key, target).await;
                    }
                    let error_detail = error.to_string();
                    self.state
                        .set_run_error(&run_id, error_detail.clone())
                        .await;
                    let error_obj = parse_chat_error(&error_detail, Some(&provider_name));
                    deliver_channel_error(&self.state, &session_key, &error_obj).await;
                    let ui = self
                        .session_store
                        .ui_history
                        .session(&session_key)
                        .await
                        .map_err(ServiceError::message)?;
                    ui.record_error(chelix_sessions::ui_history_types::UiProviderError {
                        run_id: run_id.clone(),
                        segment_id: None,
                        created_at: now_ms(),
                        raw: error_detail,
                        details: error_obj,
                        retry_after_ms: None,
                    })
                    .map_err(ServiceError::message)?;
                    ui.flush().await.map_err(ServiceError::message)?;
                    let payload = serde_json::json!({"runId": run_id, "sessionKey": session_key, "state": "error"});
                    self.terminal_runs.write().await.insert(run_id.clone());
                    broadcast(&self.state, "chat", payload, BroadcastOpts::default()).await;
                    self.terminal_runs.write().await.remove(&run_id);
                    Ok(serde_json::json!({
                        "ok": true,
                        "runId": run_id,
                    }))
                }
                .await;
                self.session_gates.begin_turn(&session_key).await;
                return payload;
            },
        };

        let outer_agent_timeout_secs = if stream_only {
            runtime_limits.timeout_secs
        } else {
            0
        };

        let queued_prompts = Arc::clone(&self.queued_prompts);
        let memory_forget_provider_resolver = MemoryForgetProviderResolver::new(
            Arc::clone(&self.providers),
            Arc::clone(&self.session_metadata),
        );
        let active_event_forwarders = Arc::clone(&self.active_event_forwarders);
        let terminal_runs = Arc::clone(&self.terminal_runs);
        let tools_config_source = self.tools_config_source.clone();
        let cancellation_token = CancellationToken::new();
        self.active_runs
            .write()
            .await
            .insert(run_id.clone(), cancellation_token.clone());
        self.activate_session_turn(&session_key, &run_id).await;

        let _run_task = tokio::spawn(async move {
            let ctx_ref = project_context.as_deref();
            if let Some(target) = deferred_channel_target {
                state.push_channel_reply(&session_key_clone, target).await;
            }
            active_reply_medium
                .write()
                .await
                .insert(session_key_clone.clone(), desired_reply_medium);
            active_partial_assistant.write().await.insert(
                session_key_clone.clone(),
                ActiveAssistantDraft::new(
                    &run_id_clone,
                    &model_id,
                    &provider_name,
                    resolved_reasoning_effort.clone(),
                    client_seq,
                ),
            );
            if desired_reply_medium == ReplyMedium::Voice {
                broadcast(
                    &state,
                    "chat",
                    serde_json::json!({
                        "runId": run_id_clone,
                        "sessionKey": session_key_clone,
                        "state": "voice_pending",
                    }),
                    BroadcastOpts::default(),
                )
                .await;
            }
            // Clone the provider for potential periodic memory extraction
            // (the original Arc is moved into run_with_tools / run_streaming).
            let provider_for_extraction = Arc::clone(&provider);
            // Capture config values before persona is moved into the agent future.
            let auto_extract_interval = persona.config.memory.auto_extract_interval;
            let extraction_write_mode = persona.config.memory.agent_write_mode;
            let extraction_max_tools_threshold = runtime_limits.max_tools_threshold;
            let auto_title_enabled = persona.config.chat.auto_title;
            let agent_fut = async {
                if stream_only {
                    run_streaming(
                        persona,
                        compaction_reminder,
                        &cancellation_token,
                        &state,
                        &run_id_clone,
                        provider,
                        &model_id,
                        &user_content,
                        &provider_name,
                        &chat_history,
                        &session_key_clone,
                        &session_agent_id_clone,
                        resolved_reasoning_effort.clone(),
                        desired_reply_medium,
                        ctx_ref,
                        &discovered_skills,
                        Some(&runtime_context),
                        sender_name,
                        Some(&session_store),
                        client_seq,
                        Some(Arc::clone(&active_partial_assistant)),
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
                        &run_id_clone,
                        provider,
                        memory_forget_provider_resolver,
                        &tool_registry,
                        &user_content,
                        &provider_name,
                        &history,
                        &chat_history,
                        &session_key_clone,
                        &session_agent_id_clone,
                        resolved_reasoning_effort.clone(),
                        desired_reply_medium,
                        ctx_ref,
                        Some(&runtime_context),
                        &discovered_skills,
                        hook_registry,
                        accept_language.clone(),
                        conn_id.clone(),
                        Some(&session_store),
                        mcp_disabled,
                        client_seq,
                        Some(Arc::clone(&active_tool_invocations)),
                        Some(Arc::clone(&active_partial_assistant)),
                        &active_event_forwarders,
                        &terminal_runs,
                        sender_name,
                        tool_choice,
                    )
                    .await
                }
            };

            tokio::pin!(agent_fut);
            let run_outcome = if outer_agent_timeout_secs > 0 {
                match tokio::time::timeout(
                    Duration::from_secs(outer_agent_timeout_secs),
                    &mut agent_fut,
                )
                .await
                {
                    Ok(result) => result,
                    Err(_) => {
                        let timeout_detail =
                            format!("Agent run timed out after {outer_agent_timeout_secs}s");
                        let timeout_ui = session_store.ui_history.session(&session_key_clone).await;
                        match &timeout_ui {
                            Ok(ui) => {
                                if let Err(error) = ui.record_error(chelix_sessions::ui_history_types::UiProviderError {
                                    run_id: run_id_clone.clone(), segment_id: None, created_at: now_ms(), raw: timeout_detail.clone(),
                                    details: serde_json::json!({"type": "timeout", "title": "Timed out", "detail": timeout_detail}), retry_after_ms: None,
                                }) {
                                    tracing::error!(%error, "failed to retain timeout in UI history");
                                }
                            },
                            Err(error) => tracing::error!(%error, "UI history unavailable for timeout"),
                        }
                        cancellation_token.cancel();
                        let cancellation_outcome = agent_fut.await;
                        if matches!(cancellation_outcome, ChatRunOutcome::Failed) {
                            tracing::error!(run_id = %run_id_clone, "timeout cancellation finalization failed");
                        }
                        warn!(
                            run_id = %run_id_clone,
                            session = %session_key_clone,
                            timeout_secs = outer_agent_timeout_secs,
                            "agent run timed out"
                        );
                        let mut detail = timeout_detail;
                        if matches!(cancellation_outcome, ChatRunOutcome::Failed)
                            && let Some(error) = state.last_run_error(&run_id_clone).await
                        {
                            detail = format!("{detail}; {error}");
                        }
                        if let Ok(ui) = timeout_ui
                            && let Err(error) = ui.flush().await
                        {
                            tracing::error!(%error, "timeout UI persistence failed");
                            detail = format!("{detail}; UI persistence failed: {error}");
                        }
                        let error_obj = serde_json::json!({
                            "type": "timeout",
                            "title": "Timed out",
                            "detail": detail,
                        });
                        state.set_run_error(&run_id_clone, detail.clone()).await;
                        deliver_channel_error(&state, &session_key_clone, &error_obj).await;
                        terminal_runs.write().await.insert(run_id_clone.clone());
                        let payload = serde_json::json!({
                            "runId": run_id_clone,
                            "sessionKey": session_key_clone,
                            "state": "error",
                        });
                        broadcast(&state, "chat", payload, BroadcastOpts::default()).await;
                        ChatRunOutcome::Failed
                    },
                }
            } else {
                agent_fut.await
            };

            if let Ok(count) = session_store.ui_message_count(&session_key_clone).await {
                if let Err(error) = session_metadata.touch(&session_key_clone, count).await {
                    tracing::error!(
                        session = %session_key_clone,
                        %error,
                        "failed to update session message count"
                    );
                }

                // ── Periodic background memory extraction ──────────────
                // Every `auto_extract_interval` turns, spawn a background
                // silent turn to save important recent context to memory.
                // Uses startup memory settings and reloads `[tools]` for the silent run.
                let interval = auto_extract_interval;
                let write_mode = extraction_write_mode;
                // A "turn" = user + assistant = 2 messages.
                let turn_number = count / 2;
                if matches!(run_outcome, ChatRunOutcome::Completed(_))
                    && interval > 0
                    && turn_number > 0
                    && turn_number % interval == 0
                    && !stream_only
                    && memory_write_mode_allows_save(write_mode)
                    && let Some(mm) = state.memory_manager()
                {
                    let window = (interval as usize) * 2;
                    let recent: Vec<serde_json::Value> =
                        if let Ok(h) = session_store.read(&session_key_clone).await {
                            h.into_iter()
                                .rev()
                                .take(window)
                                .collect::<Vec<_>>()
                                .into_iter()
                                .rev()
                                .collect()
                        } else {
                            Vec::new()
                        };
                    if !recent.is_empty() {
                        match values_to_chat_messages(&recent) {
                            Ok(chat_msgs) => {
                                let agent_id = session_agent_id_clone.clone();
                                let mm = Arc::clone(mm);
                                let prov = Arc::clone(&provider_for_extraction);
                                let extraction_tools_config_source = tools_config_source.clone();
                                tokio::spawn(async move {
                                    let extraction_tools_config =
                                        match extraction_tools_config_source.load() {
                                            Ok(config) => config,
                                            Err(error) => {
                                                tracing::warn!(
                                                    error = %error,
                                                    "periodic memory extraction: failed to reload tools config"
                                                );
                                                return;
                                            },
                                        };
                                    let writer: Arc<
                                        dyn chelix_agents::memory_writer::MemoryWriter,
                                    > = Arc::new(AgentScopedMemoryWriter::new(
                                        mm, agent_id, write_mode,
                                    ));
                                    match chelix_agents::silent_turn::run_silent_memory_turn_with_prompt(
                                        prov,
                                        &extraction_tools_config,
                                        extraction_max_tools_threshold,
                                        &chat_msgs,
                                        writer,
                                        chelix_agents::silent_turn::SilentTurnPrompt::PeriodicExtract,
                                    )
                                    .await
                                    {
                                        Ok(paths) if !paths.is_empty() => {
                                            tracing::info!(
                                                files = paths.len(),
                                                turn = turn_number,
                                                "periodic memory extraction: wrote files"
                                            );
                                        },
                                        Ok(_) => {},
                                        Err(e) => {
                                            tracing::warn!(
                                                error = %e,
                                                "periodic memory extraction failed"
                                            );
                                        },
                                    }
                                });
                            },
                            Err(error) => {
                                tracing::warn!(
                                    %error,
                                    "periodic memory extraction: failed to reconstruct recent history"
                                );
                            },
                        }
                    }
                }
            }

            // ── Auto-title generation ──────────────────────────────
            // After the first completed turn, trigger background title
            // generation. `generate_title_if_needed` guards against
            // duplicate titles.
            if auto_title_enabled
                && let Ok(count) = session_store.ui_message_count(&session_key_clone).await
                && count >= 2
            {
                state.trigger_auto_title(&session_key_clone).await;
            }

            let _ = LiveChatService::wait_for_event_forwarder(
                &active_event_forwarders,
                &session_key_clone,
            )
            .await;

            session_gates
                .finish_turn(&session_key_clone, terminal_from_outcome(&run_outcome))
                .await;
            active_runs.write().await.remove(&run_id_clone);
            let mut runs_by_session = active_runs_by_session.write().await;
            if runs_by_session.get(&session_key_clone) == Some(&run_id_clone) {
                runs_by_session.remove(&session_key_clone);
            }
            drop(runs_by_session);
            active_tool_invocations
                .write()
                .await
                .remove(&session_key_clone);
            terminal_runs.write().await.remove(&run_id_clone);
            active_partial_assistant
                .write()
                .await
                .remove(&session_key_clone);
            active_reply_medium.write().await.remove(&session_key_clone);

            let session_id = SessionKey::new(session_key_clone.clone());
            let drain = match queued_prompts.drain(session_id.clone()).await {
                Ok(drain) => drain,
                Err(error) => {
                    warn!(
                        session = %session_key_clone,
                        %error,
                        "queuedPrompts drain failed after the complete final gate"
                    );
                    broadcast(
                        &state,
                        "chat",
                        serde_json::json!({
                            "runId": run_id_clone,
                            "sessionKey": session_key_clone,
                            "state": "error",
                            "error": {
                                "type": "queued_prompts",
                                "detail": error.to_string(),
                            },
                        }),
                        BroadcastOpts::default(),
                    )
                    .await;
                    return;
                },
            };
            if let Err(error) = broadcast_queued_prompts_status(&state, &drain.status).await {
                warn!(
                    session = %session_key_clone,
                    %error,
                    "failed to broadcast queuedPrompts status after drain"
                );
                return;
            }
            if drain.prompts.is_empty() {
                return;
            }

            let prompts = drain
                .prompts
                .into_iter()
                .map(|prompt| prompt.content)
                .collect::<Vec<_>>();
            info!(
                session = %session_key_clone,
                count = prompts.len(),
                "starting drained queuedPrompts batch as one full agent turn"
            );
            if let Err(error) = owner.start_queued_batch(session_id, prompts).await {
                warn!(
                    session = %session_key_clone,
                    %error,
                    "drained queuedPrompts batch was not accepted by the session"
                );
                broadcast(
                    &state,
                    "chat",
                    serde_json::json!({
                        "runId": run_id_clone,
                        "sessionKey": session_key_clone,
                        "state": "error",
                        "error": {
                            "type": "queued_prompts",
                            "detail": error.to_string(),
                        },
                    }),
                    BroadcastOpts::default(),
                )
                .await;
            }
        });

        info!(
            run_id = %run_id,
            session = %session_key,
            client_seq = ?client_seq,
            "chat.send: returning run id"
        );
        Ok(serde_json::json!({
            "ok": true,
            "runId": run_id,
        }))
    }
}
