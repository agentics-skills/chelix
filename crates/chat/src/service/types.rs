//! `LiveChatService` struct, constructors, and helper methods.

use std::{
    collections::{HashMap, HashSet},
    path::Path,
    sync::Arc,
};

use {serde_json::Value, tokio::sync::RwLock, tokio_util::sync::CancellationToken, tracing::warn};

use {
    chelix_agents::tool_registry::ToolRegistry,
    chelix_common::{
        ActiveToolInvocation, MaterializerError, ProviderItemUpdate, ProviderSegmentMaterializer,
    },
    chelix_providers::ProviderRegistry,
    chelix_service_traits::SessionMutationCoordinator,
    chelix_sessions::{
        PersistedMessage, SessionPromptQueueStore,
        message::{PersistedFunction, PersistedToolCall},
        metadata::SqliteSessionMetadata,
        state_store::SessionStateStore,
        store::SessionStore,
    },
};

use {
    chelix_agents::prompt::{
        build_system_prompt_minimal_runtime_details,
        build_system_prompt_with_session_runtime_details,
    },
    chelix_config::ToolMode,
};

use crate::{
    agent_loop::effective_tool_mode,
    error,
    models::DisabledModelsStore,
    prompt::{
        apply_request_runtime_context, build_policy_context, build_prompt_runtime_context,
        discover_skills_if_enabled, filter_skills_for_agent, load_prompt_persona_for_session,
        prepare_run_registry, prompt_build_limits_from_config,
    },
    prompt_queue::PromptQueue,
    runtime::ChatRuntime,
    types::*,
};

#[derive(Debug, Clone)]
pub(crate) struct ActiveAssistantDraft {
    pub(crate) materializer: ProviderSegmentMaterializer,
    model: String,
    provider: String,
    reasoning_effort: Option<String>,
    seq: Option<u64>,
    run_id: String,
}

#[derive(Default)]
pub(crate) struct EventForwarderResult {
    pub(crate) tool_segment_indices: HashMap<String, usize>,
    pub(crate) error: Option<String>,
}

impl ActiveAssistantDraft {
    pub(crate) fn new(
        run_id: &str,
        model: &str,
        provider: &str,
        reasoning_effort: Option<String>,
        seq: Option<u64>,
    ) -> Self {
        Self {
            // Segment identity is adopted from the provider's first update.
            materializer: ProviderSegmentMaterializer::pending(),
            model: model.to_string(),
            provider: provider.to_string(),
            reasoning_effort,
            seq,
            run_id: run_id.to_string(),
        }
    }

    pub(crate) fn apply_update(
        &mut self,
        update: &ProviderItemUpdate,
    ) -> Result<(), MaterializerError> {
        self.materializer.apply_update(update)
    }

    pub(crate) fn next_segment(&self) -> Self {
        Self::new(
            &self.run_id,
            &self.model,
            &self.provider,
            self.reasoning_effort.clone(),
            self.seq,
        )
    }

    pub(crate) fn has_provider_output(&self) -> bool {
        !self.materializer.segment.items.is_empty()
    }

    pub(crate) fn to_persisted_message(
        &self,
        tool_calls: Option<Vec<PersistedToolCall>>,
        usage: Option<&chelix_agents::model::Usage>,
    ) -> PersistedMessage {
        PersistedMessage::Assistant {
            content: self.materializer.segment.message_text().unwrap_or_default(),
            created_at: Some(now_ms()),
            model: Some(self.model.clone()),
            provider: Some(self.provider.clone()),
            reasoning_effort: self.reasoning_effort.clone(),
            input_tokens: usage.map(|usage| usage.input_tokens),
            output_tokens: usage.map(|usage| usage.output_tokens),
            cache_read_tokens: usage.map(|usage| usage.cache_read_tokens),
            cache_write_tokens: usage.map(|usage| usage.cache_write_tokens),
            duration_ms: None,
            request_input_tokens: usage.map(|usage| usage.input_tokens),
            request_output_tokens: usage.map(|usage| usage.output_tokens),
            request_cache_read_tokens: usage.map(|usage| usage.cache_read_tokens),
            request_cache_write_tokens: usage.map(|usage| usage.cache_write_tokens),
            tool_calls,
            reasoning: self.materializer.segment.reasoning_content(),
            provider_items: Some(self.materializer.segment.items.clone()),
            segment_id: self.materializer.segment.segment_id.clone(),
            llm_api_response: None,
            audio: None,
            seq: self.seq,
            run_id: Some(self.run_id.clone()),
        }
    }
}

pub(crate) async fn persist_active_assistant_draft(
    session_store: &SessionStore,
    active_partial_assistant: &Arc<RwLock<HashMap<String, ActiveAssistantDraft>>>,
    session_key: &str,
) -> error::Result<Option<(Value, usize)>> {
    let mut drafts = active_partial_assistant.write().await;
    let Some(draft) = drafts.get_mut(session_key) else {
        return Ok(None);
    };
    if !draft.has_provider_output() {
        drafts.remove(session_key);
        return Ok(None);
    }

    let draft = drafts.remove(session_key).ok_or_else(|| {
        error::Error::message(format!(
            "active assistant draft disappeared for session '{session_key}'"
        ))
    })?;
    drop(drafts);
    let partial_value = draft.to_persisted_message(None, None).to_value();
    let message_index = session_store
        .append_with_index(session_key, &partial_value)
        .await
        .map_err(|source| {
            error::Error::external("failed to persist partial assistant segment", source)
        })?;

    Ok(Some((partial_value, message_index)))
}

