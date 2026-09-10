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
    tokio_util::sync::CancellationToken,
    tracing::{info, warn},
};

use {
    chelix_agents::{
        ChatMessage, UserContent,
        model::{StreamEvent, Usage, push_capped_provider_raw_event},
        prompt::{PromptRuntimeContext, build_system_prompt_minimal_runtime_details},
    },
    chelix_common::{ProviderSegmentId, ProviderSegmentMaterializer, ProviderSegmentOutcome},
    chelix_sessions::{PersistedMessage, store::SessionStore},
};

use crate::{
    agent_loop::ChannelStreamDispatcher,
    channels::{
        deliver_channel_error, deliver_channel_replies, generate_tts_audio,
        send_retry_status_to_channels,
    },
    chat_error::parse_chat_error,
    message::apply_voice_reply_suffix,
    prompt::prompt_build_limits_from_config,
    runtime::ChatRuntime,
    service::{
        ActiveAssistantDraft, persist_active_assistant_draft, persist_final_assistant_segment,
    },
    stream_journal::StreamJournal,
    types::*,
};

#[cfg(feature = "push-notifications")]
use crate::channels::send_chat_push_notification;

const STREAM_RETRYABLE_SERVER_PATTERNS: &[&str] = &[
    "http 500",
    "http 502",
    "http 503",
    "http 504",
    "http 529",
    "server_error",
    "internal server error",
    "overloaded",
    "bad gateway",
    "service unavailable",
    "gateway timeout",
    "temporarily unavailable",
    "the server had an error processing your request",
    "timeout",
    "connection reset",
];
const STREAM_TERMINAL_ERROR_TYPES: &[&str] =
    &["auth_error", "model_not_found", "unsupported_model"];
const STREAM_TERMINAL_ERROR_PATTERNS: &[&str] = &[
    "http 400",
    "http 401",
    "http 403",
    "http 404",
    "http 405",
    "http 413",
    "http 415",
    "http 422",
    "invalid_request_error",
    "invalid_api_key",
    "authentication_error",
    "permission_denied",
    "model_not_found",
    "unsupported_model",
    "context_length_exceeded",
    "response incomplete:",
];
const STREAM_SERVER_RETRY_DELAY_MS: u64 = 2_000;
const STREAM_SERVER_MAX_RETRIES: u8 = 1;
const STREAM_UNKNOWN_RETRY_DELAY_MS: u64 = 10_000;
const STREAM_UNKNOWN_MAX_RETRIES: u8 = 1;
const STREAM_RATE_LIMIT_INITIAL_RETRY_MS: u64 = 2_000;
const STREAM_RATE_LIMIT_MAX_RETRY_MS: u64 = 60_000;
const STREAM_RATE_LIMIT_MAX_RETRIES: u8 = 10;

fn is_retryable_stream_server_error(msg: &str) -> bool {
    let lower = msg.to_ascii_lowercase();
    STREAM_RETRYABLE_SERVER_PATTERNS
        .iter()
        .any(|pattern| lower.contains(pattern))
}

