//! Non-streaming agent loop with explicit runtime limits.

use std::{collections::HashSet, sync::Arc};

use {
    anyhow::Result,
    tracing::{debug, info, trace, warn},
};

use chelix_common::{
    ContextBudgetMetadata,
    hooks::{HookAction, HookPayload, HookRegistry},
    tool_lifecycle::{ToolLifecycleEvent, ToolLifecycleUpdate},
};

use crate::{
    model::{
        AgentToolControls, ChatMessage, CompletionOptions, LlmProvider, ToolChoice, UserContent,
    },
    response_sanitizer::recover_tool_calls_from_content,
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
};

use chelix_sessions::ToolResultStore;

use crate::tool_loop_detector::ToolLoopDetector;

async fn emit_non_stream_tool_lifecycle(
    callback: Option<&OnToolLifecycle>,
    tool_call_id: &str,
    tool_name: &str,
    sequence: &mut u64,
    update: ToolLifecycleUpdate,
    iteration_tool_calls: Option<Arc<[RunnerToolCall]>>,
    iteration_usage: Option<crate::model::Usage>,
    context_budget: &ContextBudgetMetadata,
) -> Result<(), AgentRunError> {
    let lifecycle = ToolLifecycleEvent {
        tool_call_id: tool_call_id.to_owned(),
        tool_name: tool_name.to_owned(),
        sequence: *sequence,
        emitted_at_ms: lifecycle_now_ms()?,
        run_id: None,
        context_budget: None,
        update,
    };
    *sequence = sequence.saturating_add(1);
    let mut event = RunnerToolLifecycleEvent::new(lifecycle);
    event.iteration_tool_calls = iteration_tool_calls;
    event.iteration_usage = iteration_usage;
    event.context_budget = Some(context_budget.clone());
    deliver_tool_lifecycle(callback, event).await
}