pub(crate) fn build_persisted_tool_call(
    tool_call_id: impl Into<String>,
    tool_name: impl Into<String>,
    arguments: Option<Value>,
) -> PersistedToolCall {
    PersistedToolCall {
        id: tool_call_id.into(),
        call_type: "function".to_string(),
        function: PersistedFunction {
            name: tool_name.into(),
            arguments: arguments
                .unwrap_or_else(|| serde_json::json!({}))
                .to_string(),
        },
    }
}

pub(crate) fn build_persisted_assistant_message(
    assistant_output: AssistantTurnOutput,
    model: Option<String>,
    provider: Option<String>,
    reasoning_effort: Option<String>,
    seq: Option<u64>,
    run_id: Option<String>,
) -> PersistedMessage {
    PersistedMessage::Assistant {
        content: assistant_output.text,
        created_at: Some(now_ms()),
        model,
        provider,
        reasoning_effort,
        input_tokens: Some(assistant_output.input_tokens),
        output_tokens: Some(assistant_output.output_tokens),
        cache_read_tokens: Some(assistant_output.cache_read_tokens),
        cache_write_tokens: Some(assistant_output.cache_write_tokens),
        duration_ms: Some(assistant_output.duration_ms),
        request_input_tokens: Some(assistant_output.request_input_tokens),
        request_output_tokens: Some(assistant_output.request_output_tokens),
        request_cache_read_tokens: Some(assistant_output.request_cache_read_tokens),
        request_cache_write_tokens: Some(assistant_output.request_cache_write_tokens),
        tool_calls: None,
        reasoning: assistant_output.reasoning,
        provider_items: if assistant_output.provider_items.is_empty() {
            None
        } else {
            Some(assistant_output.provider_items)
        },
        segment_id: assistant_output.segment_id,
        llm_api_response: assistant_output.llm_api_response,
        audio: assistant_output.audio_path,
        seq,
        run_id,
    }
}

pub(crate) async fn persist_final_assistant_segment(
    session_store: &SessionStore,
    session_key: &str,
    assistant_output: &AssistantTurnOutput,
    model: &str,
    provider: &str,
    reasoning_effort: Option<String>,
    seq: Option<u64>,
    run_id: &str,
) -> error::Result<usize> {
    let message = build_persisted_assistant_message(
        assistant_output.clone(),
        Some(model.to_string()),
        Some(provider.to_string()),
        reasoning_effort,
        seq,
        Some(run_id.to_string()),
    );

    session_store
        .append_with_index(session_key, &message.to_value())
        .await
        .map_err(|source| {
            error::Error::external("failed to persist final assistant segment", source)
        })
}

pub(crate) fn finalize_persisted_assistant_message(
    assistant_output: AssistantTurnOutput,
    existing: PersistedMessage,
) -> PersistedMessage {
    let PersistedMessage::Assistant {
        content,
        created_at,
        model,
        provider,
        reasoning_effort,
        tool_calls,
        reasoning,
        provider_items,
        segment_id,
        seq,
        run_id,
        ..
    } = existing
    else {
        return existing;
    };

    PersistedMessage::Assistant {
        content,
        created_at,
        model,
        provider,
        reasoning_effort,
        input_tokens: Some(assistant_output.input_tokens),
        output_tokens: Some(assistant_output.output_tokens),
        cache_read_tokens: Some(assistant_output.cache_read_tokens),
        cache_write_tokens: Some(assistant_output.cache_write_tokens),
        duration_ms: Some(assistant_output.duration_ms),
        request_input_tokens: Some(assistant_output.request_input_tokens),
        request_output_tokens: Some(assistant_output.request_output_tokens),
        request_cache_read_tokens: Some(assistant_output.request_cache_read_tokens),
        request_cache_write_tokens: Some(assistant_output.request_cache_write_tokens),
        tool_calls,
        reasoning,
        provider_items,
        segment_id,
        llm_api_response: assistant_output.llm_api_response,
        audio: assistant_output.audio_path,
        seq,
        run_id,
    }
}

pub(crate) fn finalize_aborted_tool_segment(
    mut existing: PersistedMessage,
    duration_ms: u64,
) -> PersistedMessage {
    if let PersistedMessage::Assistant {
        duration_ms: persisted_duration,
        ..
    } = &mut existing
    {
        *persisted_duration = Some(duration_ms);
    }
    existing
}

pub(crate) fn latest_tool_segment_index(
    tool_segment_indices: &HashMap<String, usize>,
) -> Option<usize> {
    tool_segment_indices.values().copied().max()
}

pub struct LiveChatService {
    pub(in crate::service) providers: Arc<RwLock<ProviderRegistry>>,
    pub(in crate::service) model_store: Arc<RwLock<DisabledModelsStore>>,
    pub(in crate::service) state: Arc<dyn ChatRuntime>,
    pub(in crate::service) active_runs: Arc<RwLock<HashMap<String, CancellationToken>>>,
    pub(in crate::service) active_runs_by_session: Arc<RwLock<HashMap<String, String>>>,
    pub(in crate::service) active_event_forwarders:
        Arc<RwLock<HashMap<String, tokio::task::JoinHandle<EventForwarderResult>>>>,
    pub(in crate::service) terminal_runs: Arc<RwLock<HashSet<String>>>,
    pub(in crate::service) tool_registry: Arc<RwLock<ToolRegistry>>,
    pub(in crate::service) session_store: Arc<SessionStore>,
    pub(in crate::service) session_metadata: Arc<SqliteSessionMetadata>,
    pub(in crate::service) session_state_store: Option<Arc<SessionStateStore>>,
    pub(in crate::service) hook_registry: Option<Arc<chelix_common::hooks::HookRegistry>>,
    /// Per-session coordinator ensuring session history mutations do not race chat turns.
    pub(in crate::service) session_mutations: Arc<SessionMutationCoordinator>,
    /// Durable per-session queue of prompts submitted during an active run.
    pub(in crate::service) prompt_queue: Arc<PromptQueue>,
    /// Per-session last-seen client sequence number for ordering diagnostics.
    pub(in crate::service) last_client_seq: Arc<RwLock<HashMap<String, u64>>>,
    /// Per-session active tool invocation lifecycle snapshots for `chat.peek`.
    pub(in crate::service) active_tool_invocations:
        Arc<RwLock<HashMap<String, Vec<ActiveToolInvocation>>>>,
    /// Per-session streamed assistant content buffered so an abort can persist
    /// what the user already saw instead of dropping it on the floor.
    pub(in crate::service) active_partial_assistant:
        Arc<RwLock<HashMap<String, ActiveAssistantDraft>>>,
    /// Per-session reply medium for active runs, so the frontend can restore
    /// `voicePending` state after a page reload.
    pub(in crate::service) active_reply_medium: Arc<RwLock<HashMap<String, ReplyMedium>>>,
    /// Startup configuration snapshot for non-agent chat settings.
    pub(in crate::service) config: chelix_config::ChelixConfig,
    /// Live agent registry shared with agent CRUD and `spawn_agent`.
    pub(in crate::service) agents_config: Arc<RwLock<chelix_config::AgentsConfig>>,
    /// Source used to reload `[tools]` before each new agent run.
    pub(in crate::service) tools_config_source: chelix_config::ToolsConfigSource,
}