fn is_terminal_stream_error(raw_error: &str, error_type: Option<&str>) -> bool {
    if error_type.is_some_and(|error_type| STREAM_TERMINAL_ERROR_TYPES.contains(&error_type)) {
        return true;
    }
    let lower = raw_error.to_ascii_lowercase();
    STREAM_TERMINAL_ERROR_PATTERNS
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
    unknown_retries_remaining: &mut u8,
) -> Option<u64> {
    let error_type = error_obj.get("type").and_then(Value::as_str);
    if error_type == Some("billing_exhausted") {
        return None;
    }

    if error_type == Some("rate_limit_exceeded") {
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

    if error_type == Some("server_error") || is_retryable_stream_server_error(raw_error) {
        if *server_retries_remaining == 0 {
            return None;
        }
        *server_retries_remaining -= 1;
        return Some(STREAM_SERVER_RETRY_DELAY_MS);
    }

    if is_terminal_stream_error(raw_error, error_type) {
        return None;
    }

    if *unknown_retries_remaining == 0 {
        return None;
    }
    *unknown_retries_remaining -= 1;
    Some(STREAM_UNKNOWN_RETRY_DELAY_MS)
}

fn failed_stream_attempt_message(
    materializer: &ProviderSegmentMaterializer,
) -> Option<ChatMessage> {
    if materializer.segment.items.is_empty() {
        return None;
    }
    Some(ChatMessage::Assistant {
        content: materializer.segment.message_text(),
        tool_calls: Vec::new(),
        reasoning: materializer.segment.reasoning_content(),
        provider_items: materializer.segment.items.clone(),
        segment_id: materializer.segment.segment_id.clone(),
    })
}

#[allow(clippy::too_many_arguments)]
async fn close_stream_segment(
    journal: Option<&StreamJournal>,
    ui_run: Option<&chelix_sessions::ui_history_engine::UiHistoryRun>,
    materializer: &mut ProviderSegmentMaterializer,
    segment_id: ProviderSegmentId,
    outcome: ProviderSegmentOutcome,
    usage: Option<Usage>,
    run_id: &str,
    client_seq: Option<u64>,
) -> Result<(), String> {
    materializer
        .close(outcome)
        .map_err(|error| format!("provider segment close rejected: {error}"))?;
    let persisted = PersistedMessage::ProviderSegmentClose {
        segment_id,
        outcome,
        created_at: Some(now_ms()),
        seq: client_seq,
        run_id: Some(run_id.to_string()),
    };
    if let Some(run) = ui_run {
        let id = run
            .copy(persisted.clone())
            .map_err(|error| error.to_string())?;
        if let Some(usage) = usage {
            let value = serde_json::to_value(usage).map_err(|error| error.to_string())?;
            run.merge_metadata(
                &id,
                std::collections::BTreeMap::from([("segmentUsage".to_string(), value)]),
            )
            .map_err(|error| error.to_string())?;
        }
    }
    if let Some(journal) = journal {
        journal.append(persisted)?;
        journal.flush().await?;
    }
    Ok(())
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

#[allow(clippy::too_many_arguments)]
async fn finish_streaming_cancellation(
    journal: Option<&StreamJournal>,
    ui_run: Option<&chelix_sessions::ui_history_engine::UiHistoryRun>,
    state: &Arc<dyn ChatRuntime>,
    session_store: Option<&Arc<SessionStore>>,
    active_partial_assistant: Option<&Arc<RwLock<HashMap<String, ActiveAssistantDraft>>>>,
    materializer: &mut ProviderSegmentMaterializer,
    channel_stream_dispatcher: Option<&mut ChannelStreamDispatcher>,
    run_id: &str,
    session_key: &str,
    client_seq: Option<u64>,
) -> ChatRunOutcome {
    let mut terminal_errors = Vec::new();
    if materializer.segment.outcome == ProviderSegmentOutcome::Active
        && let Some(segment_id) = materializer.segment.segment_id.clone()
        && let Err(error) = close_stream_segment(
            journal,
            ui_run,
            materializer,
            segment_id,
            ProviderSegmentOutcome::Cancelled,
            None,
            run_id,
            client_seq,
        )
        .await
    {
        terminal_errors.push(error);
    }

    if let Some(dispatcher) = channel_stream_dispatcher {
        dispatcher.finish().await;
    }
    if let Err(error) =
        persist_streaming_partial(session_store, active_partial_assistant, session_key).await
    {
        terminal_errors.push(error.to_string());
    }
    if terminal_errors.is_empty() {
        return ChatRunOutcome::Cancelled;
    }

    let error = terminal_errors.join("; ");
    warn!(run_id, %error, "failed to finalize cancelled streaming run");
    let error_obj = serde_json::json!({
        "title": "Failed to stop assistant cleanly",
        "detail": error,
    });
    crate::ui_history_ingress::fail_run(ui_run, state, run_id, error, "", Some(error_obj.clone()))
        .await;
    deliver_channel_error(state, session_key, &error_obj).await;
    ChatRunOutcome::Failed
}

pub(crate) async fn run_streaming(
    persona: PromptPersona,
    compaction_reminder: crate::compaction_reminder::CompactionReminder,
    cancellation_token: &CancellationToken,
    state: &Arc<dyn ChatRuntime>,
    run_id: &str,
    provider: Arc<dyn chelix_agents::model::LlmProvider>,
    model_id: &str,
    user_content: &UserContent,
    provider_name: &str,
    chat_history: &[ChatMessage],
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
) -> ChatRunOutcome {
    let ui_run = match crate::ui_history_ingress::begin(
        session_store,
        session_key,
        run_id,
        provider.id(),
        provider_name,
        session_reasoning_effort.clone(),
    )
    .await
    {
        Ok(run) => run,
        Err(error) => {
            tracing::error!(%error, run_id, "UI history refused streaming");
            state.set_run_error(run_id, error.to_string()).await;
            return ChatRunOutcome::Failed;
        },
    };
    let journal = session_store.map(|store| {
        StreamJournal::new(
            Arc::clone(store),
            session_key.to_string(),
            cancellation_token.clone(),
        )
    });
    let health_monitor =
        crate::ui_history_ingress::monitor(ui_run.as_ref(), cancellation_token.clone());
    let outcome = async {
    #[cfg(not(feature = "metrics"))]
    let _ = model_id;
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
    let system_prompt = compaction_reminder.render(&system_prompt);

    // Fold datetime into the user message content so the message array before
    // it stays positionally stable, preserving KV cache prefix matching for
    // local OpenAI-compatible endpoints and prompt-cache hits for cloud providers.
    let effective_user_content =
        chelix_agents::prompt::prepend_datetime_to_user_content(user_content, runtime_context)
            .unwrap_or_else(|| user_content.clone());

    let mut messages: Vec<ChatMessage> = Vec::new();
    messages.push(ChatMessage::system(system_prompt));
    messages.extend_from_slice(chat_history);
    messages.push(ChatMessage::User {
        content: effective_user_content,
        name: sender_name,
    });

    let mut server_retries_remaining: u8 = STREAM_SERVER_MAX_RETRIES;
    let mut rate_limit_retries_remaining: u8 = STREAM_RATE_LIMIT_MAX_RETRIES;
    let mut rate_limit_backoff_ms: Option<u64> = None;
    let mut unknown_retries_remaining: u8 = STREAM_UNKNOWN_MAX_RETRIES;
    let mut raw_llm_responses: Vec<Value> = Vec::new();
    let mut channel_stream_dispatcher =
        ChannelStreamDispatcher::for_session(state, session_key).await;

    'attempts: loop {
        #[cfg(feature = "metrics")]
        let stream_start = Instant::now();

        if let Some(run) = &ui_run && let Err(error) = run.start_attempt(None) {
            crate::ui_history_ingress::fail_run(ui_run.as_ref(), state, run_id, error.to_string(), provider_name, None).await;
            return ChatRunOutcome::Failed;
        }
        if let Some(map) = &active_partial_assistant
            && let Some(draft) = map.write().await.get_mut(session_key)
        {
            *draft = draft.next_segment();
        }
        let mut stream = provider.stream(messages.clone());
        let mut accumulated = String::new();
        // Segment identity is owned by the provider and adopted on ingress.
        let mut materializer = ProviderSegmentMaterializer::pending();
        // Set when the canonical provider pipeline refuses to continue. The run
        // must fail loudly instead of silently dropping provider output.
        let mut stream_failure: Option<String> = None;

        loop {
            let event = tokio::select! {
                biased;
                () = cancellation_token.cancelled() => {
                    return finish_streaming_cancellation(
                        journal.as_ref(), ui_run.as_ref(),
                        state,
                        session_store,
                        active_partial_assistant.as_ref(),
                        &mut materializer,
                        channel_stream_dispatcher.as_mut(),
                        run_id,
                        session_key,
                        client_seq,
                    )
                    .await;
                },
                event = stream.next() => event,
            };
            let Some(event) = event else {
                break;
            };
            let event = match event {
                StreamEvent::Done(usage)
                    if accumulated.trim().is_empty()
                        && materializer.segment.items.is_empty()
                        && usage.output_tokens == 0 =>
                {
                    StreamEvent::Error("The provider returned an empty response (possible network error). Please try again.".to_string())
                },
                event => event,
            };
            match event {
                StreamEvent::SegmentStart { segment_id } => {
                    if let Some(run) = &ui_run && let Err(error) = run.start_attempt(Some(segment_id.clone())) {
                        stream_failure = Some(error.to_string());
                        break;
                    }
                    materializer = ProviderSegmentMaterializer::new(segment_id.clone());
                    if let Some(ref map) = active_partial_assistant
                        && let Some(draft) = map.write().await.get_mut(session_key)
                    {
                        draft.start_segment(segment_id.clone());
                    }
                    broadcast(
                        state,
                        "chat",
                        serde_json::json!({
                            "runId": run_id,
                            "sessionKey": session_key,
                            "state": "segment_start",
                            "segmentId": segment_id.0,
                        }),
                        BroadcastOpts::default(),
                    )
                    .await;
                },
                StreamEvent::ProviderItemUpdate(update) => {
                    if let Some(run) = &ui_run
                        && let Err(error) = run.copy(PersistedMessage::ProviderUpdate {
                            update: update.clone(), created_at: Some(now_ms()), seq: client_seq, run_id: Some(run_id.to_string()),
                        })
                    {
                        stream_failure = Some(error.to_string());
                        break;
                    }
                    if let Err(error) = materializer.apply_update(&update) {
                        stream_failure = Some(format!("provider item update rejected: {error}"));
                        break;
                    }
                    if let Some(ref map) = active_partial_assistant
                        && let Some(draft) = map.write().await.get_mut(session_key)
                        && let Err(error) = draft.apply_update(&update)
                    {
                        stream_failure =
                            Some(format!("active assistant draft rejected update: {error}"));
                        break;
                    }
                    if let Some(journal) = &journal {
                        let persisted = PersistedMessage::ProviderUpdate {
                            update, created_at: Some(now_ms()), seq: client_seq, run_id: Some(run_id.to_string()),
                        };
                        if let Err(error) = journal.append(persisted) {
                            stream_failure = Some(error);
                            break;
                        }
                    }
                },
                StreamEvent::SegmentClose {
                    segment_id,
                    outcome,
                    usage,
                } => {
                    if let Err(error) = close_stream_segment(
                        journal.as_ref(),
                        ui_run.as_ref(),
                        &mut materializer,
                        segment_id,
                        outcome,
                        usage,
                        run_id,
                        client_seq,
                    )
                    .await
                    {
                        stream_failure = Some(error);
                        break;
                    }
                },
                StreamEvent::Delta(delta) => {
                    accumulated.push_str(&delta);
                    if let Some(dispatcher) = channel_stream_dispatcher.as_mut() {
                        dispatcher.send_delta(&delta).await;
                    }

                },
                StreamEvent::ProviderRaw(raw) => {
                    push_capped_provider_raw_event(&mut raw_llm_responses, raw);
                },
                StreamEvent::Done(usage) => {
                    if materializer.segment.outcome == ProviderSegmentOutcome::Active
                        && let Some(segment_id) = materializer.segment.segment_id.clone()
                        && let Err(error) = close_stream_segment(
                            journal.as_ref(), ui_run.as_ref(), &mut materializer, segment_id,
                            ProviderSegmentOutcome::Completed, Some(usage.clone()), run_id, client_seq,
                        ).await
                    {
                        stream_failure = Some(error);
                        break;
                    }
                    if let Some(journal) = &journal && let Err(error) = journal.flush().await {
                        stream_failure = Some(error);
                        break;
                    }
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
                    let reasoning = materializer
                        .segment
                        .reasoning_content()
                        .filter(|reasoning| !reasoning.is_blank());

                    info!(
                        run_id,
                        input_tokens = usage.input_tokens,
                        output_tokens = usage.output_tokens,
                        response = %accumulated,
                        silent = is_silent,
                        "chat stream done"
                    );

                    let streamed_target_keys =
                        if let Some(dispatcher) = channel_stream_dispatcher.as_mut() {
                            dispatcher.finish().await;
                            dispatcher.completed_target_keys().await
                        } else {
                            HashSet::new()
                        };

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
                        materializer.segment.items.clone(),
                        materializer.segment.segment_id.clone(),
                        llm_api_response,
                    );
                    if let (Some(store), Some(_drafts)) =
                        (session_store, active_partial_assistant.as_ref())
                    {
                        match persist_final_assistant_segment(
                            store,
                            session_key,
                            &assistant_output,
                            provider.id(),
                            provider_name,
                            session_reasoning_effort.clone(),
                            client_seq,
                            run_id,
                        )
                        .await
                        {
                            Ok(message_index) => {
                                assistant_output.persisted_message_index = Some(message_index);
                            },
                            Err(error) => {
                                stream_failure = Some(error.to_string());
                                break;
                            },
                        }
                    }

                    if let Some(drafts) = &active_partial_assistant {
                        drafts.write().await.remove(session_key);
                    }
                    if let Err(error) = crate::ui_history_ingress::finish_output(ui_run.as_ref(), &assistant_output, desired_reply_medium, audio_warning, None).await {
                        stream_failure = Some(error.to_string());
                        break;
                    }

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
                    return ChatRunOutcome::Completed(Box::new(assistant_output));
                },
                StreamEvent::Error(msg) => {
                    let provider_error_obj = parse_chat_error(&msg, Some(provider_name));
                    let retry_after_ms = next_stream_retry_delay_ms(
                        &msg,
                        &provider_error_obj,
                        &mut server_retries_remaining,
                        &mut rate_limit_retries_remaining,
                        &mut rate_limit_backoff_ms,
                        &mut unknown_retries_remaining,
                    );
                    if let Some(run) = &ui_run
                        && let Err(error) = crate::ui_history_ingress::record_error(run, run_id, &msg, provider_name, retry_after_ms)
                    {
                        stream_failure = Some(format!("{msg}; UI error retention failed: {error}"));
                        break;
                    }
                    if materializer.segment.outcome == ProviderSegmentOutcome::Active
                        && let Some(segment_id) = materializer.segment.segment_id.clone()
                        && let Err(error) = close_stream_segment(
                            journal.as_ref(), ui_run.as_ref(), &mut materializer, segment_id,
                            ProviderSegmentOutcome::TransportError, None, run_id, client_seq,
                        ).await
                    {
                        stream_failure = Some(format!("{msg}; {error}"));
                        break;
                    }
                    if let Some(delay_ms) = retry_after_ms {
                        if let Some(message) = failed_stream_attempt_message(&materializer) {
                            messages.push(message);
                        }
                        warn!(
                            run_id,
                            error = %msg,
                            delay_ms,
                            server_retries_remaining,
                            rate_limit_retries_remaining,
                            unknown_retries_remaining,
                            "chat stream error, retrying after delay"
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
                                "retryAfterMs": delay_ms,
                            }),
                            BroadcastOpts::default(),
                        )
                        .await;
                        if let Some(dispatcher) = channel_stream_dispatcher.as_mut() {
                            dispatcher.finish().await;
                        }
                        if cancellation_token
                            .run_until_cancelled(tokio::time::sleep(Duration::from_millis(
                                delay_ms,
                            )))
                            .await
                            .is_none()
                        {
                            return finish_streaming_cancellation(
                                journal.as_ref(), ui_run.as_ref(),
                                state,
                                session_store,
                                active_partial_assistant.as_ref(),
                                &mut materializer,
                                channel_stream_dispatcher.as_mut(),
                                run_id,
                                session_key,
                                client_seq,
                            )
                            .await;
                        }
                        channel_stream_dispatcher =
                            ChannelStreamDispatcher::for_session(state, session_key).await;
                        continue 'attempts;
                    }

                    stream_failure = Some(msg);
                    break;
                },
                // Tool events not expected in stream-only mode.
                StreamEvent::ToolCallStart { .. }
                | StreamEvent::ToolCallArgumentsDelta { .. }
                | StreamEvent::ToolCallComplete { .. } => {},
            }
        }

        let stream_error = stream_failure
            .unwrap_or_else(|| "The provider stream ended without a terminal event.".to_string());
        if let Some(run) = &ui_run {
            let retained = run.recorded_error(&stream_error).and_then(|existing| {
                if existing.is_none() {
                    crate::ui_history_ingress::record_error(run, run_id, &stream_error, provider_name, None)?;
                }
                Ok(())
            });
            if let Err(error) = retained {
                tracing::error!(run_id, %error, "failed to retain streaming error");
            }
        }
        let mut errors = vec![stream_error];
        if materializer.segment.outcome == ProviderSegmentOutcome::Active
            && let Some(segment_id) = materializer.segment.segment_id.clone()
            && let Err(error) = close_stream_segment(
                journal.as_ref(), ui_run.as_ref(), &mut materializer, segment_id,
                ProviderSegmentOutcome::TransportError, None, run_id, client_seq,
            ).await
        {
            errors.push(error);
        }
        if let Some(journal) = &journal && let Err(error) = journal.flush().await {
            errors.push(error);
        }
        if let Err(error) = persist_streaming_partial(
            session_store, active_partial_assistant.as_ref(), session_key,
        ).await {
            errors.push(format!("partial assistant persistence failed: {error}"));
        }
        let terminal_error = errors.join("; ");
        warn!(run_id, error = %terminal_error, "chat stream terminated without a successful outcome");
        if let Some(dispatcher) = channel_stream_dispatcher.as_mut() {
            dispatcher.finish().await;
        }
        let error_obj = parse_chat_error(&terminal_error, Some(provider_name));
        crate::ui_history_ingress::fail_run(ui_run.as_ref(), state, run_id, terminal_error, provider_name, Some(error_obj.clone())).await;
        deliver_channel_error(state, session_key, &error_obj).await;
        return ChatRunOutcome::Failed;
    }
    }.await;
    if let Some(monitor) = health_monitor {
        monitor.abort();
    }
    let outcome = if let Some(journal) = &journal
        && let Err(error) = journal.flush().await
    {
        tracing::error!(run_id, %error, "stream journal terminal flush failed");
        crate::ui_history_ingress::fail_run(
            ui_run.as_ref(),
            state,
            run_id,
            error,
            provider_name,
            None,
        )
        .await;
        ChatRunOutcome::Failed
    } else {
        outcome
    };
    let outcome = crate::ui_history_ingress::finish(ui_run.as_ref(), outcome, state, run_id).await;
    let status = match &outcome {
        ChatRunOutcome::Completed(_) => "final",
        ChatRunOutcome::Cancelled => "aborted",
        ChatRunOutcome::Failed => "error",
    };
    terminal_runs.write().await.insert(run_id.to_string());
    broadcast(
        state,
        "chat",
        serde_json::json!({"runId": run_id, "sessionKey": session_key, "state": status}),
        BroadcastOpts::default(),
    )
    .await;
    outcome
}

