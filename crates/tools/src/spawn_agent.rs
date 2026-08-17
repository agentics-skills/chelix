//! Sub-agent tool: lets the LLM delegate tasks to a child agent loop.

use std::{
    collections::HashSet,
    sync::{Arc, OnceLock},
};

use {
    async_trait::async_trait,
    futures::{FutureExt, future::Abortable},
    tracing::info,
};

use crate::{
    error::Error,
    params::{bool_param, str_param, string_array_param, u64_param},
    spawn_agent_tasks::{SpawnTaskStore, SpawnTaskUpdate},
};

use {
    chelix_agents::{
        AgentRunError,
        model::LlmProvider,
        runner::{AgentLoopLimits, RunnerEvent, run_agent_loop_with_context_and_limits},
        tool_registry::{AgentTool, ToolRegistry},
    },
    chelix_config::{
        AgentRuntimeLimits, ToolsConfigSource,
        schema::{AgentConfig, AgentsConfig, ToolsConfig},
    },
    chelix_providers::ProviderRegistry,
    chelix_sessions::{metadata::SqliteSessionMetadata, store::SessionStore},
};

use crate::sessions_communicate::{
    SendToSessionFn, SessionAccessPolicy, SessionsHistoryTool, SessionsListTool,
    SessionsSearchTool, SessionsSendTool,
};

/// Maximum nesting depth for sub-agents (prevents infinite recursion).
const MAX_SPAWN_DEPTH: u64 = 3;

/// Tool parameter injected via `tool_context` to track nesting depth.
const SPAWN_DEPTH_KEY: &str = "_spawn_depth";

/// Callback for emitting events from the sub-agent back to the parent UI.
pub type OnSpawnEvent = Arc<dyn Fn(RunnerEvent) + Send + Sync>;

/// Dependencies for building policy-aware session tools in sub-agents.
#[derive(Clone)]
pub struct SessionDeps {
    pub session_metadata: Arc<SqliteSessionMetadata>,
    pub session_store: Arc<SessionStore>,
    pub send_to_session: SendToSessionFn,
}

/// A tool that spawns a sub-agent running its own agent loop.
///
/// By default the sub-agent executes synchronously (blocks until done) and
/// its result is returned as the tool output. With `nonblocking: true`, the
/// sub-agent runs in the background and the caller gets a `task_id` to poll
/// via `spawn_status` / `spawn_result`.
///
/// Sub-agents get a filtered copy of the parent's tool registry and a
/// focused system prompt.
pub struct SpawnAgentTool {
    provider_registry: Arc<tokio::sync::RwLock<ProviderRegistry>>,
    default_provider: Arc<dyn LlmProvider>,
    tool_registry: Arc<ToolRegistry>,
    tools_config_source: ToolsConfigSource,
    agents_config: Option<Arc<tokio::sync::RwLock<AgentsConfig>>>,
    on_event: Option<OnSpawnEvent>,
    session_deps: Option<SessionDeps>,
    task_store: Arc<SpawnTaskStore>,
}

impl SpawnAgentTool {
    pub fn new(
        provider_registry: Arc<tokio::sync::RwLock<ProviderRegistry>>,
        default_provider: Arc<dyn LlmProvider>,
        tool_registry: Arc<ToolRegistry>,
        tools_config_source: ToolsConfigSource,
    ) -> Self {
        Self {
            provider_registry,
            default_provider,
            tool_registry,
            tools_config_source,
            agents_config: None,
            on_event: None,
            session_deps: None,
            task_store: default_spawn_task_store(),
        }
    }

    /// Set an event callback so sub-agent activity is visible to the UI.
    pub fn with_on_event(mut self, on_event: OnSpawnEvent) -> Self {
        self.on_event = Some(on_event);
        self
    }

    /// Attach the shared agent registry.
    pub fn with_agents_config(
        mut self,
        agents_config: Arc<tokio::sync::RwLock<AgentsConfig>>,
    ) -> Self {
        self.agents_config = Some(agents_config);
        self
    }