async fn runtime_config_for_agent_run(
    base: &chelix_config::ChelixConfig,
    agents_config: &RwLock<chelix_config::AgentsConfig>,
    tools_config_source: &chelix_config::ToolsConfigSource,
) -> error::Result<chelix_config::ChelixConfig> {
    let tools = tools_config_source
        .load()
        .map_err(|error| error::Error::message(format!("reload tools config: {error}")))?;
    let agents = agents_config.read().await.clone();
    let mut config = base.clone();
    config.agents = agents;
    config.tools = tools;
    Ok(config)
}

impl LiveChatService {
    pub fn new(
        providers: Arc<RwLock<ProviderRegistry>>,
        model_store: Arc<RwLock<DisabledModelsStore>>,
        state: Arc<dyn ChatRuntime>,
        session_store: Arc<SessionStore>,
        session_metadata: Arc<SqliteSessionMetadata>,
        prompt_queue_store: Arc<SessionPromptQueueStore>,
        config: chelix_config::ChelixConfig,
        agents_config: Arc<RwLock<chelix_config::AgentsConfig>>,
        tools_config_source: chelix_config::ToolsConfigSource,
    ) -> Self {
        let prompt_queue = Arc::new(PromptQueue::new(prompt_queue_store, Arc::clone(&state)));
        Self {
            providers,
            model_store,
            state,
            active_runs: Arc::new(RwLock::new(HashMap::new())),
            active_runs_by_session: Arc::new(RwLock::new(HashMap::new())),
            active_event_forwarders: Arc::new(RwLock::new(HashMap::new())),
            terminal_runs: Arc::new(RwLock::new(HashSet::new())),
            tool_registry: Arc::new(RwLock::new(ToolRegistry::new())),
            session_store,
            session_metadata,
            session_state_store: None,
            prompt_queue,
            hook_registry: None,
            session_mutations: Arc::new(SessionMutationCoordinator::default()),
            last_client_seq: Arc::new(RwLock::new(HashMap::new())),
            active_tool_invocations: Arc::new(RwLock::new(HashMap::new())),
            active_partial_assistant: Arc::new(RwLock::new(HashMap::new())),
            active_reply_medium: Arc::new(RwLock::new(HashMap::new())),
            config,
            agents_config,
            tools_config_source,
        }
    }

    pub fn with_tools(mut self, registry: Arc<RwLock<ToolRegistry>>) -> Self {
        self.tool_registry = registry;
        self
    }

    pub(in crate::service) async fn load_runtime_config_for_agent_run(
        &self,
    ) -> error::Result<chelix_config::ChelixConfig> {
        runtime_config_for_agent_run(
            &self.config,
            self.agents_config.as_ref(),
            &self.tools_config_source,
        )
        .await
    }

    pub(in crate::service) async fn load_prompt_persona_for_agent_run(
        &self,
        session_key: &str,
        session_entry: Option<&chelix_sessions::metadata::SessionEntry>,
    ) -> error::Result<PromptPersona> {
        let config = self.load_runtime_config_for_agent_run().await?;
        load_prompt_persona_for_session(
            &config,
            session_key,
            session_entry,
            self.session_state_store.as_deref(),
        )
        .await
    }

    pub fn with_session_mutations(mut self, mutations: Arc<SessionMutationCoordinator>) -> Self {
        self.session_mutations = mutations;
        self
    }

    pub fn with_session_state_store(mut self, store: Arc<SessionStateStore>) -> Self {
        self.session_state_store = Some(store);
        self
    }

    pub fn with_hooks(mut self, registry: chelix_common::hooks::HookRegistry) -> Self {
        self.hook_registry = Some(Arc::new(registry));
        self
    }

    pub fn with_hooks_arc(mut self, registry: Arc<chelix_common::hooks::HookRegistry>) -> Self {
        self.hook_registry = Some(registry);
        self
    }

    pub(in crate::service) fn has_tools_sync(&self) -> bool {
        // Best-effort check: try_read avoids blocking. If the lock is held,
        // assume tools are present (conservative — enables tool mode).
        self.tool_registry
            .try_read()
            .map(|r| {
                let schemas = r.list_schemas();
                let has = !schemas.is_empty();
                tracing::debug!(
                    tool_count = schemas.len(),
                    has_tools = has,
                    "has_tools_sync check"
                );
                has
            })
            .unwrap_or(true)
    }

