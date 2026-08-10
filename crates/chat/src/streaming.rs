//! Streaming mode (no tools) - `run_streaming` with retry logic.

use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
    time::{Duration, Instant},
};

use {
    serde_json::Value,
    tokio::sync::RwLock,
    tokio_stream::StreamExt,
    tracing::{info, warn},
};

use {
    chelix_agents::{
        ChatMessage, UserContent,
        model::{StreamEvent, push_capped_provider_raw_event, values_to_chat_messages},
        prompt::{PromptRuntimeContext, build_system_prompt_minimal_runtime_details},
    },
    chelix_sessions::store::SessionStore,
};

use crate::{
    agent_loop::{ChannelStreamDispatcher, clear_unsupported_model, mark_unsupported_model},
    channels::{
        deliver_channel_error, deliver_channel_replies, generate_tts_audio,
        send_retry_status_to_channels,
    },
    chat_error::parse_chat_error,
    message::apply_voice_reply_suffix,
    models::DisabledModelsStore,
    prompt::prompt_build_limits_from_config,
    runtime::ChatRuntime,
    service::{
        ActiveAssistantDraft, append_assistant_delta, persist_active_assistant_draft,
        persist_final_assistant_segment, reserve_assistant_message_index,
    },
    types::*,
};

#[cfg(feature = "push-notifications")]
use crate::channels::send_chat_push_notification;

const STREAM_RETRYABLE_SERVER_PATTERNS: &[&str] = &[
    "http 500",
    "http 502",
    "http 503",
    "http 504",
    "internal server error",
    "service unavailable",
    "gateway timeout",
    "temporarily unavailable",
    "overloaded",
    "timeout",
    "connection reset",
];
const STREAM_SERVER_RETRY_DELAY_MS: u64 = 2_000;
const STREAM_SERVER_MAX_RETRIES: u8 = 1;
const STREAM_RATE_LIMIT_INITIAL_RETRY_MS: u64 = 2_000;
const STREAM_RATE_LIMIT_MAX_RETRY_MS: u64 = 60_000;
const STREAM_RATE_LIMIT_MAX_RETRIES: u8 = 10;

fn is_retryable_stream_server_error(msg: &str) -> bool {
    let lower = msg.to_ascii_lowercase();
    STREAM_RETRYABLE_SERVER_PATTERNS
        .iter()
        .any(|pattern| lower.contains(pattern))
}

fn next_stream_rate_limit_retry_ms(previous_ms: Option<u64>) -> u64 {
    previous_ms
        .map(|ms| ms.saturating_mul(2))
        .unwrap_or(STREAM_RATE_LIMIT_INITIAL_RETRY_MS)
        .clamp(
            STREAM_RATE_LIMIT_INITIAL_RETRY_MS,
            STREAM_RATE_LIMIT_MAX_RETRY_MS,
        )
}

fn next_stream_retry_delay_ms(
    raw_error: &str,
    error_obj: &Value,
    server_retries_remaining: &mut u8,
    rate_limit_retries_remaining: &mut u8,
    rate_limit_backoff_ms: &mut Option<u64>,
) -> Option<u64> {
    if error_obj.get("type").and_then(Value::as_str) == Some("rate_limit_exceeded") {
        if *rate_limit_retries_remaining == 0 {
            return None;
        }
        *rate_limit_retries_remaining -= 1;

        let current_backoff = *rate_limit_backoff_ms;
        *rate_limit_backoff_ms = Some(next_stream_rate_limit_retry_ms(current_backoff));

        let hinted_ms = error_obj.get("retryAfterMs").and_then(Value::as_u64);
        let delay_ms = hinted_ms
            .or(*rate_limit_backoff_ms)
            .unwrap_or(STREAM_RATE_LIMIT_INITIAL_RETRY_MS);
        return Some(delay_ms.clamp(1, STREAM_RATE_LIMIT_MAX_RETRY_MS));
    }

    if is_retryable_stream_server_error(raw_error) {
        if *server_retries_remaining == 0 {
            return None;
        }
        *server_retries_remaining -= 1;
        return Some(STREAM_SERVER_RETRY_DELAY_MS);
    }

    None
}