    /// Provide session dependencies so sub-agents can get policy-aware session tools.
    pub fn with_session_deps(mut self, deps: SessionDeps) -> Self {
        self.session_deps = Some(deps);
        self
    }

    /// Share background task state with `spawn_status` and `spawn_result`.
    pub fn with_task_store(mut self, task_store: Arc<SpawnTaskStore>) -> Self {
        self.task_store = task_store;
        self
    }

    fn emit(&self, event: RunnerEvent) {
        if let Some(ref cb) = self.on_event {
            cb(event);
        }
    }

    fn build_sub_tools(&self, allow_tools: &[String], deny_tools: &[String]) -> ToolRegistry {
        let mut sub_tools = if allow_tools.is_empty() {
            self.tool_registry.clone_without(&["spawn_agent"])
        } else {
            let allowed: HashSet<&str> = allow_tools.iter().map(String::as_str).collect();
            self.tool_registry
                .clone_allowed_by(|name| name != "spawn_agent" && allowed.contains(name))
        };

        if !deny_tools.is_empty() {
            let deny: HashSet<&str> = deny_tools.iter().map(String::as_str).collect();
            sub_tools = sub_tools.clone_allowed_by(|name| !deny.contains(name));
        }

        sub_tools
    }

    /// Apply the agent reasoning effort to the provider, if configured.
    fn maybe_apply_reasoning_effort(
        provider: Arc<dyn LlmProvider>,
        agent: &AgentConfig,
    ) -> Arc<dyn LlmProvider> {
        let Some(effort) = agent.reasoning_effort.as_ref() else {
            return provider;
        };
        if let Some(existing) = provider.reasoning_effort()
            && existing.as_str() != effort.as_str()
        {
            tracing::warn!(
                model = %provider.id(),
                existing = ?existing,
                agent = ?effort,
                "agent reasoning_effort overrides model-ID suffix; using agent value"
            );
        }
        let cloned = Arc::clone(&provider);
        let new_provider = cloned.with_reasoning_effort(effort.clone());
        if new_provider.is_none() {
            info!(
                model = %provider.id(),
                ?effort,
                "provider does not support reasoning effort; ignoring agent setting"
            );
        }
        new_provider.unwrap_or(provider)
    }

    async fn resolve_agent(
        &self,
        params: &serde_json::Value,
    ) -> crate::Result<(String, AgentConfig)> {
        let agents_config = self
            .agents_config
            .as_ref()
            .ok_or_else(|| Error::message("spawn_agent requires a configured agent registry"))?;
        let agents = agents_config.read().await;
        let agent_id = str_param(params, "agent")
            .map(String::from)
            .unwrap_or_else(|| agents.default.clone());
        if agent_id.trim().is_empty() {
            return Err(Error::message(
                "spawn_agent requires agents.default to be configured",
            ));
        }
        let agent = agents.get(&agent_id).cloned().ok_or_else(|| {
            Error::message(format!("agent '{agent_id}' not found in config.agents"))
        })?;
        Ok((agent_id, agent))
    }
}

fn default_spawn_task_store() -> Arc<SpawnTaskStore> {
    static STORE: OnceLock<Arc<SpawnTaskStore>> = OnceLock::new();
    Arc::clone(STORE.get_or_init(|| Arc::new(SpawnTaskStore::default())))
}

/// Resolve the memory directory for an agent based on its scope.
fn resolve_memory_dir(
    agent_id: &str,
    scope: &chelix_config::schema::MemoryScope,
) -> std::path::PathBuf {
    use chelix_config::schema::MemoryScope;
    match scope {
        MemoryScope::User => {
            let data_dir = chelix_config::data_dir();
            data_dir.join("agent-memory").join(agent_id)
        },
        MemoryScope::Project => std::path::PathBuf::from(".chelix")
            .join("agent-memory")
            .join(agent_id),
        MemoryScope::Local => std::path::PathBuf::from(".chelix")
            .join("agent-memory-local")
            .join(agent_id),
    }
}

/// Load the first N lines of MEMORY.md from the agent's memory directory.
/// Returns `None` if the file doesn't exist or is empty.
fn load_memory_context(
    agent_id: &str,
    config: &chelix_config::schema::AgentMemoryConfig,
) -> Option<String> {
    let dir = resolve_memory_dir(agent_id, &config.scope);
    load_memory_from_dir(&dir, config.max_lines)
}