    pub(in crate::service) async fn cancel_run(
        active_runs: &Arc<RwLock<HashMap<String, CancellationToken>>>,
        active_runs_by_session: &Arc<RwLock<HashMap<String, String>>>,
        terminal_runs: &Arc<RwLock<HashSet<String>>>,
        run_id: Option<&str>,
        session_key: Option<&str>,
    ) -> (Option<String>, bool) {
        let resolved_run_id = if let Some(id) = run_id {
            Some(id.to_string())
        } else if let Some(key) = session_key {
            active_runs_by_session.read().await.get(key).cloned()
        } else {
            None
        };

        let Some(target_run_id) = resolved_run_id.clone() else {
            return (None, false);
        };
        let terminal_runs = terminal_runs.read().await;
        if terminal_runs.contains(&target_run_id) {
            return (resolved_run_id, false);
        }
        let cancellation_token = active_runs.read().await.get(&target_run_id).cloned();
        let cancelled = cancellation_token.is_some_and(|token| {
            if token.is_cancelled() {
                return false;
            }
            token.cancel();
            true
        });

        (resolved_run_id, cancelled)
    }

    pub(in crate::service) async fn resolve_session_key_for_run(
        active_runs_by_session: &Arc<RwLock<HashMap<String, String>>>,
        run_id: Option<&str>,
        session_key: Option<&str>,
    ) -> Option<String> {
        if let Some(key) = session_key {
            return Some(key.to_string());
        }
        let target_run_id = run_id?;
        active_runs_by_session
            .read()
            .await
            .iter()
            .find_map(|(key, active_run_id)| (active_run_id == target_run_id).then(|| key.clone()))
    }

    pub(crate) async fn wait_for_event_forwarder(
        active_event_forwarders: &Arc<
            RwLock<HashMap<String, tokio::task::JoinHandle<EventForwarderResult>>>,
        >,
        session_key: &str,
    ) -> EventForwarderResult {
        let handle = active_event_forwarders.write().await.remove(session_key);
        let Some(handle) = handle else {
            return EventForwarderResult::default();
        };

        match handle.await {
            Ok(result) => result,
            Err(e) => {
                let error = format!("runner event forwarder task failed: {e}");
                warn!(
                    session = %session_key,
                    %error,
                    "runner event forwarder unavailable"
                );
                EventForwarderResult {
                    error: Some(error),
                    ..EventForwarderResult::default()
                }
            },
        }
    }

    /// Resolve a provider from session metadata, history, or first registered.
    pub(in crate::service) async fn resolve_provider(
        &self,
        session_key: &str,
        history: &[Value],
    ) -> error::Result<Arc<dyn chelix_agents::model::LlmProvider>> {
        let reg = self.providers.read().await;
        let session_model = self
            .session_metadata
            .get(session_key)
            .await
            .and_then(|e| e.model.clone());
        let history_model = history
            .iter()
            .rev()
            .find_map(|m| m.get("model").and_then(|v| v.as_str()).map(String::from));
        let model_id = session_model.or(history_model);

        model_id
            .and_then(|id| reg.get(&id))
            .or_else(|| reg.first())
            .ok_or_else(|| error::Error::message("no LLM providers configured"))
    }

    /// Resolve the active session key for a connection.
    pub(in crate::service) async fn session_key_for(&self, conn_id: Option<&str>) -> String {
        if let Some(cid) = conn_id
            && let Some(key) = self.state.active_session_key(cid).await
        {
            return key;
        }
        "main".to_string()
    }

    /// Resolve the effective session key for chat operations.
    ///
    /// Precedence is:
    /// 1. Internal `_session_key` overrides used by runtime-owned callers.
    /// 2. Public `sessionKey` / `session_key` request parameters.
    /// 3. Connection-scoped active session derived from `_conn_id`.
    /// 4. The default `"main"` session.
    pub(in crate::service) async fn resolve_session_key_from_params(
        &self,
        params: &Value,
    ) -> String {
        if let Some(session_key) = params
            .get("_session_key")
            .and_then(|v| v.as_str())
            .filter(|v| !v.is_empty())
        {
            return session_key.to_string();
        }
        if let Some(session_key) = params
            .get("sessionKey")
            .or_else(|| params.get("session_key"))
            .and_then(|v| v.as_str())
            .filter(|v| !v.is_empty())
        {
            return session_key.to_string();
        }
        let conn_id = params.get("_conn_id").and_then(|v| v.as_str());
        self.session_key_for(conn_id).await
    }

    /// Resolve the project context prompt section for a session.
    pub(in crate::service) async fn resolve_project_context(
        &self,
        session_key: &str,
        conn_id: Option<&str>,
    ) -> Option<String> {
        let project_id = if let Some(cid) = conn_id {
            self.state.active_project_id(cid).await
        } else {
            None
        };
        // Also check session metadata for project binding (async path).
        let project_id = match project_id {
            Some(pid) => Some(pid),
            None => self
                .session_metadata
                .get(session_key)
                .await
                .and_then(|e| e.project_id),
        };

        let pid = project_id?;
        let val = self
            .state
            .project_service()
            .get(serde_json::json!({"id": pid}))
            .await
            .ok()?;
        let dir = val.get("directory").and_then(|v| v.as_str())?;
        let files = match chelix_projects::context::load_context_files(Path::new(dir)) {
            Ok(f) => f,
            Err(e) => {
                warn!("failed to load project context: {e}");
                return None;
            },
        };
        let project: chelix_projects::Project = serde_json::from_value(val.clone()).ok()?;
        let worktree_dir = self
            .session_metadata
            .get(session_key)
            .await
            .and_then(|e| e.worktree_branch)
            .and_then(|_| {
                let wt_path = Path::new(dir).join(".chelix-worktrees").join(session_key);
                if wt_path.exists() {
                    Some(wt_path)
                } else {
                    None
                }
            });
        let ctx = chelix_projects::ProjectContext {
            project,
            context_files: files,
            worktree_dir,
        };
        Some(ctx.to_prompt_section())
    }

