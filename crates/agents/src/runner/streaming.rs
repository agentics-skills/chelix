//! Streaming variant of the agent loop.

use std::{collections::HashSet, sync::Arc};

use {
    anyhow::Result,
    tracing::{debug, info, trace, warn},
};

#[cfg(feature = "metrics")]
use chelix_metrics::{counter, histogram, labels, llm as llm_metrics};

use futures::StreamExt;

use chelix_common::{
    ContextBudgetMetadata,
    hooks::{HookAction, HookPayload, HookRegistry},
    tool_lifecycle::{ToolLifecycleEvent, ToolLifecycleUpdate},
};

use crate::{
    model::{
        AgentToolControls, ChatMessage, LlmProvider, ReasoningAccumulator, StreamEvent, ToolCall,
        ToolChoice, Usage, UserContent, decode_tool_call_arguments_from_str,
        push_capped_provider_raw_event,
    },
    response_sanitizer::{clean_response, recover_tool_calls_from_content},
    tool_loop_detector::ToolCallFingerprint,
    tool_parsing::{looks_like_failed_tool_call, parse_tool_calls_from_text},
    tool_registry::ToolRegistry,
};

use super::{
    AUTO_CONTINUE_NUDGE, AgentLoopLimits, AgentRunError, AgentRunResult, AssistantIterationOutput,
    FinalTextSource, MALFORMED_TOOL_RETRY_PROMPT, OnEvent, OnToolLifecycle, RunnerEvent,
    RunnerToolCall, RunnerToolLifecycleEvent, ToolCallBudget, ToolInvocationExecutor,
    UsageAccumulator, apply_before_llm_call_modify_payload, apply_loop_detector_intervention,
    channel_binding_from_tool_context, deliver_tool_lifecycle, dispatch_after_llm_call_hook,
    dispatch_before_agent_start_hook, empty_tool_name_retry_prompt, fallback_final_text_source,
    find_empty_tool_name_call, finish_agent_run, has_named_tool_call, is_substantive_answer_text,
    lifecycle_now_ms, record_answer_text,
    retry::{RATE_LIMIT_MAX_RETRIES, next_retry_delay_ms},
    streaming_tool_call_message_content,
};

use chelix_sessions::ToolResultStore;

use crate::tool_loop_detector::ToolLoopDetector;

async fn emit_stream_tool_lifecycle(
    callback: Option<&OnToolLifecycle>,
    tool_call_id: &str,
    tool_name: &str,
    sequence: &mut u64,
    update: ToolLifecycleUpdate,
    iteration_tool_calls: Option<Arc<[RunnerToolCall]>>,
    iteration_usage: Option<Usage>,
    context_budget: Option<ContextBudgetMetadata>,
) -> Result<(), AgentRunError> {
    let lifecycle = ToolLifecycleEvent {
        tool_call_id: tool_call_id.to_owned(),
        tool_name: tool_name.to_owned(),
        sequence: *sequence,
        emitted_at_ms: lifecycle_now_ms()?,
        run_id: None,
        context_budget: update
            .stage()
            .is_terminal()
            .then(|| context_budget.clone())
            .flatten(),
        update,
    };
    *sequence = sequence.saturating_add(1);
    let mut event = RunnerToolLifecycleEvent::new(lifecycle);
    event.iteration_tool_calls = iteration_tool_calls;
    event.iteration_usage = iteration_usage;
    event.context_budget = context_budget;
    deliver_tool_lifecycle(callback, event).await
}

async fn cancel_stream_tool_lifecycles(
    callback: Option<&OnToolLifecycle>,
    tool_calls: &[ToolCall],
    sequences: &mut std::collections::HashMap<String, u64>,
    reason: &str,
    context_budget: &ContextBudgetMetadata,
) -> Result<(), AgentRunError> {
    for tool_call in tool_calls {
        let Some(sequence) = sequences.get_mut(&tool_call.id) else {
            continue;
        };
        emit_stream_tool_lifecycle(
            callback,
            &tool_call.id,
            &tool_call.name,
            sequence,
            ToolLifecycleUpdate::Cancelled {
                arguments: None,
                reason: reason.to_owned(),
            },
            None,
            None,
            Some(context_budget.clone()),
        )
        .await?;
    }
    Ok(())
}