pub async fn run_agent_loop_with_context_and_limits(
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
        "starting agent loop"
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
        // for this single turn so the model is forced to respond in text.
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
            "calling LLM"
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
                    raw_llm_responses: Vec::new(),
                },
            )));
        }

        if let Some(cb) = on_event {
            cb(RunnerEvent::Thinking);
        }

        let completion_result = if schemas_for_api.is_empty() {
            provider.complete(&messages, &[]).await
        } else {
            let completion_options = CompletionOptions::from(tool_controls.clone());
            provider
                .complete_with_options(&messages, &schemas_for_api, &completion_options)
                .await
        };

        let mut response = match completion_result {
            Ok(r) => r,
            Err(e) => {
                let msg = e.to_string();
                if let Some(delay_ms) = next_retry_delay_ms(
                    &msg,
                    &mut server_retries_remaining,
                    &mut rate_limit_retries_remaining,
                    &mut rate_limit_backoff_ms,
                ) {
                    iterations -= 1;
                    warn!(
                        error = %msg,
                        delay_ms,
                        server_retries_remaining,
                        rate_limit_retries_remaining,
                        "transient LLM error, retrying after delay"
                    );
                    if let Some(cb) = on_event {
                        cb(RunnerEvent::RetryingAfterError {
                            error: msg,
                            delay_ms,
                        });
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
                    continue;
                }
                return Err(AgentRunError::Other(e));
            },
        };

        if let Some(cb) = on_event {
            cb(RunnerEvent::ThinkingDone);
        }

        usage_accumulator.record_request(response.usage.clone());

        info!(
            iteration = iterations,
            has_text = response.text.is_some(),
            tool_calls_count = response.tool_calls.len(),
            input_tokens = response.usage.input_tokens,
            output_tokens = response.usage.output_tokens,
            "LLM response received"
        );
        if let Some(ref text) = response.text {
            trace!(iteration = iterations, text = %text, "LLM response text");
        }

        // Fallback: parse tool calls from model text if the provider returned
        // no structured tool calls (some providers/models emit text-based calls).
        if response.tool_calls.is_empty()
            && let Some(ref text) = response.text
        {
            let (parsed, remaining) = parse_tool_calls_from_text(text);
            if !parsed.is_empty() {
                info!(
                    native_tools,
                    count = parsed.len(),
                    first_tool = %parsed[0].name,
                    "parsed tool call(s) from text fallback"
                );
                response.text = remaining;
                response.tool_calls = parsed;
            }
        }

        // One-shot retry for malformed tool calls: if the text looks like a
        // failed tool call attempt, ask the model to retry with exact format.
        if response.tool_calls.is_empty()
            && looks_like_failed_tool_call(&response.text)
            && malformed_retry_count == 0
        {
            malformed_retry_count += 1;
            info!("detected malformed tool call, requesting retry");
            messages.push(ChatMessage::assistant(
                response.text.as_deref().unwrap_or(""),
            ));
            messages.push(ChatMessage::user(MALFORMED_TOOL_RETRY_PROMPT));
            continue;
        }

        // Fallback: recover tool calls from XML blocks (<function_call>, <tool_call>).
        if !native_tools
            && response.tool_calls.is_empty()
            && let Some(ref text) = response.text
        {
            let (cleaned, recovered) = recover_tool_calls_from_content(text);
            if !recovered.is_empty() {
                info!(
                    count = recovered.len(),
                    "recovered tool calls from XML blocks in response text"
                );
                response.text = if cleaned.is_empty() {
                    None
                } else {
                    Some(cleaned)
                };
                response.tool_calls = recovered;
            }
        }

        tool_call_budget.reserve_batch(response.tool_calls.len())?;

        if let Some(tc) = find_empty_tool_name_call(&response.tool_calls) {
            if has_named_tool_call(&response.tool_calls) {
                warn!(
                    tool_call_id = %tc.id,
                    "structured tool call batch contains both empty and valid tool names; preserving valid sibling tool calls and falling back to normal tool error handling"
                );
            } else if empty_tool_name_retry_count == 0 {
                empty_tool_name_retry_count += 1;
                info!(tool_call_id = %tc.id, "detected structured tool call with empty name, requesting retry");
                record_answer_text(
                    &mut last_answer_text,
                    &mut last_answer_tool_call_id,
                    response.text.as_deref(),
                    &[],
                );
                messages.push(ChatMessage::assistant(
                    response.text.as_deref().unwrap_or(""),
                ));
                messages.push(ChatMessage::user(empty_tool_name_retry_prompt(tc)));
                continue;
            }
            warn!(
                tool_call_id = %tc.id,
                "structured tool call still has empty name after retry; falling back to normal tool error handling"
            );
        }

        for tc in &response.tool_calls {
            info!(
                iteration = iterations,
                tool_name = %tc.name,
                arguments = %tc.arguments,
                "LLM requested tool call"
            );
        }

        // Dispatch AfterLLMCall hook — may block tool execution.
        dispatch_after_llm_call_hook(
            hook_registry.as_ref(),
            &session_key_for_hooks,
            provider.name(),
            provider.id(),
            response.text.clone(),
            &response.tool_calls,
            &response.usage,
            iterations,
        )
        .await?;

        // If no tool calls, auto-continue or return the text response.
        if response.tool_calls.is_empty() {
            let response_text = response
                .text
                .clone()
                .filter(|t| !t.is_empty())
                .unwrap_or_default();

            // Auto-continue: if the model made tool calls earlier in this run
            // and we haven't exhausted nudges, ask it to keep going. Suppress
            // the nudge when the model already produced a substantive final
            // answer — nudging in that case risks losing the answer (GH #628).
            if !is_substantive_answer_text(&response_text)
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
                if !response_text.is_empty() {
                    messages.push(ChatMessage::assistant(&response_text));
                }
                messages.push(ChatMessage::user(AUTO_CONTINUE_NUDGE));
                continue;
            }

            let used_fallback_text = response_text.is_empty() && !last_answer_text.is_empty();
            let text = if !response_text.is_empty() {
                response_text
            } else {
                std::mem::take(&mut last_answer_text)
            };

            info!(
                iterations,
                tool_calls = tool_call_budget.used(),
                "agent loop complete — returning text"
            );
            return Ok(finish_agent_run(
                AssistantIterationOutput {
                    text,
                    ..AssistantIterationOutput::default()
                },
                if used_fallback_text {
                    fallback_final_text_source(last_answer_tool_call_id)
                } else {
                    FinalTextSource::NewSegment
                },
                iterations,
                tool_call_budget.used(),
                &usage_accumulator,
                Vec::new(),
            ));
        }

        // Append assistant message with tool calls.
        // Save any answer text for fallback — when the final iteration returns
        // empty, this becomes the result. Don't emit as ThinkingText because
        // it may be the actual answer (e.g. a table produced before a cleanup
        // tool call like `browser close`).
        record_answer_text(
            &mut last_answer_text,
            &mut last_answer_tool_call_id,
            response.text.as_deref(),
            &response.tool_calls,
        );
        if continuation_tool_rounds > 0 {
            compaction_continuation_start = messages.len();
        }
        continuation_tool_rounds += 1;
        messages.push(ChatMessage::assistant_with_tools(
            response.text.clone(),
            response.tool_calls.clone(),
        ));

        // Publish the complete invocation before execution.
        let iteration_tool_calls: Arc<[RunnerToolCall]> = response
            .tool_calls
            .iter()
            .map(Into::into)
            .collect::<Vec<_>>()
            .into();
        let mut tool_lifecycle_sequences = std::collections::HashMap::new();
        for tool_call in &response.tool_calls {
            let mut sequence = 0;
            emit_non_stream_tool_lifecycle(
                on_tool_lifecycle,
                &tool_call.id,
                &tool_call.name,
                &mut sequence,
                ToolLifecycleUpdate::Created {
                    provider_index: None,
                },
                None,
                None,
                &context_budget,
            )
            .await?;
            emit_non_stream_tool_lifecycle(
                on_tool_lifecycle,
                &tool_call.id,
                &tool_call.name,
                &mut sequence,
                ToolLifecycleUpdate::InputReady {
                    arguments: tool_call.arguments.clone(),
                },
                Some(iteration_tool_calls.clone()),
                Some(response.usage.clone()),
                &context_budget,
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
        let mut tool_futures = Vec::with_capacity(response.tool_calls.len());
        for tool_call in &response.tool_calls {
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

        let mut round_outcomes = Vec::with_capacity(response.tool_calls.len());
        for (tool_call, outcome) in response.tool_calls.iter().zip(results) {
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
    }
}

/// Convenience wrapper matching the old stub signature.
pub async fn run_agent(_agent_id: &str, _session_key: &str, _message: &str) -> Result<String> {
    anyhow::bail!(
        "run_agent requires a configured provider and tool registry; use run_agent_loop instead"
    )
}