    /// Build the session's system prompt and native tool schemas exactly as a
    /// regular turn would.
    ///
    /// Used by summarization so the request prefix (system prompt + tools +
    /// history) matches the previous turn byte-for-byte and hits the
    /// provider's prompt cache.
    pub(in crate::service) async fn session_prompt_context(
        &self,
        session_key: &str,
        history: &[Value],
        provider: &Arc<dyn chelix_agents::model::LlmProvider>,
        params: &Value,
    ) -> error::Result<(String, Vec<Value>)> {
        let tool_mode = effective_tool_mode(&**provider);
        let native_tools = matches!(tool_mode, ToolMode::Native);
        let tools_enabled = !matches!(tool_mode, ToolMode::Off);

        let session_entry = self.session_metadata.get(session_key).await;
        let persona = self
            .load_prompt_persona_for_agent_run(session_key, session_entry.as_ref())
            .await?;
        let mut runtime_context = build_prompt_runtime_context(
            &self.state,
            &persona.config,
            provider,
            session_key,
            session_entry.as_ref(),
        )
        .await;
        apply_request_runtime_context(
            &mut runtime_context.host,
            params,
            persona
                .user
                .timezone
                .as_ref()
                .map(|timezone| timezone.name()),
        );

        let conn_id = params.get("_conn_id").and_then(|v| v.as_str());
        let project_context = self.resolve_project_context(session_key, conn_id).await;

        let discovered_skills = discover_skills_if_enabled(&persona.config).await;
        let mcp_disabled = session_entry
            .as_ref()
            .and_then(|entry| entry.mcp_disabled)
            .unwrap_or(false);
        let agent_id = persona.agent_id.clone();
        let discovered_skills = filter_skills_for_agent(discovered_skills, &persona.agent.skills);

        let policy_ctx = build_policy_context(&agent_id, Some(&runtime_context), Some(params));
        let filtered_registry = {
            let registry_guard = self.tool_registry.read().await;
            let memory_setup = self
                .state
                .memory_manager()
                .map(|manager| (manager, Arc::clone(provider)));
            prepare_run_registry(
                &registry_guard,
                &persona.config,
                &discovered_skills,
                mcp_disabled,
                &policy_ctx,
                tools_enabled,
                &agent_id,
                memory_setup,
                history,
            )
        }
        .map_err(|e| error::Error::message(e.to_string()))?;

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

        let tools = if native_tools {
            filtered_registry.list_schemas()
        } else {
            Vec::new()
        };
        Ok((prompt_build.prompt, tools))
    }
}

#[cfg(test)]
mod tests {
    use {
        super::{
            ActiveAssistantDraft, build_persisted_assistant_message, build_persisted_tool_call,
            finalize_aborted_tool_segment, finalize_persisted_assistant_message,
            latest_tool_segment_index, persist_active_assistant_draft,
            persist_final_assistant_segment, runtime_config_for_agent_run,
        },
        crate::types::AssistantTurnOutput,
        chelix_agents::model::Usage,
        chelix_common::{
            ProviderItemId, ProviderItemPosition, ProviderItemUpdate, ProviderItemUpdatePayload,
            ProviderSegmentId,
        },
        chelix_sessions::{PersistedMessage, store::SessionStore},
        std::{collections::HashMap, sync::Arc},
        tokio::sync::RwLock,
    };

    #[tokio::test]
    async fn runtime_config_uses_live_agent_registry_updates() -> crate::error::Result<()> {
        let mut initial = chelix_config::AgentsConfig {
            default: "main".to_string(),
            ..Default::default()
        };
        initial
            .entries
            .insert("main".to_string(), chelix_config::AgentConfig {
                name: "Main".to_string(),
                ..Default::default()
            });
        let live = RwLock::new(initial);
        let base = chelix_config::ChelixConfig::default();
        let tools = chelix_config::ToolsConfigSource::snapshot(
            chelix_config::schema::ToolsConfig::default(),
        );

        let first = runtime_config_for_agent_run(&base, &live, &tools).await?;
        assert!(first.agents.get("main").is_some());
        assert!(first.agents.get("writer").is_none());

        {
            let mut agents = live.write().await;
            agents.entries.remove("main");
            agents.default = "writer".to_string();
            agents
                .entries
                .insert("writer".to_string(), chelix_config::AgentConfig {
                    name: "Writer".to_string(),
                    ..Default::default()
                });
        }

        let updated = runtime_config_for_agent_run(&base, &live, &tools).await?;
        assert!(updated.agents.get("main").is_none());
        assert_eq!(updated.agents.default, "writer");
        assert!(updated.agents.get("writer").is_some());
        Ok(())
    }

    #[test]
    fn latest_tool_segment_index_outlives_completed_tool_calls() {
        let segments = HashMap::from([
            ("completed-tool".to_string(), 11_usize),
            ("active-tool".to_string(), 7_usize),
        ]);

        assert_eq!(latest_tool_segment_index(&segments), Some(11));
    }