async fn persist_streaming_partial(
    session_store: Option<&Arc<SessionStore>>,
    active_partial_assistant: Option<&Arc<RwLock<HashMap<String, ActiveAssistantDraft>>>>,
    session_key: &str,
) -> crate::error::Result<Option<(Value, usize)>> {
    match (session_store, active_partial_assistant) {
        (Some(store), Some(drafts)) => {
            persist_active_assistant_draft(store, drafts, session_key).await
        },
        (None, None) => Ok(None),
        _ => Err(crate::error::Error::message(
            "assistant persistence dependencies are inconsistent",
        )),
    }
}

fn attach_partial_to_error_payload(payload: &mut Value, partial: Option<(Value, usize)>) {
    if let Some((partial_message, message_index)) = partial {
        payload["partialMessage"] = partial_message;
        payload["messageIndex"] = serde_json::json!(message_index);
    }
}

pub(crate) async fn run_streaming(
    persona: PromptPersona,
    state: &Arc<dyn ChatRuntime>,
    model_store: &Arc<RwLock<DisabledModelsStore>>,
    run_id: &str,
    provider: Arc<dyn chelix_agents::model::LlmProvider>,
    model_id: &str,
    user_content: &UserContent,
    provider_name: &str,
    history_raw: &[Value],
    session_key: &str,
    _agent_id: &str,
    session_reasoning_effort: Option<String>,
    desired_reply_medium: ReplyMedium,
    project_context: Option<&str>,
    _skills: &[chelix_skills::types::SkillMetadata],
    runtime_context: Option<&PromptRuntimeContext>,
    sender_name: Option<String>,
    session_store: Option<&Arc<SessionStore>>,
    client_seq: Option<u64>,
    active_partial_assistant: Option<Arc<RwLock<HashMap<String, ActiveAssistantDraft>>>>,
    terminal_runs: &Arc<RwLock<HashSet<String>>>,
) -> Option<AssistantTurnOutput> {
    let run_started = Instant::now();

    // ── Memory prefetch (same logic as run_with_tools) ───────────
    let mut memory_text_with_prefetch: Option<String> = None;
    if persona.config.memory.enable_prefetch {
        let query_text = match user_content {
            UserContent::Text(t) => Some(t.as_str()),
            UserContent::Multimodal(parts) => parts.iter().find_map(|p| match p {
                chelix_agents::model::ContentPart::Text(t) => Some(t.as_str()),
                _ => None,
            }),
        };
        if let Some(query) = query_text
            && query.len() >= 10
            && !query.starts_with('/')
            && let Some(manager) = state.memory_manager()
        {
            let limit = persona.config.memory.prefetch_limit.clamp(1, 10);
            match manager.search(query, limit).await {
                Ok(results) if !results.is_empty() => {
                    let recalled = crate::run_with_tools::format_recalled_context(&results);
                    let mut combined = persona
                        .memory_text
                        .as_deref()
                        .unwrap_or_default()
                        .to_string();
                    if !combined.is_empty() {
                        combined.push_str("\n\n");
                    }
                    combined.push_str(&recalled);
                    memory_text_with_prefetch = Some(combined);
                    info!(
                        results = results.len(),
                        session = %session_key,
                        "memory prefetch (streaming): injected recalled context"
                    );
                },
                Ok(_) => {},
                Err(e) => {
                    warn!(error = %e, "memory prefetch (streaming) failed");
                },
            }
        }
    }
    let effective_memory_text = memory_text_with_prefetch
        .as_deref()
        .or(persona.memory_text.as_deref());

    let system_prompt = build_system_prompt_minimal_runtime_details(
        project_context,
        Some(&persona.agent),
        Some(&persona.user),
        persona.soul_text.as_deref(),
        persona.boot_text.as_deref(),
        persona.agents_text.as_deref(),
        persona.tools_text.as_deref(),
        runtime_context,
        effective_memory_text,
        prompt_build_limits_from_config(&persona.config),
        persona.guidelines_text.as_deref(),
    )
    .prompt;

    // Layer 1: instruct the LLM to write speech-friendly output when voice is active.
    let system_prompt = apply_voice_reply_suffix(system_prompt, desired_reply_medium);

    // Fold datetime into the user message content so the message array before
    // it stays positionally stable, preserving KV cache prefix matching for
    // local OpenAI-compatible endpoints and prompt-cache hits for cloud providers.
    let effective_user_content =
        chelix_agents::prompt::prepend_datetime_to_user_content(user_content, runtime_context)
            .unwrap_or_else(|| user_content.clone());

    let mut messages: Vec<ChatMessage> = Vec::new();
    messages.push(ChatMessage::system(system_prompt));
    // Convert persisted JSON history to typed ChatMessages for the LLM provider.
    messages.extend(values_to_chat_messages(history_raw));
    messages.push(ChatMessage::User {
        content: effective_user_content,
        name: sender_name,
    });

    let mut server_retries_remaining: u8 = STREAM_SERVER_MAX_RETRIES;
    let mut rate_limit_retries_remaining: u8 = STREAM_RATE_LIMIT_MAX_RETRIES;
    let mut rate_limit_backoff_ms: Option<u64> = None;
    let mut channel_stream_dispatcher =
        ChannelStreamDispatcher::for_session(state, session_key).await;

    'attempts: loop {
        #[cfg(feature = "metrics")]
        let stream_start = Instant::now();

        let mut stream = provider.stream(messages.clone());
        let mut accumulated = String::new();
        let mut accumulated_reasoning = String::new();
        let mut raw_llm_responses: Vec<Value> = Vec::new();

        while let Some(event) = stream.next().await {
            match event {
                StreamEvent::Delta(delta) => {
                    accumulated.push_str(&delta);
                    let message_index = if let (Some(store), Some(drafts)) =
                        (session_store, active_partial_assistant.as_ref())
                    {
                        match append_assistant_delta(store, drafts, session_key, &delta).await {
                            Ok(message_index) => message_index,
                            Err(error) => {
                                let error = error.to_string();
                                warn!(run_id, %error, "failed to persist streaming assistant segment");
                                state.set_run_error(run_id, error.clone()).await;
                                let error_obj = parse_chat_error(&error, Some(provider_name));
                                deliver_channel_error(state, session_key, &error_obj).await;
                                let error_payload = ChatErrorBroadcast {
                                    run_id: run_id.to_string(),
                                    session_key: session_key.to_string(),
                                    state: "error",
                                    error: error_obj,
                                    seq: client_seq,
                                };
                                #[allow(clippy::unwrap_used)] // serializing known-valid struct
                                let payload_val = serde_json::to_value(&error_payload).unwrap();
                                terminal_runs.write().await.insert(run_id.to_string());
                                broadcast(state, "chat", payload_val, BroadcastOpts::default())
                                    .await;
                                return None;
                            },
                        }
                    } else {
                        None
                    };
                    if let Some(dispatcher) = channel_stream_dispatcher.as_mut() {
                        dispatcher.send_delta(&delta).await;
                    }
                    let mut payload = serde_json::json!({
                        "runId": run_id,
                        "sessionKey": session_key,
                        "state": "delta",
                        "text": delta,
                    });
                    if let Some(message_index) = message_index {
                        payload["messageIndex"] = serde_json::json!(message_index);
                    }
                    broadcast(state, "chat", payload, BroadcastOpts::default()).await;
                },
                StreamEvent::ReasoningDelta(delta) => {
                    accumulated_reasoning.push_str(&delta);
                    if let Some(ref map) = active_partial_assistant
                        && let Some(draft) = map.write().await.get_mut(session_key)
                    {
                        draft.set_reasoning(&accumulated_reasoning);
                    }
                    broadcast(
                        state,
                        "chat",
                        serde_json::json!({
                            "runId": run_id,
                            "sessionKey": session_key,
                            "state": "thinking_text",
                            "text": accumulated_reasoning.clone(),
                        }),
                        BroadcastOpts::default(),
                    )
                    .await;
                },
                StreamEvent::ProviderRaw(raw) => {
                    push_capped_provider_raw_event(&mut raw_llm_responses, raw);
                },
                StreamEvent::Done(usage) => {
                    clear_unsupported_model(state, model_store, model_id).await;

                    // Record streaming completion metrics.
                    #[cfg(feature = "metrics")]
                    {
                        let duration = stream_start.elapsed().as_secs_f64();
                        counter!(
                            llm_metrics::COMPLETIONS_TOTAL,
                            labels::PROVIDER => provider_name.to_string(),
                            labels::MODEL => model_id.to_string()
                        )
                        .increment(1);
                        counter!(
                            llm_metrics::INPUT_TOKENS_TOTAL,
                            labels::PROVIDER => provider_name.to_string(),
                            labels::MODEL => model_id.to_string()
                        )
                        .increment(u64::from(usage.input_tokens));
                        counter!(
                            llm_metrics::OUTPUT_TOKENS_TOTAL,
                            labels::PROVIDER => provider_name.to_string(),
                            labels::MODEL => model_id.to_string()
                        )
                        .increment(u64::from(usage.output_tokens));
                        counter!(
                            llm_metrics::CACHE_READ_TOKENS_TOTAL,
                            labels::PROVIDER => provider_name.to_string(),
                            labels::MODEL => model_id.to_string()
                        )
                        .increment(u64::from(usage.cache_read_tokens));
                        counter!(
                            llm_metrics::CACHE_WRITE_TOKENS_TOTAL,
                            labels::PROVIDER => provider_name.to_string(),
                            labels::MODEL => model_id.to_string()
                        )
                        .increment(u64::from(usage.cache_write_tokens));
                        histogram!(
                            llm_metrics::COMPLETION_DURATION_SECONDS,
                            labels::PROVIDER => provider_name.to_string(),
                            labels::MODEL => model_id.to_string()
                        )
                        .record(duration);
                    }

                    let is_silent = accumulated.trim().is_empty();
                    let reasoning = {
                        let trimmed = accumulated_reasoning.trim();
                        (!trimmed.is_empty()).then(|| trimmed.to_string())
                    };
                    let streamed_target_keys =
                        if let Some(dispatcher) = channel_stream_dispatcher.as_mut() {
                            dispatcher.finish().await;
                            dispatcher.completed_target_keys().await
                        } else {
                            HashSet::new()
                        };

                    info!(
                        run_id,
                        input_tokens = usage.input_tokens,
                        output_tokens = usage.output_tokens,
                        response = %accumulated,
                        silent = is_silent,
                        "chat stream done"
                    );

                    // Detect provider failures: silent stream with zero tokens
                    // means the LLM never produced output (e.g. network_error).
                    if is_silent && usage.output_tokens == 0 {
                        warn!(
                            run_id,
                            "empty stream with zero tokens — treating as provider error"
                        );
                        let provider_error = "The provider returned an empty response (possible network error). Please try again.";
                        let (terminal_error, partial) = match persist_streaming_partial(
                            session_store,
                            active_partial_assistant.as_ref(),
                            session_key,
                        )
                        .await
                        {
                            Ok(partial) => (provider_error.to_string(), partial),
                            Err(error) => (
                                format!(
                                    "{provider_error} Partial assistant persistence failed: {error}"
                                ),
                                None,
                            ),
                        };
                        state.set_run_error(run_id, terminal_error.clone()).await;
                        let error_obj = parse_chat_error(&terminal_error, Some(provider_name));
                        deliver_channel_error(state, session_key, &error_obj).await;
                        let error_payload = ChatErrorBroadcast {
                            run_id: run_id.to_string(),
                            session_key: session_key.to_string(),
                            state: "error",
                            error: error_obj,
                            seq: client_seq,
                        };
                        #[allow(clippy::unwrap_used)] // serializing known-valid struct
                        let mut payload_val = serde_json::to_value(&error_payload).unwrap();
                        attach_partial_to_error_payload(&mut payload_val, partial);
                        terminal_runs.write().await.insert(run_id.to_string());
                        broadcast(state, "chat", payload_val, BroadcastOpts::default()).await;
                        return None;
                    }

                    // Generate & persist TTS audio for voice-medium web UI replies.
                    let mut audio_warning: Option<String> = None;
                    let audio_path = if !is_silent && desired_reply_medium == ReplyMedium::Voice {
                        match generate_tts_audio(state, session_key, &accumulated).await {
                            Ok(bytes) => {
                                let filename = format!("{run_id}.ogg");
                                if let Some(store) = session_store {
                                    match store.save_media(session_key, &filename, &bytes).await {
                                        Ok(path) => Some(path),
                                        Err(e) => {
                                            let warning = format!(
                                                "TTS audio generated but failed to save: {e}"
                                            );
                                            warn!(run_id, error = %warning, "failed to save TTS audio to media dir");
                                            audio_warning = Some(warning);
                                            None
                                        },
                                    }
                                } else {
                                    audio_warning = Some(
                                        "TTS audio generated but session media storage is unavailable"
                                            .to_string(),
                                    );
                                    None
                                }
                            },
                            Err(error) => {
                                let error = error.to_string();
                                warn!(run_id, error = %error, "voice reply generation skipped");
                                audio_warning = Some(error);
                                None
                            },
                        }
                    } else {
                        None
                    };

                    let duration_ms = run_started.elapsed().as_millis() as u64;
                    let llm_api_response =
                        (!raw_llm_responses.is_empty()).then_some(Value::Array(raw_llm_responses));
                    let mut assistant_output = build_assistant_turn_output(
                        accumulated.clone(),
                        None,
                        UsageSnapshot::new(usage.clone(), Some(usage.clone())),
                        duration_ms,
                        audio_path.clone(),
                        reasoning.clone(),
                        llm_api_response,
                    );
                    if let (Some(store), Some(drafts)) =
                        (session_store, active_partial_assistant.as_ref())
                    {
                        let message_index = match reserve_assistant_message_index(
                            store,
                            drafts,
                            session_key,
                        )
                        .await
                        {
                            Ok(message_index) => message_index,
                            Err(error) => {
                                let error = error.to_string();
                                warn!(run_id, %error, "failed to reserve final assistant message index");
                                state.set_run_error(run_id, error.clone()).await;
                                let error_obj = parse_chat_error(&error, Some(provider_name));
                                deliver_channel_error(state, session_key, &error_obj).await;
                                let error_payload = ChatErrorBroadcast {
                                    run_id: run_id.to_string(),
                                    session_key: session_key.to_string(),
                                    state: "error",
                                    error: error_obj,
                                    seq: client_seq,
                                };
                                #[allow(clippy::unwrap_used)] // serializing known-valid struct
                                let payload_val = serde_json::to_value(&error_payload).unwrap();
                                terminal_runs.write().await.insert(run_id.to_string());
                                broadcast(state, "chat", payload_val, BroadcastOpts::default())
                                    .await;
                                return None;
                            },
                        };
                        match persist_final_assistant_segment(
                            store,
                            session_key,
                            &assistant_output,
                            provider.id(),
                            provider_name,
                            session_reasoning_effort.clone(),
                            client_seq,
                            run_id,
                            message_index,
                        )
                        .await
                        {
                            Ok(message_index) => {
                                assistant_output.persisted_message_index = Some(message_index);
                            },
                            Err(error) => {
                                let persistence_error = error.to_string();
                                let (terminal_error, partial) = match persist_streaming_partial(
                                    session_store,
                                    active_partial_assistant.as_ref(),
                                    session_key,
                                )
                                .await
                                {
                                    Ok(partial) => (persistence_error, partial),
                                    Err(error) => (
                                        format!(
                                            "{persistence_error}; partial assistant persistence failed: {error}"
                                        ),
                                        None,
                                    ),
                                };
                                warn!(run_id, error = %terminal_error, "failed to finalize streaming assistant segment");
                                state.set_run_error(run_id, terminal_error.clone()).await;
                                let error_obj =
                                    parse_chat_error(&terminal_error, Some(provider_name));
                                deliver_channel_error(state, session_key, &error_obj).await;
                                let error_payload = ChatErrorBroadcast {
                                    run_id: run_id.to_string(),
                                    session_key: session_key.to_string(),
                                    state: "error",
                                    error: error_obj,
                                    seq: client_seq,
                                };
                                #[allow(clippy::unwrap_used)] // serializing known-valid struct
                                let mut payload_val = serde_json::to_value(&error_payload).unwrap();
                                attach_partial_to_error_payload(&mut payload_val, partial);
                                terminal_runs.write().await.insert(run_id.to_string());
                                broadcast(state, "chat", payload_val, BroadcastOpts::default())
                                    .await;
                                return None;
                            },
                        }
                    }

                    let final_payload = build_chat_final_broadcast(
                        run_id,
                        session_key,
                        accumulated.clone(),
                        provider.id().to_string(),
                        provider_name.to_string(),
                        session_reasoning_effort.clone(),
                        UsageSnapshot::new(usage.clone(), Some(usage.clone())),
                        duration_ms,
                        assistant_output.persisted_message_index,
                        desired_reply_medium,
                        None,
                        None,
                        audio_path.clone(),
                        audio_warning,
                        reasoning.clone(),
                        client_seq,
                    );
                    #[allow(clippy::unwrap_used)] // serializing known-valid struct
                    let payload_val = serde_json::to_value(&final_payload).unwrap();
                    terminal_runs.write().await.insert(run_id.to_string());
                    broadcast(state, "chat", payload_val, BroadcastOpts::default()).await;

                    if !is_silent {
                        // Send push notification when chat response completes
                        #[cfg(feature = "push-notifications")]
                        {
                            tracing::info!("push: checking push notification");
                            send_chat_push_notification(state, session_key, &accumulated).await;
                        }
                        deliver_channel_replies(
                            state,
                            session_key,
                            &accumulated,
                            desired_reply_medium,
                            &streamed_target_keys,
                        )
                        .await;
                    }
                    return Some(assistant_output);
                },
                StreamEvent::Error(msg) => {
                    let provider_error_obj = parse_chat_error(&msg, Some(provider_name));
                    let has_no_streamed_content = accumulated.trim().is_empty()
                        && accumulated_reasoning.trim().is_empty()
                        && raw_llm_responses.is_empty();
                    if has_no_streamed_content
                        && let Some(delay_ms) = next_stream_retry_delay_ms(
                            &msg,
                            &provider_error_obj,
                            &mut server_retries_remaining,
                            &mut rate_limit_retries_remaining,
                            &mut rate_limit_backoff_ms,
                        )
                    {
                        warn!(
                            run_id,
                            error = %msg,
                            delay_ms,
                            server_retries_remaining,
                            rate_limit_retries_remaining,
                            "chat stream transient error, retrying after delay"
                        );
                        if provider_error_obj.get("type").and_then(Value::as_str)
                            == Some("rate_limit_exceeded")
                        {
                            send_retry_status_to_channels(
                                state,
                                session_key,
                                &provider_error_obj,
                                Duration::from_millis(delay_ms),
                            )
                            .await;
                        }
                        broadcast(
                            state,
                            "chat",
                            serde_json::json!({
                                "runId": run_id,
                                "sessionKey": session_key,
                                "state": "retrying",
                                "error": provider_error_obj,
                                "retryAfterMs": delay_ms,
                                "seq": client_seq,
                            }),
                            BroadcastOpts::default(),
                        )
                        .await;
                        tokio::time::sleep(Duration::from_millis(delay_ms)).await;
                        continue 'attempts;
                    }

                    let (terminal_error, partial) = match persist_streaming_partial(
                        session_store,
                        active_partial_assistant.as_ref(),
                        session_key,
                    )
                    .await
                    {
                        Ok(partial) => (msg, partial),
                        Err(error) => (
                            format!("{msg}; partial assistant persistence failed: {error}"),
                            None,
                        ),
                    };
                    warn!(run_id, error = %terminal_error, "chat stream error");
                    if let Some(dispatcher) = channel_stream_dispatcher.as_mut() {
                        dispatcher.finish().await;
                    }
                    state.set_run_error(run_id, terminal_error.clone()).await;
                    mark_unsupported_model(
                        state,
                        model_store,
                        model_id,
                        provider_name,
                        &provider_error_obj,
                    )
                    .await;
                    let error_obj = parse_chat_error(&terminal_error, Some(provider_name));
                    deliver_channel_error(state, session_key, &error_obj).await;
                    let error_payload = ChatErrorBroadcast {
                        run_id: run_id.to_string(),
                        session_key: session_key.to_string(),
                        state: "error",
                        error: error_obj,
                        seq: client_seq,
                    };
                    #[allow(clippy::unwrap_used)] // serializing known-valid struct
                    let mut payload_val = serde_json::to_value(&error_payload).unwrap();
                    attach_partial_to_error_payload(&mut payload_val, partial);
                    terminal_runs.write().await.insert(run_id.to_string());
                    broadcast(state, "chat", payload_val, BroadcastOpts::default()).await;
                    return None;
                },
                // Tool events not expected in stream-only mode.
                StreamEvent::ToolCallStart { .. }
                | StreamEvent::ToolCallArgumentsDelta { .. }
                | StreamEvent::ToolCallComplete { .. } => {},
            }
        }

        // Stream ended unexpectedly without Done/Error.
        let stream_error = "The provider stream ended without a terminal event.";
        let (terminal_error, partial) = match persist_streaming_partial(
            session_store,
            active_partial_assistant.as_ref(),
            session_key,
        )
        .await
        {
            Ok(partial) => (stream_error.to_string(), partial),
            Err(error) => (
                format!("{stream_error} Partial assistant persistence failed: {error}"),
                None,
            ),
        };
        warn!(run_id, error = %terminal_error, "chat stream ended unexpectedly");
        if let Some(dispatcher) = channel_stream_dispatcher.as_mut() {
            dispatcher.finish().await;
        }
        state.set_run_error(run_id, terminal_error.clone()).await;
        let error_obj = parse_chat_error(&terminal_error, Some(provider_name));
        deliver_channel_error(state, session_key, &error_obj).await;
        let error_payload = ChatErrorBroadcast {
            run_id: run_id.to_string(),
            session_key: session_key.to_string(),
            state: "error",
            error: error_obj,
            seq: client_seq,
        };
        #[allow(clippy::unwrap_used)] // serializing known-valid struct
        let mut payload_val = serde_json::to_value(&error_payload).unwrap();
        attach_partial_to_error_payload(&mut payload_val, partial);
        terminal_runs.write().await.insert(run_id.to_string());
        broadcast(state, "chat", payload_val, BroadcastOpts::default()).await;
        return None;
    }
}