#[cfg(test)]
mod tests {
    use {std::collections::HashMap, tokio::sync::Mutex};

    use super::*;

    #[derive(Default)]
    struct TestChatRuntime {
        broadcasts: Mutex<Vec<Value>>,
        run_errors: Mutex<HashMap<String, String>>,
    }

    #[async_trait::async_trait]
    impl ChatRuntime for TestChatRuntime {
        async fn broadcast(&self, _topic: &str, payload: Value) {
            self.broadcasts.lock().await.push(payload);
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

        async fn set_run_error(&self, run_id: &str, error: String) {
            self.run_errors
                .lock()
                .await
                .insert(run_id.to_owned(), error);
        }

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
            panic!("sandbox router is not used by this test")
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

        fn tts_service(&self) -> &dyn chelix_service_traits::TtsService {
            panic!("TTS service is not used by this test")
        }

        fn project_service(&self) -> &dyn chelix_service_traits::ProjectService {
            panic!("project service is not used by this test")
        }

        fn mcp_service(&self) -> &dyn chelix_service_traits::McpService {
            panic!("MCP service is not used by this test")
        }

        async fn chat_service(&self) -> Arc<dyn chelix_service_traits::ChatService> {
            panic!("chat service is not used by this test")
        }

        async fn last_run_error(&self, run_id: &str) -> Option<String> {
            self.run_errors.lock().await.remove(run_id)
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

    #[tokio::test]
    async fn streaming_cancellation_finalization_failure_returns_failed() {
        let directory = tempfile::tempdir()
            .unwrap_or_else(|error| panic!("temporary session directory: {error}"));
        let store = Arc::new(SessionStore::new(directory.path().to_path_buf()));
        let runtime = Arc::new(TestChatRuntime::default());
        let state: Arc<dyn ChatRuntime> = runtime.clone();
        let mut materializer = ProviderSegmentMaterializer::pending();

        let outcome = finish_streaming_cancellation(
            None,
            None,
            &state,
            Some(&store),
            None,
            &mut materializer,
            None,
            "run-1",
            "session-1",
            None,
        )
        .await;

        assert!(matches!(outcome, ChatRunOutcome::Failed));
        let run_error = runtime
            .run_errors
            .lock()
            .await
            .get("run-1")
            .cloned()
            .unwrap_or_else(|| panic!("run error is recorded"));
        assert_eq!(
            run_error,
            "assistant persistence dependencies are inconsistent"
        );
        let broadcasts = runtime.broadcasts.lock().await;
        assert!(broadcasts.is_empty());
    }
}