    #[test]
    fn active_assistant_draft_omits_cache_usage_fields() {
        let mut draft = ActiveAssistantDraft::new(
            "run-1",
            "gpt-4.1",
            "openai",
            Some("high".to_string()),
            Some(7),
        );
        let seg_id = ProviderSegmentId::new("resp_test");
        draft
            .apply_update(&ProviderItemUpdate {
                segment_id: seg_id.clone(),
                item_id: ProviderItemId::new("msg_0"),
                position: ProviderItemPosition::new(0),
                update_seq: 1,
                payload: ProviderItemUpdatePayload::MessageDone {
                    text: "hello".to_string(),
                },
            })
            .unwrap_or_else(|error| panic!("draft accepts provider update: {error}"));
        draft
            .apply_update(&ProviderItemUpdate {
                segment_id: seg_id,
                item_id: ProviderItemId::new("rs_0"),
                position: ProviderItemPosition::new(1),
                update_seq: 1,
                payload: ProviderItemUpdatePayload::ReasoningText {
                    text: "thinking".to_string(),
                },
            })
            .unwrap_or_else(|error| panic!("draft accepts provider update: {error}"));

        let message = draft.to_persisted_message(None, None);

        match message {
            PersistedMessage::Assistant {
                cache_read_tokens,
                cache_write_tokens,
                request_cache_read_tokens,
                request_cache_write_tokens,
                seq,
                run_id,
                ..
            } => {
                assert_eq!(cache_read_tokens, None);
                assert_eq!(cache_write_tokens, None);
                assert_eq!(request_cache_read_tokens, None);
                assert_eq!(request_cache_write_tokens, None);
                assert_eq!(seq, Some(7));
                assert_eq!(run_id.as_deref(), Some("run-1"));
            },
            _ => panic!("expected assistant message"),
        }
    }

    #[test]
    fn persisted_assistant_message_includes_cache_usage_fields() {
        let message = build_persisted_assistant_message(
            AssistantTurnOutput {
                text: "hello".to_string(),
                persisted_message_index: None,
                input_tokens: 1200,
                output_tokens: 80,
                cache_read_tokens: 1050,
                cache_write_tokens: 4,
                duration_ms: 250,
                request_input_tokens: 900,
                request_output_tokens: 60,
                request_cache_read_tokens: 850,
                request_cache_write_tokens: 2,
                audio_path: None,
                reasoning: Some(chelix_common::ReasoningContent::Text(
                    "thinking".to_string(),
                )),
                provider_items: Vec::new(),
                segment_id: None,
                llm_api_response: None,
            },
            Some("gpt-4.1".to_string()),
            Some("openai".to_string()),
            Some("high".to_string()),
            Some(7),
            Some("run-1".to_string()),
        );

        match message {
            PersistedMessage::Assistant {
                cache_read_tokens,
                cache_write_tokens,
                request_cache_read_tokens,
                request_cache_write_tokens,
                ..
            } => {
                assert_eq!(cache_read_tokens, Some(1050));
                assert_eq!(cache_write_tokens, Some(4));
                assert_eq!(request_cache_read_tokens, Some(850));
                assert_eq!(request_cache_write_tokens, Some(2));
            },
            _ => panic!("expected assistant message"),
        }
    }

    #[tokio::test]
    async fn assistant_deltas_accumulate_without_reserving_storage() {
        let directory = tempfile::tempdir()
            .unwrap_or_else(|error| panic!("temporary session directory: {error}"));
        let store = SessionStore::new(directory.path().to_path_buf());
        store
            .append(
                "main",
                &serde_json::json!({ "role": "user", "content": "hello" }),
            )
            .await
            .unwrap_or_else(|error| panic!("user message persists: {error}"));
        let mut draft = ActiveAssistantDraft::new("run-1", "model-1", "provider-1", None, Some(7));
        let seg_id = ProviderSegmentId::new("resp_test");
        draft
            .apply_update(&ProviderItemUpdate {
                segment_id: seg_id.clone(),
                item_id: ProviderItemId::new("msg_0"),
                position: ProviderItemPosition::new(0),
                update_seq: 1,
                payload: ProviderItemUpdatePayload::MessageDelta {
                    delta: "first".to_string(),
                },
            })
            .unwrap_or_else(|error| panic!("draft accepts provider update: {error}"));
        draft
            .apply_update(&ProviderItemUpdate {
                segment_id: seg_id,
                item_id: ProviderItemId::new("msg_0"),
                position: ProviderItemPosition::new(0),
                update_seq: 2,
                payload: ProviderItemUpdatePayload::MessageDelta {
                    delta: " second".to_string(),
                },
            })
            .unwrap_or_else(|error| panic!("draft accepts provider update: {error}"));
        let drafts = Arc::new(RwLock::new(HashMap::from([("main".to_string(), draft)])));
        assert_eq!(
            store
                .count("main")
                .await
                .unwrap_or_else(|error| panic!("history count: {error}")),
            1
        );
        let drafts_guard = drafts.read().await;
        let message = drafts_guard
            .get("main")
            .unwrap_or_else(|| panic!("active draft remains"))
            .to_persisted_message(None, None);
        drop(drafts_guard);
        assert!(matches!(
            message,
            PersistedMessage::Assistant { content, .. } if content == "first second"
        ));
    }