/// Load memory content from a specific directory.
fn load_memory_from_dir(dir: &std::path::Path, max_lines: usize) -> Option<String> {
    let memory_path = dir.join("MEMORY.md");

    // Create directory if missing so agents can write to it later.
    let _ = std::fs::create_dir_all(dir);

    let content = std::fs::read_to_string(&memory_path).ok()?;
    if content.trim().is_empty() {
        return None;
    }

    let lines: Vec<&str> = content.lines().take(max_lines).collect();
    Some(lines.join("\n"))
}

/// Build the system prompt for a spawned agent.
fn build_sub_agent_prompt(
    task: &str,
    context: &str,
    agent: &AgentConfig,
    agent_id: &str,
) -> String {
    let mut prompt = chelix_config::load_subagent_prompt_for_agent(agent_id).unwrap_or_default();

    if let Some(ref memory_config) = agent.memory
        && let Some(memory_content) = load_memory_context(agent_id, memory_config)
    {
        if !prompt.is_empty() {
            prompt.push_str("\n\n");
        }
        prompt.push_str("# Agent Memory\n\n");
        prompt.push_str(&memory_content);
    }

    if !prompt.is_empty() {
        prompt.push_str("\n\n");
    }
    prompt.push_str("Task: ");
    prompt.push_str(task);
    if !context.is_empty() {
        prompt.push_str("\n\nContext: ");
        prompt.push_str(context);
    }

    prompt
}

#[async_trait]
impl AgentTool for SpawnAgentTool {
    fn name(&self) -> &str {
        "spawn_agent"
    }

