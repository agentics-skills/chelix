use std::{collections::HashSet, sync::Arc, time::Duration};

use {
    chelix_common::{
        ContextBudgetMetadata,
        hooks::{ChannelBinding, HookAction, HookPayload, HookRegistry},
        tool_lifecycle::{ToolLifecycleEvent, ToolLifecycleUpdate},
    },
    chelix_sessions::ToolResultStore,
    tokio::time::{Instant, MissedTickBehavior},
    tokio_util::sync::CancellationToken,
    tracing::{debug, info, warn},
};

use crate::{
    model::{ToolCall, ToolChoice},
    tool_arg_validator::validate_tool_args,
    tool_registry::ToolRegistry,
};

use super::{
    AGENT_RUN_CANCELLED_REASON, AgentRunError, OnToolLifecycle, RunnerToolLifecycleEvent,
    deliver_tool_lifecycle, enrich_tool_arguments, log_tool_argument_diagnostic,
    public_tool_arguments, resolve_tool_lookup, sanitize_tool_name,
    tool_result::persist_and_truncate,
};

#[derive(Debug)]
pub(crate) struct ToolExecutionOutcome {
    pub success: bool,
    pub error: Option<String>,
    pub rejected: bool,
    pub result: String,
}

pub(crate) struct ToolInvocationExecutor<'a> {
    pub tools: &'a ToolRegistry,
    pub tool_result_store: &'a ToolResultStore,
    pub max_tool_result_bytes: usize,
    pub tool_context: Option<&'a serde_json::Value>,
    pub hook_registry: Option<&'a Arc<HookRegistry>>,
    pub session_key: &'a str,
    pub channel: Option<&'a ChannelBinding>,
    pub active_tool_names: Option<&'a HashSet<String>>,
    pub tool_choice: Option<&'a ToolChoice>,
    pub on_lifecycle: Option<&'a OnToolLifecycle>,
    pub context_budget: &'a ContextBudgetMetadata,
}