    #[tokio::test]
    async fn active_partial_uses_the_tail_after_interleaved_lifecycle_records() {
        let directory = tempfile::tempdir()
            .unwrap_or_else(|error| panic!("temporary session directory: {error}"));
        let store = SessionStore::new(directory.path().to_path_buf());
        store
            .append(
                "main",
                &serde_json::json!({ "role": "user", "content": "hello" }),
            )
            .await
            .unwrap_or_else(|error| panic!("user message persists: {error}"));
        let mut draft = ActiveAssistantDraft::new("run-1", "model-1", "provider-1", None, Some(7));
        let seg_id = ProviderSegmentId::new("resp_test");
        draft
            .apply_update(&ProviderItemUpdate {
                segment_id: seg_id,
                item_id: ProviderItemId::new("msg_0"),
                position: ProviderItemPosition::new(0),
                update_seq: 1,
                payload: ProviderItemUpdatePayload::MessageDelta {
                    delta: "partial".to_string(),
                },
            })
            .unwrap_or_else(|error| panic!("draft accepts provider update: {error}"));
        let drafts = Arc::new(RwLock::new(HashMap::from([("main".to_string(), draft)])));
        store
            .append(
                "main",
                &serde_json::json!({
                    "role": "tool_lifecycle",
                    "toolCallId": "call-1",
                    "toolName": "read_file",
                    "sequence": 0,
                    "emittedAtMs": 1,
                    "runId": "run-1",
                    "stage": "created",
                    "providerIndex": 0
                }),
            )
            .await
            .unwrap_or_else(|error| panic!("lifecycle record persists: {error}"));

        let (partial, persisted_index) = persist_active_assistant_draft(&store, &drafts, "main")
            .await
            .unwrap_or_else(|error| panic!("partial persistence succeeds: {error}"))
            .unwrap_or_else(|| panic!("visible partial is persisted"));

        assert_eq!(persisted_index, 2);
        assert_eq!(partial["content"], "partial");
        assert!(!drafts.read().await.contains_key("main"));
        let history = store
            .read("main")
            .await
            .unwrap_or_else(|error| panic!("session history reads: {error}"));
        assert_eq!(history.len(), 3);
        assert_eq!(history[1]["role"], "tool_lifecycle");
        assert_eq!(history[persisted_index]["content"], "partial");
    }

    #[tokio::test]
    async fn opaque_only_active_partial_is_persisted() {
        let directory = tempfile::tempdir()
            .unwrap_or_else(|error| panic!("temporary session directory: {error}"));
        let store = SessionStore::new(directory.path().to_path_buf());
        store
            .append(
                "main",
                &serde_json::json!({ "role": "user", "content": "hello" }),
            )
            .await
            .unwrap_or_else(|error| panic!("user message persists: {error}"));
        let mut draft = ActiveAssistantDraft::new("run-1", "model-1", "provider-1", None, Some(7));
        let seg_id = ProviderSegmentId::new("resp_test");
        draft
            .apply_update(&ProviderItemUpdate {
                segment_id: seg_id,
                item_id: ProviderItemId::new("rs_partial"),
                position: ProviderItemPosition::new(0),
                update_seq: 1,
                payload: ProviderItemUpdatePayload::ReasoningItemDone {
                    encrypted_content: Some("opaque-partial".to_string()),
                },
            })
            .unwrap_or_else(|error| panic!("draft accepts provider update: {error}"));
        let drafts = Arc::new(RwLock::new(HashMap::from([("main".to_string(), draft)])));

        let (partial, persisted_index) = persist_active_assistant_draft(&store, &drafts, "main")
            .await
            .unwrap_or_else(|error| panic!("opaque partial persistence succeeds: {error}"))
            .unwrap_or_else(|| panic!("opaque partial is persisted"));

        assert_eq!(persisted_index, 1);
        assert_eq!(partial["content"], "");
        assert!(partial.get("reasoning").is_none());
        assert!(!drafts.read().await.contains_key("main"));
        let history = store
            .read("main")
            .await
            .unwrap_or_else(|error| panic!("session history reads: {error}"));
        assert_eq!(history.len(), 2);
    }

    #[tokio::test]
    async fn persist_final_assistant_segment_returns_its_physical_history_index() {
        let directory = tempfile::tempdir()
            .unwrap_or_else(|error| panic!("temporary session directory: {error}"));
        let store = SessionStore::new(directory.path().to_path_buf());
        store
            .append(
                "main",
                &serde_json::json!({ "role": "user", "content": "hello" }),
            )
            .await
            .unwrap_or_else(|error| panic!("user message persists: {error}"));
        let assistant_output = AssistantTurnOutput {
            text: "streamed response".to_string(),
            persisted_message_index: None,
            input_tokens: 12,
            output_tokens: 8,
            cache_read_tokens: 2,
            cache_write_tokens: 1,
            duration_ms: 250,
            request_input_tokens: 12,
            request_output_tokens: 8,
            request_cache_read_tokens: 2,
            request_cache_write_tokens: 1,
            audio_path: None,
            reasoning: Some(chelix_common::ReasoningContent::Text(
                "streamed reasoning".to_string(),
            )),
            provider_items: Vec::new(),
            segment_id: None,
            llm_api_response: None,
        };

        let message_index = persist_final_assistant_segment(
            &store,
            "main",
            &assistant_output,
            "model-1",
            "provider-1",
            Some("high".to_string()),
            Some(7),
            "run-1",
        )
        .await
        .unwrap_or_else(|error| panic!("assistant segment persists: {error}"));

        assert_eq!(message_index, 1);
        let history = store
            .read("main")
            .await
            .unwrap_or_else(|error| panic!("session history reads: {error}"));
        assert_eq!(history.len(), 2);
        assert_eq!(history[1]["role"], "assistant");
        assert_eq!(history[1]["content"], "streamed response");
        assert_eq!(history[1]["reasoning"], "streamed reasoning");
        assert_eq!(history[1]["model"], "model-1");
        assert_eq!(history[1]["provider"], "provider-1");
        assert_eq!(history[1]["seq"], 7);
        assert_eq!(history[1]["run_id"], "run-1");
    }