    fn description(&self) -> &str {
        "Spawn a sub-agent to handle a complex, multi-step task autonomously. \
         The sub-agent runs its own agent loop with access to tools and returns \
         the result when done. Use this to delegate tasks that require multiple \
         tool calls or independent reasoning. Supports optional tool policy controls."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "task": {
                    "type": "string",
                    "description": "The task to delegate to the sub-agent"
                },
                "context": {
                    "type": "string",
                    "description": "Additional context for the sub-agent (optional)"
                },
                "agent": {
                    "type": "string",
                    "description": "Agent ID. If omitted, uses agents.default."
                },
                "model": {
                    "type": "string",
                    "description": "Model ID to use. If omitted, uses the selected agent model or the parent model."
                },
                "allow_tools": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Optional whitelist of tool names for the spawned agent. spawn_agent is always excluded."
                },
                "deny_tools": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Optional blacklist of tool names for the sub-agent."
                },
                "nonblocking": {
                    "type": "boolean",
                    "description": "If true, return immediately with a task_id and let the sub-agent continue in the background. Use spawn_status and spawn_result to inspect it."
                },
                "active_tools": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Optional per-turn whitelist of tool names visible to the spawned agent. Overrides agent tool_controls.active_tools."
                },
                "tool_choice": {
                    "type": "object",
                    "description": "Optional provider tool choice for the spawned-agent turn. Overrides agent tool_controls.tool_choice.",
                    "properties": {
                        "type": { "type": "string", "enum": ["auto", "any", "none", "tool"] },
                        "name": { "type": "string" }
                    },
                    "required": ["type"]
                }
            },
            "required": ["task"]
        })
    }

    async fn execute(&self, params: serde_json::Value) -> anyhow::Result<serde_json::Value> {
        let task = str_param(&params, "task")
            .ok_or_else(|| Error::message("missing required parameter: task"))?;
        let context = str_param(&params, "context").unwrap_or("");
        let (agent_id, agent) = self.resolve_agent(&params).await?;
        let tools_config = self
            .tools_config_source
            .load()
            .map_err(|error| Error::message(format!("reload tools config: {error}")))?;
        let runtime_limits = AgentRuntimeLimits::resolve_for_spawned_agent(&tools_config, &agent);
        let explicit_model = str_param(&params, "model").map(String::from);
        let model_id = explicit_model.clone().or_else(|| agent.model.clone());

        let explicit_allow_tools = string_array_param(&params, "allow_tools")?;
        let allow_tools = if explicit_allow_tools.is_empty() {
            agent.tools.allow.clone()
        } else {
            explicit_allow_tools
        };

        let explicit_deny_tools = string_array_param(&params, "deny_tools")?;
        let deny_tools = if explicit_deny_tools.is_empty() {
            agent.tools.deny.clone()
        } else {
            explicit_deny_tools
        };

        let nonblocking = bool_param(&params, "nonblocking", false);
        let mut tool_controls = agent.tool_controls.clone();
        if params.get("active_tools").is_some() {
            tool_controls.active_tools = Some(string_array_param(&params, "active_tools")?);
        }
        if let Some(value) = params.get("tool_choice") {
            tool_controls.tool_choice = Some(
                serde_json::from_value(value.clone())
                    .map_err(|e| Error::message(format!("invalid tool_choice parameter: {e}")))?,
            );
        }

        // Check nesting depth.
        let depth = u64_param(&params, SPAWN_DEPTH_KEY, 0);
        if depth >= MAX_SPAWN_DEPTH {
            return Err(Error::message(format!(
                "maximum sub-agent nesting depth ({MAX_SPAWN_DEPTH}) exceeded"
            ))
            .into());
        }

        // Resolve provider and apply the selected agent's reasoning effort.
        let provider = if let Some(id) = model_id {
            let reg = self.provider_registry.read().await;
            let base_provider = reg
                .get(&id)
                .ok_or_else(|| Error::message(format!("unknown model: {id}")))?;
            Self::maybe_apply_reasoning_effort(base_provider, &agent)
        } else {
            let base = Arc::clone(&self.default_provider);
            Self::maybe_apply_reasoning_effort(base, &agent)
        };

        // Capture model ID before provider is moved into the sub-agent loop.
        let model_id = provider.id().to_string();

        info!(
            task = %task,
            depth = depth,
            model = %model_id,
            agent = %agent_id,
            timeout_secs = runtime_limits.timeout_secs,
            timeout_source = runtime_limits.timeout_source.as_str(),
            max_tools_threshold = runtime_limits.max_tools_threshold,
            "spawning sub-agent"
        );

        self.emit(RunnerEvent::SubAgentStart {
            task: task.to_string(),
            model: model_id.clone(),
            depth,
        });

        // Build filtered tool registry from policy knobs.
        let mut sub_tools = self.build_sub_tools(&allow_tools, &deny_tools);

        // Apply session access policy configured for the selected agent.
        if let Some(ref session_config) = agent.sessions
            && let Some(ref deps) = self.session_deps
        {
            let policy = SessionAccessPolicy::from(session_config);
            sub_tools.replace(Box::new(
                SessionsListTool::new(Arc::clone(&deps.session_metadata))
                    .with_policy(policy.clone()),
            ));
            sub_tools.replace(Box::new(
                SessionsHistoryTool::new(
                    Arc::clone(&deps.session_store),
                    Arc::clone(&deps.session_metadata),
                )
                .with_policy(policy.clone()),
            ));
            sub_tools.replace(Box::new(
                SessionsSearchTool::new(
                    Arc::clone(&deps.session_store),
                    Arc::clone(&deps.session_metadata),
                )
                .with_policy(policy.clone()),
            ));
            sub_tools.replace(Box::new(
                SessionsSendTool::new(
                    Arc::clone(&deps.session_metadata),
                    Arc::clone(&deps.send_to_session),
                )
                .with_policy(policy),
            ));
        }

        let system_prompt = build_sub_agent_prompt(task, context, &agent, &agent_id);

        // Build tool context with incremented depth and propagated session key.
        let session_key = params
            .get("_session_key")
            .and_then(serde_json::Value::as_str)
            .map(String::from);
        let mut tool_context = serde_json::json!({
            SPAWN_DEPTH_KEY: depth + 1,
        });
        if let Some(ref key) = session_key {
            tool_context["_session_key"] = serde_json::Value::String(key.clone());
        }
        if let Some(active_tools) = tool_controls.active_tools {
            tool_context["active_tools"] = serde_json::json!(active_tools);
        }
        if let Some(tool_choice) = tool_controls.tool_choice {
            tool_context["tool_choice"] = serde_json::to_value(tool_choice)?;
        }

        if nonblocking {
            #[cfg(feature = "metrics")]
            {
                use chelix_metrics::{counter, gauge, labels, spawn as spawn_metrics};
                counter!(spawn_metrics::SPAWNED_TOTAL, labels::MODE => "nonblocking").increment(1);
                gauge!(spawn_metrics::TASKS_IN_FLIGHT).increment(1.0);
            }

            let (abort_handle, abort_registration) = futures::future::AbortHandle::new_pair();
            let task_entry = self
                .task_store
                .insert_running(
                    task.to_string(),
                    session_key,
                    model_id.clone(),
                    Some(agent_id.clone()),
                    abort_handle,
                )
                .await;
            let task_id = task_entry.id.clone();
            let store = Arc::clone(&self.task_store);
            let on_event = self.on_event.clone();
            let task_for_run = task.to_string();
            let model_for_run = model_id.clone();
            let agent_for_log = agent_id.clone();
            tokio::spawn(async move {
                let result = Abortable::new(
                    std::panic::AssertUnwindSafe(run_spawned_agent(
                        provider,
                        sub_tools,
                        system_prompt,
                        task_for_run.clone(),
                        tool_context,
                        runtime_limits,
                        tools_config,
                    ))
                    .catch_unwind(),
                    abort_registration,
                )
                .await;

                let result = match result {
                    Ok(result) => result,
                    Err(_aborted) => {
                        store
                            .complete(&task_id, SpawnTaskUpdate {
                                text: None,
                                iterations: 0,
                                tool_calls_made: 0,
                                error: Some("cancelled by caller".to_string()),
                            })
                            .await;

                        #[cfg(feature = "metrics")]
                        {
                            use chelix_metrics::{counter, gauge, labels, spawn as spawn_metrics};
                            counter!(
                                spawn_metrics::COMPLETED_TOTAL,
                                labels::STATUS => "cancelled"
                            )
                            .increment(1);
                            gauge!(spawn_metrics::TASKS_IN_FLIGHT).decrement(1.0);
                        }

                        if let Some(cb) = on_event {
                            cb(RunnerEvent::SubAgentEnd {
                                task: task_for_run,
                                model: model_for_run,
                                depth,
                                iterations: 0,
                                tool_calls_made: 0,
                            });
                        }
                        return;
                    },
                };

                let (update, iterations, tool_calls_made) = match result {
                    Ok(Ok(result)) => {
                        let iterations = result.iterations;
                        let tool_calls_made = result.tool_calls_made;
                        (
                            SpawnTaskUpdate {
                                text: Some(result.output.text),
                                iterations,
                                tool_calls_made,
                                error: None,
                            },
                            iterations,
                            tool_calls_made,
                        )
                    },
                    Ok(Err(err)) => (
                        SpawnTaskUpdate {
                            text: None,
                            iterations: 0,
                            tool_calls_made: 0,
                            error: Some(err.to_string()),
                        },
                        0,
                        0,
                    ),
                    Err(_panic) => {
                        tracing::error!(
                            task_id = %task_id,
                            task = %task_for_run,
                            "nonblocking sub-agent panicked"
                        );
                        (
                            SpawnTaskUpdate {
                                text: None,
                                iterations: 0,
                                tool_calls_made: 0,
                                error: Some("sub-agent panicked".to_string()),
                            },
                            0,
                            0,
                        )
                    },
                };

                let status_label = if update.error.is_some() {
                    "failed"
                } else {
                    "completed"
                };

                store.complete(&task_id, update).await;

                info!(
                    task_id = %task_id,
                    task = %task_for_run,
                    model = %model_for_run,
                    depth = depth,
                    iterations = iterations,
                    tool_calls = tool_calls_made,
                    agent = ?agent_for_log,
                    status = status_label,
                    "nonblocking sub-agent finished"
                );

                #[cfg(feature = "metrics")]
                {
                    use chelix_metrics::{counter, gauge, labels, spawn as spawn_metrics};
                    counter!(
                        spawn_metrics::COMPLETED_TOTAL,
                        labels::STATUS => status_label.to_string()
                    )
                    .increment(1);
                    gauge!(spawn_metrics::TASKS_IN_FLIGHT).decrement(1.0);
                }

                if let Some(cb) = on_event {
                    cb(RunnerEvent::SubAgentEnd {
                        task: task_for_run,
                        model: model_for_run,
                        depth,
                        iterations,
                        tool_calls_made,
                    });
                }
            });

            return Ok(serde_json::json!({
                "task_id": task_entry.id,
                "status": "running",
                "started_at": task_entry.started_at,
                "model": model_id,
                "agent": agent_id,
            }));
        }

        #[cfg(feature = "metrics")]
        {
            use chelix_metrics::{counter, labels, spawn as spawn_metrics};
            counter!(spawn_metrics::SPAWNED_TOTAL, labels::MODE => "blocking").increment(1);
        }

        let result = run_spawned_agent(
            provider,
            sub_tools,
            system_prompt,
            task.to_string(),
            tool_context,
            runtime_limits,
            tools_config,
        )
        .await;

        // Emit SubAgentEnd regardless of success/failure.
        let (iterations, tool_calls_made) = match &result {
            Ok(r) => (r.iterations, r.tool_calls_made),
            Err(_) => (0, 0),
        };
        self.emit(RunnerEvent::SubAgentEnd {
            task: task.to_string(),
            model: model_id.clone(),
            depth,
            iterations,
            tool_calls_made,
        });

        #[cfg(feature = "metrics")]
        {
            use chelix_metrics::{counter, labels, spawn as spawn_metrics};
            let status = if result.is_ok() {
                "completed"
            } else {
                "failed"
            };
            counter!(
                spawn_metrics::COMPLETED_TOTAL,
                labels::STATUS => status.to_string()
            )
            .increment(1);
        }

        let result = result?;

        info!(
            task = %task,
            depth = depth,
            iterations = result.iterations,
            tool_calls = result.tool_calls_made,
            agent = ?agent_id,
            "sub-agent completed"
        );

        Ok(serde_json::json!({
            "text": result.output.text,
            "iterations": result.iterations,
            "tool_calls_made": result.tool_calls_made,
            "model": model_id,
            "agent": agent_id,
        }))
    }
}