impl ToolInvocationExecutor<'_> {
    pub async fn execute(
        &self,
        tool_call: &ToolCall,
        first_sequence: u64,
    ) -> Result<ToolExecutionOutcome, AgentRunError> {
        self.execute_inner(tool_call, first_sequence, None).await
    }

    pub async fn execute_cancellable(
        &self,
        tool_call: &ToolCall,
        first_sequence: u64,
        cancellation_token: &CancellationToken,
    ) -> Result<ToolExecutionOutcome, AgentRunError> {
        self.execute_inner(tool_call, first_sequence, Some(cancellation_token))
            .await
    }

    async fn execute_inner(
        &self,
        tool_call: &ToolCall,
        first_sequence: u64,
        cancellation_token: Option<&CancellationToken>,
    ) -> Result<ToolExecutionOutcome, AgentRunError> {
        let sanitized = sanitize_tool_name(&tool_call.name);
        if *sanitized != tool_call.name {
            debug!(
                original = %tool_call.name,
                sanitized = %sanitized,
                "sanitized mangled tool name"
            );
        }
        let (tool, resolved_name) = resolve_tool_lookup(self.tools, sanitized.as_ref());
        let execution_name = resolved_name.into_owned();
        let mut execution_arguments = tool_call.arguments.clone();
        enrich_tool_arguments(&mut execution_arguments, self.tool_context, &tool_call.id);
        log_tool_argument_diagnostic(&execution_name, tool_call.argument_diagnostic.as_ref());

        let public_arguments = public_tool_arguments(&execution_arguments);
        let validation_error = if matches!(self.tool_choice, Some(ToolChoice::None)) {
            Some(format!(
                "tool `{execution_name}` cannot be called: tool use is disabled for this turn"
            ))
        } else if self
            .active_tool_names
            .is_some_and(|active| !active.contains(&execution_name))
        {
            Some(format!(
                "tool `{execution_name}` is not active for this turn; choose one of the currently available tools"
            ))
        } else if let Some(ref tool) = tool {
            let schema = tool.parameters_schema();
            match validate_tool_args(&schema, &execution_arguments) {
                Ok(()) => tool.validate(&execution_arguments).err().map(|error| {
                    warn!(
                        tool = %execution_name,
                        error = %error,
                        "tool call rejected by implementation validation"
                    );
                    format!("Tool call rejected before execution by `{execution_name}`: {error}")
                }),
                Err(error) => {
                    warn!(
                        tool = %execution_name,
                        summary = %error.short_summary_with_argument_diagnostic(
                            tool_call.argument_diagnostic.as_ref(),
                        ),
                        "tool call rejected by pre-dispatch schema validation"
                    );
                    Some(error.to_llm_error_message_with_argument_diagnostic(
                        &execution_name,
                        tool_call.argument_diagnostic.as_ref(),
                    ))
                },
            }
        } else {
            Some(format!("unknown tool: {execution_name}"))
        };

        let mut sequence = first_sequence;
        if cancellation_token.is_some_and(CancellationToken::is_cancelled) {
            self.emit(
                tool_call,
                &mut sequence,
                ToolLifecycleUpdate::Cancelled {
                    arguments: Some(public_arguments),
                    reason: AGENT_RUN_CANCELLED_REASON.to_owned(),
                },
                None,
            )
            .await?;
            return Err(AgentRunError::Cancelled);
        }
        if let Some(error) = validation_error {
            let raw_result = serde_json::json!({ "error": error.clone() });
            let result = self
                .prepare_agent_result(
                    tool_call,
                    tool.as_ref(),
                    &execution_arguments,
                    &raw_result,
                    false,
                )
                .await?;
            self.emit(
                tool_call,
                &mut sequence,
                ToolLifecycleUpdate::Rejected {
                    arguments: public_arguments,
                    reason: error.clone(),
                    result: result.clone(),
                },
                None,
            )
            .await?;
            return Ok(ToolExecutionOutcome {
                success: false,
                error: Some(error),
                rejected: true,
                result,
            });
        }

        self.emit(
            tool_call,
            &mut sequence,
            ToolLifecycleUpdate::WaitingForExecution {
                arguments: public_arguments.clone(),
            },
            None,
        )
        .await?;

        let execution = self.run_before_hook_and_execute(
            tool_call,
            &execution_name,
            tool.as_ref(),
            execution_arguments,
            &mut sequence,
        );
        let raw_execution = match cancellation_token {
            Some(cancellation_token) => {
                match cancellation_token.run_until_cancelled(execution).await {
                    Some(result) => result?,
                    None => {
                        self.emit(
                            tool_call,
                            &mut sequence,
                            ToolLifecycleUpdate::Cancelled {
                                arguments: Some(public_arguments),
                                reason: AGENT_RUN_CANCELLED_REASON.to_owned(),
                            },
                            None,
                        )
                        .await?;
                        return Err(AgentRunError::Cancelled);
                    },
                }
            },
            None => execution.await?,
        };
        let (success, wrapped_result, error, effective_arguments) = raw_execution;
        let public_arguments = public_tool_arguments(&effective_arguments);
        let has_raw_result = wrapped_result.get("result").is_some();
        let raw_result = wrapped_result
            .get("result")
            .cloned()
            .unwrap_or_else(|| wrapped_result.clone());
        let result = self
            .prepare_agent_result(
                tool_call,
                tool.as_ref(),
                &effective_arguments,
                &raw_result,
                has_raw_result,
            )
            .await?;
        let lifecycle_result = result.clone();

        self.emit(
            tool_call,
            &mut sequence,
            ToolLifecycleUpdate::ResultReady {
                arguments: public_arguments.clone(),
                success,
                result: Some(lifecycle_result.clone()),
                error: error.clone(),
            },
            None,
        )
        .await?;

        self.emit(
            tool_call,
            &mut sequence,
            ToolLifecycleUpdate::Completed {
                arguments: public_arguments,
                success,
                result: Some(lifecycle_result),
                error: error.clone(),
            },
            has_raw_result.then_some(raw_result),
        )
        .await?;

        Ok(ToolExecutionOutcome {
            success,
            error,
            rejected: false,
            result,
        })
    }

    async fn run_before_hook_and_execute(
        &self,
        tool_call: &ToolCall,
        execution_name: &str,
        tool: Option<&Arc<dyn crate::tool_registry::AgentTool>>,
        mut arguments: serde_json::Value,
        sequence: &mut u64,
    ) -> Result<(bool, serde_json::Value, Option<String>, serde_json::Value), AgentRunError> {
        if let Some(hooks) = self.hook_registry {
            let payload = HookPayload::BeforeToolCall {
                session_key: self.session_key.to_owned(),
                tool_name: execution_name.to_owned(),
                arguments: arguments.clone(),
                channel: self.channel.cloned(),
            };
            match hooks.dispatch(&payload).await {
                Ok(HookAction::Block(reason)) => {
                    warn!(tool = %execution_name, reason = %reason, "tool call blocked by hook");
                    let error = format!("blocked by hook: {reason}");
                    return Ok((
                        false,
                        serde_json::json!({ "error": error.clone() }),
                        Some(error),
                        arguments,
                    ));
                },
                Ok(HookAction::ModifyPayload(value)) => {
                    arguments = value;
                    enrich_tool_arguments(&mut arguments, self.tool_context, &tool_call.id);
                    if let Some(tool) = tool {
                        let schema = tool.parameters_schema();
                        if let Err(validation_error) = validate_tool_args(&schema, &arguments) {
                            let error = validation_error.to_llm_error_message(execution_name);
                            warn!(
                                tool = %execution_name,
                                summary = %validation_error.short_summary(),
                                "tool call rejected after BeforeToolCall hook modified arguments"
                            );
                            return Ok((
                                false,
                                serde_json::json!({ "error": error.clone() }),
                                Some(error),
                                arguments,
                            ));
                        }
                    }
                },
                Ok(HookAction::Continue) => {},
                Err(error) => {
                    warn!(tool = %execution_name, error = %error, "BeforeToolCall hook dispatch failed");
                },
            }
        }

        let Some(tool) = tool else {
            let error = format!("unknown tool: {execution_name}");
            return Ok((
                false,
                serde_json::json!({ "error": error.clone() }),
                Some(error),
                arguments,
            ));
        };

        let public_arguments = public_tool_arguments(&arguments);
        self.emit(
            tool_call,
            sequence,
            ToolLifecycleUpdate::Executing {
                arguments: public_arguments.clone(),
                started_at_ms: lifecycle_now_ms()?,
            },
            None,
        )
        .await?;
        info!(tool = %execution_name, id = %tool_call.id, args = %arguments, "executing tool");

        self.emit(
            tool_call,
            sequence,
            ToolLifecycleUpdate::ExecutionProgress {
                arguments: public_arguments.clone(),
                elapsed_ms: 0,
                message: "wait for result [0] sec.".to_owned(),
            },
            None,
        )
        .await?;

        let execution = tool.execute(arguments.clone());
        tokio::pin!(execution);
        let mut interval = tokio::time::interval_at(
            Instant::now() + Duration::from_secs(1),
            Duration::from_secs(1),
        );
        interval.set_missed_tick_behavior(MissedTickBehavior::Delay);
        let mut elapsed_seconds = 0_u64;

        let execution_result = loop {
            tokio::select! {
                result = &mut execution => break result,
                _ = interval.tick() => {
                    elapsed_seconds = elapsed_seconds.saturating_add(1);
                    self.emit(
                        tool_call,
                        sequence,
                        ToolLifecycleUpdate::ExecutionProgress {
                            arguments: public_arguments.clone(),
                            elapsed_ms: elapsed_seconds.saturating_mul(1_000),
                            message: format!("wait for result [{elapsed_seconds}] sec."),
                        },
                        None,
                    )
                    .await?;
                },
            }
        };

        match execution_result {
            Ok(value) => {
                let has_error = value.get("error").is_some()
                    || value.get("success") == Some(&serde_json::json!(false));
                let error = has_error
                    .then(|| {
                        value
                            .get("error")
                            .and_then(|item| item.as_str())
                            .map(str::to_owned)
                    })
                    .flatten();
                self.dispatch_after_tool_call(execution_name, !has_error, Some(value.clone()))
                    .await;
                Ok((
                    !has_error,
                    serde_json::json!({ "result": value }),
                    error,
                    arguments,
                ))
            },
            Err(error) => {
                let error = error.to_string();
                self.dispatch_after_tool_call(execution_name, false, None)
                    .await;
                Ok((
                    false,
                    serde_json::json!({ "error": error.clone() }),
                    Some(error),
                    arguments,
                ))
            },
        }
    }

    async fn dispatch_after_tool_call(
        &self,
        tool_name: &str,
        success: bool,
        result: Option<serde_json::Value>,
    ) {
        let Some(hooks) = self.hook_registry else {
            return;
        };
        let payload = HookPayload::AfterToolCall {
            session_key: self.session_key.to_owned(),
            tool_name: tool_name.to_owned(),
            success,
            result,
            channel: self.channel.cloned(),
        };
        if let Err(error) = hooks.dispatch(&payload).await {
            warn!(tool = %tool_name, error = %error, "AfterToolCall hook dispatch failed");
        }
    }

    async fn prepare_agent_result(
        &self,
        tool_call: &ToolCall,
        tool: Option<&Arc<dyn crate::tool_registry::AgentTool>>,
        arguments: &serde_json::Value,
        raw_result: &serde_json::Value,
        has_raw_result: bool,
    ) -> Result<String, AgentRunError> {
        let mut agent_result = if has_raw_result {
            match tool {
                Some(tool) => tool.agent_result(arguments, raw_result).await?,
                None => raw_result.clone(),
            }
        } else {
            raw_result.clone()
        };

        if let Some(hooks) = self.hook_registry {
            let payload = HookPayload::ToolResultPersist {
                session_key: self.session_key.to_owned(),
                tool_name: sanitize_tool_name(&tool_call.name).into_owned(),
                result: agent_result.clone(),
                channel: self.channel.cloned(),
            };
            match hooks.dispatch(&payload).await {
                Ok(HookAction::ModifyPayload(value)) => {
                    debug!(tool = %tool_call.name, "ToolResultPersist replaced tool result");
                    agent_result = value;
                },
                Ok(HookAction::Block(reason)) => {
                    warn!(tool = %tool_call.name, reason = %reason, "ToolResultPersist blocked result — substituting error marker");
                    agent_result = serde_json::json!({
                        "error": format!("blocked by hook: {reason}")
                    });
                },
                Ok(HookAction::Continue) => {},
                Err(error) => {
                    warn!(tool = %tool_call.name, error = %error, "ToolResultPersist hook dispatch failed");
                },
            }
        }

        let truncation = tool
            .map(|tool| tool.truncation(arguments))
            .unwrap_or_default();
        let persistence = tool
            .map(|tool| tool.result_persistence(arguments))
            .unwrap_or_default();
        persist_and_truncate(
            self.tool_result_store,
            self.session_key,
            &tool_call.id,
            &agent_result,
            self.max_tool_result_bytes,
            truncation,
            persistence,
        )
        .await
        .map_err(Into::into)
    }

    async fn emit(
        &self,
        tool_call: &ToolCall,
        sequence: &mut u64,
        update: ToolLifecycleUpdate,
        raw_result: Option<serde_json::Value>,
    ) -> Result<(), AgentRunError> {
        let context_budget = update
            .stage()
            .is_terminal()
            .then(|| self.context_budget.clone());
        let lifecycle = ToolLifecycleEvent {
            tool_call_id: tool_call.id.clone(),
            tool_name: tool_call.name.clone(),
            sequence: *sequence,
            emitted_at_ms: lifecycle_now_ms()?,
            run_id: None,
            context_budget,
            update,
        };
        *sequence = sequence.saturating_add(1);
        let mut runner_event = RunnerToolLifecycleEvent::new(lifecycle);
        runner_event.raw_result = raw_result;
        runner_event.context_budget = Some(self.context_budget.clone());
        deliver_tool_lifecycle(self.on_lifecycle, runner_event).await
    }
}

pub(crate) fn lifecycle_now_ms() -> Result<u64, AgentRunError> {
    let milliseconds = time::OffsetDateTime::now_utc().unix_timestamp_nanos()
        / time::Duration::milliseconds(1).whole_nanoseconds();
    u64::try_from(milliseconds).map_err(|error| {
        AgentRunError::Other(anyhow::anyhow!(
            "current UTC timestamp is outside the supported lifecycle range: {error}"
        ))
    })
}