/// Streaming agent loop with explicit runtime limits.
///
/// Tool calls are accumulated from the stream and executed after the stream
/// completes, then the loop continues with the next iteration.
pub async fn run_agent_loop_streaming_with_limits(
    provider: Arc<dyn LlmProvider>,
    tools: &ToolRegistry,
    tools_config: &chelix_config::schema::ToolsConfig,
    system_prompt: &str,
    user_content: &UserContent,
    on_event: Option<&OnEvent>,
    on_tool_lifecycle: Option<&OnToolLifecycle>,
    history: Option<Vec<ChatMessage>>,
    tool_context: Option<serde_json::Value>,
    hook_registry: Option<Arc<HookRegistry>>,
    sender_name: Option<String>,
    steer_inbox: Option<super::SteerInbox>,
    limits: AgentLoopLimits,
) -> Result<AgentRunResult, AgentRunError> {
    let native_tools = provider.supports_tools();
    let max_tool_result_bytes = limits
        .max_tool_result_bytes
        .unwrap_or(tools_config.max_tool_result_bytes);
    let max_auto_continues = tools_config.agent_max_auto_continues;
    let auto_continue_min_tool_calls = tools_config.agent_auto_continue_min_tool_calls;

    let is_multimodal = matches!(user_content, UserContent::Multimodal(_));
    info!(
        provider = provider.name(),
        model = provider.id(),
        native_tools,
        tools_count = tools.list_names().len(),
        is_multimodal,
        "starting streaming agent loop"
    );
    let context_window = provider.context_window().ok_or_else(|| {
        anyhow::anyhow!(
            "model '{}' has no resolved context metadata; use a registry provider",
            provider.id()
        )
    })?;
    let max_input_tokens = provider.max_input_tokens().ok_or_else(|| {
        anyhow::anyhow!(
            "model '{}' has no resolved max_input_tokens metadata; use a registry provider",
            provider.id()
        )
    })?;
    let max_output_tokens = provider.max_output_tokens().ok_or_else(|| {
        anyhow::anyhow!(
            "model '{}' has no resolved max_output_tokens metadata; use a registry provider",
            provider.id()
        )
    })?;

    let mut messages: Vec<ChatMessage> = vec![ChatMessage::system(system_prompt)];
    let history_len = history.as_ref().map_or(0, Vec::len);

    // Insert conversation history before the current user message.
    if let Some(hist) = history {
        messages.extend(hist);
    }

    if !limits.resume_from_history {
        messages.push(ChatMessage::User {
            content: user_content.clone(),
            name: sender_name,
        });
    }
    let mut compaction_continuation_start = if limits.resume_from_history {
        if history_len > 1 {
            2
        } else {
            messages.len()
        }
    } else {
        1 + history_len
    };
    let mut continuation_tool_rounds = messages[compaction_continuation_start..]
        .iter()
        .filter(|message| {
            matches!(message, ChatMessage::Assistant { tool_calls, .. } if !tool_calls.is_empty())
        })
        .count();
    // Extract session key once for hook payloads.
    let session_key_for_hooks = tool_context
        .as_ref()
        .and_then(|ctx| ctx.get("_session_key"))
        .and_then(|v| v.as_str())
        .unwrap_or("main")
        .to_string();
    let channel_for_hooks =
        channel_binding_from_tool_context(&session_key_for_hooks, tool_context.as_ref());

    // Every agent-facing tool result is persisted before it enters LLM context.
    let tool_result_store = ToolResultStore::new(chelix_config::data_dir().join("sessions"));

    dispatch_before_agent_start_hook(
        hook_registry.as_ref(),
        &session_key_for_hooks,
        provider.id(),
    )
    .await?;

    let mut iterations = 0;
    let mut tool_call_budget = ToolCallBudget::new(limits.max_tools_threshold);
    let mut usage_accumulator = UsageAccumulator::default();
    let mut server_retries_remaining: u8 = 1;
    let mut rate_limit_retries_remaining: u8 = RATE_LIMIT_MAX_RETRIES;
    let mut rate_limit_backoff_ms: Option<u64> = None;
    let mut raw_llm_responses: Vec<serde_json::Value> = Vec::new();
    // Track answer text from iterations that also contained tool calls.
    // When the final iteration is empty (e.g. model stop after browser close),
    // this is used as the final response text instead of returning silent.
    let mut last_answer_text = String::new();
    let mut last_answer_tool_call_id: Option<String> = None;
    let mut malformed_retry_count: u8 = 0;
    let mut empty_tool_name_retry_count: u8 = 0;
    let mut auto_continue_count: usize = 0;
    let mut loop_detector = ToolLoopDetector::new(
        tools_config.agent_loop_detector_window,
        tools_config.agent_loop_detector_strip_tools_on_second_fire,
    );
    let mut strip_tools_next_iter = false;
    let tool_controls = AgentToolControls::from_tool_context(tool_context.as_ref());
    let active_tool_names = tool_controls
        .active_tools
        .as_ref()
        .map(|names| names.iter().cloned().collect::<HashSet<_>>());

    loop {
        iterations += 1;

        // Re-compute schemas each iteration so schemas revealed via get_tool appear immediately.
        // When the loop detector has escalated to stage 2, do not send tools
        // for this single turn so the model is forced to respond in text
        // (issue #658).
        let schemas_for_api = if native_tools && !strip_tools_next_iter {
            let schemas = if let Some(active) = active_tool_names.as_ref() {
                tools.list_schemas_allowed_by(|name| active.contains(name))
            } else {
                tools.list_schemas()
            };
            match tool_controls.tool_choice.as_ref() {
                Some(ToolChoice::None) => vec![],
                Some(ToolChoice::Any) if schemas.is_empty() => {
                    return Err(AgentRunError::Other(anyhow::anyhow!(
                        "tool_choice any requires at least one active tool"
                    )));
                },
                Some(ToolChoice::Tool { name }) => {
                    if !schemas.iter().any(|schema| {
                        schema.get("name").and_then(serde_json::Value::as_str) == Some(name)
                    }) {
                        return Err(AgentRunError::Other(anyhow::anyhow!(
                            "forced tool_choice references unavailable tool: {name}"
                        )));
                    }
                    schemas
                },
                _ => schemas,
            }
        } else {
            vec![]
        };
        if strip_tools_next_iter {
            strip_tools_next_iter = false;
            loop_detector.clear_strip_tools();
        }

        if let Some(cb) = on_event {
            cb(RunnerEvent::Iteration(iterations));
        }

        info!(
            iteration = iterations,
            messages_count = messages.len(),
            "calling LLM (streaming)"
        );
        trace!(iteration = iterations, messages = ?messages, "LLM request messages");

        // Dispatch BeforeLLMCall hook — may block the LLM call.
        if let Some(ref hooks) = hook_registry {
            let msgs_json: Vec<serde_json::Value> =
                messages.iter().map(|m| m.to_openai_value()).collect();
            let payload = HookPayload::BeforeLLMCall {
                session_key: session_key_for_hooks.clone(),
                provider: provider.name().to_string(),
                model: provider.id().to_string(),
                messages: serde_json::Value::Array(msgs_json),
                tool_count: schemas_for_api.len(),
                iteration: iterations,
            };
            match hooks.dispatch(&payload).await {
                Ok(HookAction::Block(reason)) => {
                    warn!(reason = %reason, "LLM call blocked by BeforeLLMCall hook");
                    return Err(AgentRunError::Other(anyhow::anyhow!(
                        "blocked by BeforeLLMCall hook: {reason}"
                    )));
                },
                Ok(HookAction::ModifyPayload(modified_payload)) => {
                    apply_before_llm_call_modify_payload(&mut messages, modified_payload)?;
                },
                Ok(HookAction::Continue) => {},
                Err(e) => {
                    warn!(error = %e, "BeforeLLMCall hook dispatch failed");
                },
            }
        }

        let context_budget = super::evaluate_context_budget(
            &messages,
            &schemas_for_api,
            context_window,
            max_input_tokens,
            max_output_tokens,
        );
        if super::should_trigger_automatic_checkpoint(&limits, iterations, &context_budget) {
            let (summary_messages, continuation_messages) =
                super::split_context_for_compaction(messages, compaction_continuation_start);
            return Err(AgentRunError::ContextCompactionRequired(Box::new(
                super::ContextCompactionRequest {
                    metadata: context_budget,
                    summary_messages,
                    continuation_messages,
                    tool_schemas: schemas_for_api,
                    completed_iterations: iterations.saturating_sub(1),
                    tool_calls_made: tool_call_budget.used(),
                    usage: usage_accumulator.total(),
                    raw_llm_responses,
                },
            )));
        }

        if let Some(cb) = on_event {
            cb(RunnerEvent::Thinking);
        }

        // Use streaming API.
        #[cfg(feature = "metrics")]
        let iter_start = std::time::Instant::now();
        let mut stream = if schemas_for_api.is_empty() {
            provider.stream(messages.clone())
        } else {
            provider.stream_with_tools_and_options(
                messages.clone(),
                schemas_for_api.clone(),
                tool_controls.clone(),
            )
        };

        // Accumulate answer text, reasoning text, and tool calls from the stream.
        let mut accumulated_text = String::new();
        let mut accumulated_reasoning = ReasoningAccumulator::default();
        let mut responses_reasoning = Vec::new();
        let mut tool_calls: Vec<ToolCall> = Vec::new();
        // Map streaming index -> accumulated JSON args string.
        let mut tool_call_args: std::collections::HashMap<usize, String> =
            std::collections::HashMap::new();
        // Map streaming index -> position in the `tool_calls` vec.
        // The streaming index may not start at 0. Some providers use the
        // content-block index, so a text block at index 0 pushes the tool_use
        // to index 1.
        let mut stream_idx_to_vec_pos: std::collections::HashMap<usize, usize> =
            std::collections::HashMap::new();
        let mut tool_lifecycle_sequences: std::collections::HashMap<String, u64> =
            std::collections::HashMap::new();
        let mut request_usage = Usage::default();
        let mut stream_error: Option<String> = None;

        while let Some(event) = stream.next().await {
            match event {
                StreamEvent::Delta(text) => {
                    accumulated_text.push_str(&text);
                    if let Some(cb) = on_event {
                        cb(RunnerEvent::TextDelta(text.clone()));
                        cb(RunnerEvent::FinalText(text));
                    }
                },
                StreamEvent::ProviderRaw(raw) => {
                    push_capped_provider_raw_event(&mut raw_llm_responses, raw);
                },
                StreamEvent::ReasoningDelta(text) => {
                    accumulated_reasoning.append_text(&text);
                    if let Some(cb) = on_event
                        && let Some(chelix_common::ReasoningContent::Text(text)) =
                            accumulated_reasoning.content()
                    {
                        cb(RunnerEvent::ThinkingText(text));
                    }
                },
                StreamEvent::ResponsesReasoningDelta {
                    item_id,
                    output_index,
                    summary_index,
                    delta,
                } => {
                    accumulated_reasoning.append_responses_delta(
                        &item_id,
                        output_index,
                        summary_index,
                        &delta,
                    );
                    if let Some(cb) = on_event {
                        cb(RunnerEvent::ResponsesReasoningDelta {
                            item_id,
                            output_index,
                            summary_index,
                            delta,
                        });
                    }
                },
                StreamEvent::ResponsesReasoningPartDone {
                    item_id,
                    output_index,
                    summary_index,
                    text,
                } => {
                    accumulated_reasoning.complete_responses_part(
                        &item_id,
                        output_index,
                        summary_index,
                        text.clone(),
                    );
                    if let Some(cb) = on_event {
                        cb(RunnerEvent::ResponsesReasoningPartDone {
                            item_id,
                            output_index,
                            summary_index,
                            text,
                        });
                    }
                },
                StreamEvent::ResponsesReasoningItem(item) => {
                    responses_reasoning.push(item.clone());
                    if let Some(cb) = on_event {
                        cb(RunnerEvent::ResponsesReasoningItem(item));
                    }
                },
                StreamEvent::ToolCallStart { id, name, index } => {
                    let vec_pos = tool_calls.len();
                    debug!(tool = %name, id = %id, stream_index = index, vec_pos, "tool call started in stream");
                    let mut sequence = 0;
                    emit_stream_tool_lifecycle(
                        on_tool_lifecycle,
                        &id,
                        &name,
                        &mut sequence,
                        ToolLifecycleUpdate::Created {
                            provider_index: Some(index),
                        },
                        None,
                        None,
                        Some(context_budget.clone()),
                    )
                    .await?;
                    tool_lifecycle_sequences.insert(id.clone(), sequence);
                    tool_calls.push(ToolCall {
                        id,
                        name,
                        arguments: serde_json::json!({}),
                        argument_diagnostic: None,
                    });
                    stream_idx_to_vec_pos.insert(index, vec_pos);
                    tool_call_args.insert(index, String::new());
                },
                StreamEvent::ToolCallArgumentsDelta { index, delta } => {
                    let Some(args) = tool_call_args.get_mut(&index) else {
                        continue;
                    };
                    args.push_str(&delta);
                    let Some(&vec_pos) = stream_idx_to_vec_pos.get(&index) else {
                        continue;
                    };
                    let Some(tool_call) = tool_calls.get(vec_pos) else {
                        continue;
                    };
                    let Some(sequence) = tool_lifecycle_sequences.get_mut(&tool_call.id) else {
                        continue;
                    };
                    emit_stream_tool_lifecycle(
                        on_tool_lifecycle,
                        &tool_call.id,
                        &tool_call.name,
                        sequence,
                        ToolLifecycleUpdate::InputStreaming {
                            arguments_delta: delta,
                        },
                        None,
                        None,
                        Some(context_budget.clone()),
                    )
                    .await?;
                },
                StreamEvent::ToolCallComplete { index } => {
                    // Arguments are finalized after stream completes.
                    // Just log for now - we'll parse accumulated args later.
                    debug!(index, "tool call arguments complete");
                },
                StreamEvent::Done(usage) => {
                    request_usage = usage.clone();
                    debug!(
                        input_tokens = request_usage.input_tokens,
                        output_tokens = request_usage.output_tokens,
                        cache_read_tokens = request_usage.cache_read_tokens,
                        cache_write_tokens = request_usage.cache_write_tokens,
                        "stream done"
                    );

                    #[cfg(feature = "metrics")]
                    {
                        let provider_name = provider.name().to_string();
                        let model_id = provider.id().to_string();
                        let duration = iter_start.elapsed().as_secs_f64();
                        counter!(
                            llm_metrics::COMPLETIONS_TOTAL,
                            labels::PROVIDER => provider_name.clone(),
                            labels::MODEL => model_id.clone()
                        )
                        .increment(1);
                        counter!(
                            llm_metrics::INPUT_TOKENS_TOTAL,
                            labels::PROVIDER => provider_name.clone(),
                            labels::MODEL => model_id.clone()
                        )
                        .increment(u64::from(usage.input_tokens));
                        counter!(
                            llm_metrics::OUTPUT_TOKENS_TOTAL,
                            labels::PROVIDER => provider_name.clone(),
                            labels::MODEL => model_id.clone()
                        )
                        .increment(u64::from(usage.output_tokens));
                        counter!(
                            llm_metrics::CACHE_READ_TOKENS_TOTAL,
                            labels::PROVIDER => provider_name.clone(),
                            labels::MODEL => model_id.clone()
                        )
                        .increment(u64::from(usage.cache_read_tokens));
                        counter!(
                            llm_metrics::CACHE_WRITE_TOKENS_TOTAL,
                            labels::PROVIDER => provider_name.clone(),
                            labels::MODEL => model_id.clone()
                        )
                        .increment(u64::from(usage.cache_write_tokens));
                        histogram!(
                            llm_metrics::COMPLETION_DURATION_SECONDS,
                            labels::PROVIDER => provider_name,
                            labels::MODEL => model_id
                        )
                        .record(duration);
                    }
                },
                StreamEvent::Error(msg) => {
                    stream_error = Some(msg);
                    break;
                },
            }
        }

        if let Some(cb) = on_event {
            cb(RunnerEvent::ThinkingDone);
        }

        if let Some(reason) = stream_error.as_ref() {
            cancel_stream_tool_lifecycles(
                on_tool_lifecycle,
                &tool_calls,
                &mut tool_lifecycle_sequences,
                &format!("provider stream failed: {reason}"),
                &context_budget,
            )
            .await?;
        }

        // Handle stream errors — retry on transient failures/rate limits.
        if let Some(err) = stream_error {
            if let Some(delay_ms) = next_retry_delay_ms(
                &err,
                &mut server_retries_remaining,
                &mut rate_limit_retries_remaining,
                &mut rate_limit_backoff_ms,
            ) {
                // Don't count the failed attempt as an iteration.
                iterations -= 1;
                warn!(
                    error = %err,
                    delay_ms,
                    server_retries_remaining,
                    rate_limit_retries_remaining,
                    "transient LLM error, retrying after delay"
                );
                if let Some(cb) = on_event {
                    cb(RunnerEvent::RetryingAfterError {
                        error: err,
                        delay_ms,
                    });
                }
                tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
                continue;
            }
            return Err(AgentRunError::Other(anyhow::anyhow!(err)));
        }

        usage_accumulator.record_request(request_usage.clone());

        // Finalize tool call arguments from accumulated strings.
        // Use stream_idx_to_vec_pos to map streaming indices (which may not
        // start at 0) to the actual position in the tool_calls vec.
        for (stream_idx, args_str) in &tool_call_args {
            // Emit raw accumulated string at debug level so future variants of
            // "default to {} because no deltas arrived" can be diagnosed
            // without a repro (issue #658).
            debug!(
                stream_idx,
                args_str = %args_str,
                "finalizing tool call args"
            );
            if let Some(&vec_pos) = stream_idx_to_vec_pos.get(stream_idx)
                && vec_pos < tool_calls.len()
            {
                let decoded = decode_tool_call_arguments_from_str(args_str);
                tool_calls[vec_pos].arguments = decoded.arguments;
                tool_calls[vec_pos].argument_diagnostic = decoded.diagnostic;
            }
        }

        info!(
            iteration = iterations,
            has_text = !accumulated_text.is_empty(),
            tool_calls_count = tool_calls.len(),
            input_tokens = request_usage.input_tokens,
            output_tokens = request_usage.output_tokens,
            cache_read_tokens = request_usage.cache_read_tokens,
            cache_write_tokens = request_usage.cache_write_tokens,
            "streaming LLM response complete"
        );

        // Fallback: parse tool calls from model text if the provider returned
        // no structured tool calls (some providers/models emit text-based calls).
        if tool_calls.is_empty() && !accumulated_text.is_empty() {
            let (parsed, remaining) = parse_tool_calls_from_text(&accumulated_text);
            if !parsed.is_empty() {
                info!(
                    native_tools,
                    count = parsed.len(),
                    first_tool = %parsed[0].name,
                    "parsed tool call(s) from text fallback"
                );
                accumulated_text = remaining.unwrap_or_default();
                tool_calls = parsed;
            }
        }

        // One-shot retry for malformed tool calls in streaming mode.
        if tool_calls.is_empty()
            && looks_like_failed_tool_call(&Some(accumulated_text.clone()))
            && malformed_retry_count == 0
        {
            malformed_retry_count += 1;
            info!("detected malformed tool call in stream, requesting retry");
            messages.push(
                ChatMessage::assistant(&accumulated_text)
                    .with_reasoning(accumulated_reasoning.content())
                    .with_responses_reasoning(responses_reasoning),
            );
            messages.push(ChatMessage::user(MALFORMED_TOOL_RETRY_PROMPT));
            continue;
        }

        // Fallback: recover tool calls from XML blocks (<function_call>, <tool_call>).
        if !native_tools && tool_calls.is_empty() && !accumulated_text.is_empty() {
            let (cleaned, recovered) = recover_tool_calls_from_content(&accumulated_text);
            if !recovered.is_empty() {
                info!(
                    count = recovered.len(),
                    "recovered tool calls from XML blocks in streamed text"
                );
                accumulated_text = cleaned;
                tool_calls = recovered;
            }
        }

        if let Err(error) = tool_call_budget.reserve_batch(tool_calls.len()) {
            cancel_stream_tool_lifecycles(
                on_tool_lifecycle,
                &tool_calls,
                &mut tool_lifecycle_sequences,
                &error.to_string(),
                &context_budget,
            )
            .await?;
            return Err(error);
        }

        if let Some(tc) = find_empty_tool_name_call(&tool_calls) {
            if has_named_tool_call(&tool_calls) {
                warn!(
                    tool_call_id = %tc.id,
                    "streamed tool call batch contains both empty and valid tool names; preserving valid sibling tool calls and falling back to normal tool error handling"
                );
            } else if empty_tool_name_retry_count == 0 {
                empty_tool_name_retry_count += 1;
                info!(tool_call_id = %tc.id, "detected structured tool call with empty name in stream, requesting retry");
                let retry_text = streaming_tool_call_message_content(
                    &mut last_answer_text,
                    &mut last_answer_tool_call_id,
                    &accumulated_text,
                );
                messages.push(
                    ChatMessage::assistant(retry_text.unwrap_or_default())
                        .with_reasoning(accumulated_reasoning.content())
                        .with_responses_reasoning(responses_reasoning),
                );
                messages.push(ChatMessage::user(empty_tool_name_retry_prompt(tc)));
                cancel_stream_tool_lifecycles(
                    on_tool_lifecycle,
                    &tool_calls,
                    &mut tool_lifecycle_sequences,
                    "structured tool call has an empty tool name; requesting provider retry",
                    &context_budget,
                )
                .await?;
                continue;
            }
            warn!(
                tool_call_id = %tc.id,
                "structured tool call in stream still has empty name after retry; falling back to normal tool error handling"
            );
        }

        if let Err(error) = dispatch_after_llm_call_hook(
            hook_registry.as_ref(),
            &session_key_for_hooks,
            provider.name(),
            provider.id(),
            (!accumulated_text.is_empty()).then(|| accumulated_text.clone()),
            &tool_calls,
            &request_usage,
            iterations,
        )
        .await
        {
            cancel_stream_tool_lifecycles(
                on_tool_lifecycle,
                &tool_calls,
                &mut tool_lifecycle_sequences,
                &error.to_string(),
                &context_budget,
            )
            .await?;
            return Err(error);
        }

        // If no tool calls, auto-continue or return the text response.
        if tool_calls.is_empty() {
            // Auto-continue: if the model made tool calls earlier in this run
            // and we haven't exhausted nudges, ask it to keep going. Suppress
            // the nudge when the model already produced a substantive final
            // answer — nudging in that case risks losing the answer (GH #628).
            let has_reasoning_output =
                !accumulated_reasoning.is_empty() || !responses_reasoning.is_empty();
            if !is_substantive_answer_text(&accumulated_text)
                && !has_reasoning_output
                && tool_call_budget.used() > 0
                && tool_call_budget.used() >= auto_continue_min_tool_calls
                && auto_continue_count < max_auto_continues
            {
                auto_continue_count += 1;
                info!(
                    iterations,
                    auto_continue_count, "model stopped without tool calls, auto-continuing"
                );
                if let Some(cb) = on_event {
                    cb(RunnerEvent::AutoContinue {
                        iteration: iterations,
                    });
                }
                let reasoning = accumulated_reasoning.content();
                if !accumulated_text.is_empty()
                    || reasoning.is_some()
                    || !responses_reasoning.is_empty()
                {
                    messages.push(
                        ChatMessage::assistant_with_tools(
                            (!accumulated_text.is_empty()).then(|| accumulated_text.clone()),
                            Vec::new(),
                        )
                        .with_reasoning(reasoning)
                        .with_responses_reasoning(responses_reasoning),
                    );
                }
                messages.push(ChatMessage::user(AUTO_CONTINUE_NUDGE));
                continue;
            }

            let current_output = AssistantIterationOutput {
                text: accumulated_text,
                reasoning: accumulated_reasoning.content(),
                responses_reasoning,
            };
            // A previous tool iteration can own the display answer only when the
            // current provider iteration returned no output parts at all.
            let used_fallback_text =
                !current_output.has_provider_output() && !last_answer_text.is_empty();
            let final_output = if used_fallback_text {
                AssistantIterationOutput {
                    text: std::mem::take(&mut last_answer_text),
                    ..AssistantIterationOutput::default()
                }
            } else {
                current_output
            };
            let final_text = final_output.text.clone();
            if used_fallback_text {
                let streamed_final_text = clean_response(&final_text);
                if let Some(cb) = on_event
                    && !streamed_final_text.is_empty()
                {
                    cb(RunnerEvent::FinalText(streamed_final_text));
                }
            }
            info!(
                iterations,
                tool_calls = tool_call_budget.used(),
                "streaming agent loop complete — returning text"
            );
            return Ok(finish_agent_run(
                final_output,
                if used_fallback_text {
                    fallback_final_text_source(last_answer_tool_call_id)
                } else {
                    FinalTextSource::NewSegment
                },
                iterations,
                tool_call_budget.used(),
                &usage_accumulator,
                raw_llm_responses,
            ));
        }

        // Persist every output part on the assistant frame owned by this
        // provider iteration before executing its tool calls.
        let text_for_msg = (!accumulated_text.is_empty()).then(|| accumulated_text.clone());
        let reasoning_for_msg = accumulated_reasoning.content();
        if let Some(ref text) = text_for_msg {
            record_answer_text(
                &mut last_answer_text,
                &mut last_answer_tool_call_id,
                Some(text),
                &tool_calls,
            );
            if let Some(cb) = on_event {
                cb(RunnerEvent::ProgressText(text.clone()));
            }
        }
        if continuation_tool_rounds > 0 {
            compaction_continuation_start = messages.len();
        }
        continuation_tool_rounds += 1;
        messages.push(
            ChatMessage::assistant_with_tools(text_for_msg, tool_calls.clone())
                .with_reasoning(reasoning_for_msg)
                .with_responses_reasoning(responses_reasoning),
        );

        // Publish the canonical assistant tool-call frame before execution.
        let iteration_tool_calls: Arc<[RunnerToolCall]> =
            tool_calls.iter().map(Into::into).collect::<Vec<_>>().into();
        for tool_call in &tool_calls {
            let existing_sequence = tool_lifecycle_sequences.remove(&tool_call.id);
            let mut sequence = existing_sequence.unwrap_or(0);
            if existing_sequence.is_none() {
                emit_stream_tool_lifecycle(
                    on_tool_lifecycle,
                    &tool_call.id,
                    &tool_call.name,
                    &mut sequence,
                    ToolLifecycleUpdate::Created {
                        provider_index: None,
                    },
                    None,
                    None,
                    Some(context_budget.clone()),
                )
                .await?;
            }
            emit_stream_tool_lifecycle(
                on_tool_lifecycle,
                &tool_call.id,
                &tool_call.name,
                &mut sequence,
                ToolLifecycleUpdate::InputReady {
                    arguments: tool_call.arguments.clone(),
                },
                Some(iteration_tool_calls.clone()),
                Some(request_usage.clone()),
                Some(context_budget.clone()),
            )
            .await?;
            tool_lifecycle_sequences.insert(tool_call.id.clone(), sequence);
        }

        // Execute all built-in and MCP tools through the shared lifecycle executor.
        let executor = ToolInvocationExecutor {
            tools,
            tool_result_store: &tool_result_store,
            max_tool_result_bytes,
            tool_context: tool_context.as_ref(),
            hook_registry: hook_registry.as_ref(),
            session_key: &session_key_for_hooks,
            channel: channel_for_hooks.as_ref(),
            active_tool_names: active_tool_names.as_ref(),
            tool_choice: tool_controls.tool_choice.as_ref(),
            on_lifecycle: on_tool_lifecycle,
            context_budget: &context_budget,
        };
        let mut tool_futures = Vec::with_capacity(tool_calls.len());
        for tool_call in &tool_calls {
            let sequence = tool_lifecycle_sequences
                .get(&tool_call.id)
                .copied()
                .ok_or_else(|| {
                    AgentRunError::Other(anyhow::anyhow!(
                        "tool lifecycle sequence missing for call {}",
                        tool_call.id
                    ))
                })?;
            tool_futures.push(executor.execute(tool_call, sequence));
        }
        let results = futures::future::join_all(tool_futures)
            .await
            .into_iter()
            .collect::<Result<Vec<_>, AgentRunError>>()?;

        let mut round_outcomes = Vec::with_capacity(tool_calls.len());
        for (tool_call, outcome) in tool_calls.iter().zip(results) {
            if outcome.success {
                info!(tool = %tool_call.name, id = %tool_call.id, "tool execution succeeded");
                trace!(tool = %tool_call.name, result = %outcome.result, "tool result");
            } else if outcome.rejected {
                warn!(
                    tool = %tool_call.name,
                    id = %tool_call.id,
                    "tool call rejected before execution by pre-dispatch validation"
                );
            } else {
                warn!(
                    tool = %tool_call.name,
                    id = %tool_call.id,
                    error = %outcome.error.as_deref().unwrap_or(""),
                    "tool execution failed"
                );
            }

            if loop_detector.is_enabled() {
                round_outcomes.push(if outcome.success {
                    ToolCallFingerprint::success(&tool_call.name, &tool_call.arguments)
                } else {
                    ToolCallFingerprint::failure(
                        &tool_call.name,
                        &tool_call.arguments,
                        outcome.error.as_deref(),
                    )
                });
            }

            debug!(
                tool = %tool_call.name,
                id = %tool_call.id,
                result_len = outcome.result.len(),
                "appending tool result to messages"
            );
            trace!(tool = %tool_call.name, content = %outcome.result, "tool result message content");
            messages.push(ChatMessage::tool(&tool_call.id, &outcome.result));
        }

        // Record and act on the complete LLM tool-call batch exactly once.
        let loop_action = loop_detector.record_round(&round_outcomes);
        apply_loop_detector_intervention(
            &loop_detector,
            loop_action,
            &mut messages,
            &mut strip_tools_next_iter,
            on_event,
        );

        // Drain any pending /steer text and inject as a system note.
        // Uses system role to avoid consecutive-user-message violations
        // with strict providers that enforce role alternation.
        if let Some(ref inbox) = steer_inbox {
            let mut guard = inbox.lock().await;
            if !guard.is_empty() {
                let combined = guard.drain(..).collect::<Vec<_>>().join("\n");
                debug!(steer_text = %combined, "injecting /steer guidance");
                messages.push(ChatMessage::system(format!(
                    "[Steering note from the user — adjust your approach accordingly]: {combined}"
                )));
            }
        }
    }
}