#[tracing::instrument(skip(provider, sub_tools, system_prompt, tool_context, runtime_limits, tools_config), fields(task_len = task.len()))]
async fn run_spawned_agent(
    provider: Arc<dyn LlmProvider>,
    sub_tools: ToolRegistry,
    system_prompt: String,
    task: String,
    tool_context: serde_json::Value,
    runtime_limits: AgentRuntimeLimits,
    tools_config: ToolsConfig,
) -> Result<chelix_agents::runner::AgentRunResult, AgentRunError> {
    let user_content = chelix_agents::UserContent::text(&task);
    let agent_future = run_agent_loop_with_context_and_limits(
        provider,
        &sub_tools,
        &tools_config,
        &system_prompt,
        &user_content,
        None,
        None,
        None,
        Some(tool_context),
        None,
        None,
        AgentLoopLimits {
            max_tools_threshold: runtime_limits.max_tools_threshold,
            max_tool_result_bytes: Some(runtime_limits.max_tool_result_bytes),
            automatic_checkpointing: false,
            resume_from_history: false,
            resume_after_checkpoint: false,
        },
    );

    if runtime_limits.timeout_secs > 0 {
        let timeout_secs = runtime_limits.timeout_secs;
        let duration = std::time::Duration::from_secs(timeout_secs);
        match tokio::time::timeout(duration, agent_future).await {
            Ok(r) => r,
            Err(_) => Err(AgentRunError::Other(anyhow::anyhow!(
                "sub-agent timed out after {timeout_secs}s"
            ))),
        }
    } else {
        agent_future.await
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
#[path = "spawn_agent_tests.rs"]
mod spawn_agent_tests;