    #[test]
    fn tool_segment_finalization_preserves_canonical_output_and_run_raw_payload() {
        let mut draft = ActiveAssistantDraft::new(
            "run-1",
            "gpt-4.1",
            "openai",
            Some("high".to_string()),
            Some(7),
        );
        let seg_id = ProviderSegmentId::new("resp_test");
        draft
            .apply_update(&ProviderItemUpdate {
                segment_id: seg_id.clone(),
                item_id: ProviderItemId::new("rs_tool_segment"),
                position: ProviderItemPosition::new(0),
                update_seq: 1,
                payload: ProviderItemUpdatePayload::ReasoningText {
                    text: "Initial reasoning.".to_string(),
                },
            })
            .unwrap_or_else(|error| panic!("draft accepts provider update: {error}"));
        draft
            .apply_update(&ProviderItemUpdate {
                segment_id: seg_id.clone(),
                item_id: ProviderItemId::new("rs_tool_segment"),
                position: ProviderItemPosition::new(0),
                update_seq: 2,
                payload: ProviderItemUpdatePayload::ReasoningItemDone {
                    encrypted_content: Some("opaque-tool-segment".to_string()),
                },
            })
            .unwrap_or_else(|error| panic!("draft accepts provider update: {error}"));
        draft
            .apply_update(&ProviderItemUpdate {
                segment_id: seg_id,
                item_id: ProviderItemId::new("msg_0"),
                position: ProviderItemPosition::new(1),
                update_seq: 1,
                payload: ProviderItemUpdatePayload::MessageDone {
                    text: "Text before tool.".to_string(),
                },
            })
            .unwrap_or_else(|error| panic!("draft accepts provider update: {error}"));
        let segment = draft.to_persisted_message(
            Some(vec![build_persisted_tool_call(
                "tool-1",
                "execute_command",
                Some(serde_json::json!({"command": "true"})),
            )]),
            Some(&Usage {
                input_tokens: 10,
                output_tokens: 2,
                cache_read_tokens: 4,
                cache_write_tokens: 1,
            }),
        );

        let finalized = finalize_persisted_assistant_message(
            AssistantTurnOutput {
                text: "Foreign terminal text.".to_string(),
                persisted_message_index: Some(1),
                input_tokens: 30,
                output_tokens: 8,
                cache_read_tokens: 12,
                cache_write_tokens: 3,
                duration_ms: 250,
                request_input_tokens: 20,
                request_output_tokens: 6,
                request_cache_read_tokens: 9,
                request_cache_write_tokens: 2,
                audio_path: None,
                reasoning: Some(chelix_common::ReasoningContent::Text(
                    "Foreign terminal reasoning.".to_string(),
                )),
                provider_items: Vec::new(),
                segment_id: None,
                llm_api_response: Some(serde_json::json!([{"foreign": "terminal"}])),
            },
            segment,
        );

        match finalized {
            PersistedMessage::Assistant {
                content,
                model,
                provider,
                reasoning_effort,
                input_tokens,
                output_tokens,
                cache_read_tokens,
                cache_write_tokens,
                duration_ms,
                tool_calls,
                reasoning,
                provider_items,
                segment_id,
                llm_api_response,
                seq,
                run_id,
                ..
            } => {
                assert_eq!(content, "Text before tool.");
                assert_eq!(model.as_deref(), Some("gpt-4.1"));
                assert_eq!(provider.as_deref(), Some("openai"));
                assert_eq!(reasoning_effort.as_deref(), Some("high"));
                assert_eq!(input_tokens, Some(30));
                assert_eq!(output_tokens, Some(8));
                assert_eq!(cache_read_tokens, Some(12));
                assert_eq!(cache_write_tokens, Some(3));
                assert_eq!(duration_ms, Some(250));
                assert_eq!(
                    tool_calls.as_ref().map(|calls| calls[0].id.as_str()),
                    Some("tool-1")
                );
                assert_eq!(
                    reasoning,
                    Some(chelix_common::ReasoningContent::Text(
                        "Initial reasoning.".to_string(),
                    ))
                );
                assert!(provider_items.is_some());
                assert!(segment_id.is_some());
                assert_eq!(
                    llm_api_response,
                    Some(serde_json::json!([{"foreign": "terminal"}]))
                );
                assert_eq!(seq, Some(7));
                assert_eq!(run_id.as_deref(), Some("run-1"));
            },
            _ => panic!("expected assistant message"),
        }
    }

    #[test]
    fn aborted_tool_segment_finalization_preserves_usage_and_tool_calls() {
        let segment = PersistedMessage::Assistant {
            content: "Text before tool.".to_string(),
            created_at: Some(10),
            model: Some("gpt-4.1".to_string()),
            provider: Some("openai".to_string()),
            reasoning_effort: Some("high".to_string()),
            input_tokens: Some(30),
            output_tokens: Some(8),
            cache_read_tokens: Some(12),
            cache_write_tokens: Some(3),
            duration_ms: None,
            request_input_tokens: Some(20),
            request_output_tokens: Some(6),
            request_cache_read_tokens: Some(9),
            request_cache_write_tokens: Some(2),
            tool_calls: Some(vec![build_persisted_tool_call(
                "tool-1",
                "execute_command",
                Some(serde_json::json!({"command": "true"})),
            )]),
            reasoning: Some(chelix_common::ReasoningContent::Text(
                "Initial reasoning.".to_string(),
            )),
            provider_items: None,
            segment_id: None,
            llm_api_response: None,
            audio: None,
            seq: Some(7),
            run_id: Some("run-1".to_string()),
        };

        let finalized = finalize_aborted_tool_segment(segment, 250);

        match finalized {
            PersistedMessage::Assistant {
                content,
                input_tokens,
                output_tokens,
                cache_read_tokens,
                cache_write_tokens,
                duration_ms,
                tool_calls,
                reasoning,
                ..
            } => {
                assert_eq!(content, "Text before tool.");
                assert_eq!(input_tokens, Some(30));
                assert_eq!(output_tokens, Some(8));
                assert_eq!(cache_read_tokens, Some(12));
                assert_eq!(cache_write_tokens, Some(3));
                assert_eq!(duration_ms, Some(250));
                assert_eq!(tool_calls.as_ref().map(Vec::len), Some(1));
                assert_eq!(
                    reasoning,
                    Some(chelix_common::ReasoningContent::Text(
                        "Initial reasoning.".to_string(),
                    ))
                );
            },
            _ => panic!("expected assistant message"),
        }
    }
}
