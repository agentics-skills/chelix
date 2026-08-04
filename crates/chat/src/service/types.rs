//! `LiveChatService` struct, constructors, and helper methods.

use std::{
    collections::{HashMap, HashSet},
    path::Path,
    sync::Arc,
};

use {
    serde::Serialize,
    serde_json::Value,
    tokio::{sync::RwLock, task::AbortHandle},
    tracing::warn,
};

use {
    chelix_agents::tool_registry::ToolRegistry,
    chelix_providers::ProviderRegistry,
    chelix_service_traits::SessionMutationCoordinator,
    chelix_sessions::{
        PersistedMessage,
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
        prepare_run_registry, prompt_build_limits_from_config, resolve_prompt_agent_id,
        resolve_prompt_mode_context,
    },
    runtime::ChatRuntime,
    types::*,
};

/// A message that arrived while an agent run was already active on the session.
#[derive(Debug, Clone)]
pub(in crate::service) struct QueuedMessage {
    pub(in crate::service) params: Value,
}

/// A tool call currently executing within an active agent run.
#[derive(Debug, Clone, Serialize)]
pub struct ActiveToolCall {
    #[serde(rename = "runId")]
    pub run_id: String,
    #[serde(rename = "toolCallId")]
    pub id: String,
    #[serde(rename = "toolName")]
    pub name: String,
    pub arguments: Value,
    #[serde(rename = "executionMode", skip_serializing_if = "Option::is_none")]
    pub execution_mode: Option<String>,
    #[serde(rename = "startedAt")]
    pub started_at: u64,
}

#[derive(Debug, Clone)]
pub(crate) struct ActiveAssistantDraft {
    content: String,
    reasoning: String,
    model: String,
    provider: String,
    reasoning_effort: Option<String>,
    seq: Option<u64>,
    run_id: String,
    message_index: Option<usize>,
}

#[derive(Default)]
pub(crate) struct EventForwarderResult {
    pub(crate) reasoning: String,
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
            content: String::new(),
            reasoning: String::new(),
            model: model.to_string(),
            provider: provider.to_string(),
            reasoning_effort,
            seq,
            run_id: run_id.to_string(),
            message_index: None,
        }
    }

    pub(crate) fn append_text(&mut self, delta: &str) {
        if !delta.is_empty() {
            self.content.push_str(delta);
        }
    }

    pub(crate) fn set_reasoning(&mut self, reasoning: &str) {
        self.reasoning.clear();
        self.reasoning.push_str(reasoning);
    }

    pub(crate) fn message_index(&self) -> Option<usize> {
        self.message_index
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

    pub(crate) fn has_visible_content(&self) -> bool {
        !self.content.trim().is_empty() || !self.reasoning.trim().is_empty()
    }

    fn has_visible_text(&self) -> bool {
        !self.content.trim().is_empty()
    }

    pub(crate) fn to_persisted_message(
        &self,
        tool_calls: Option<Vec<PersistedToolCall>>,
        usage: Option<&chelix_agents::model::Usage>,
    ) -> PersistedMessage {
        let reasoning = self.reasoning.trim();
        PersistedMessage::Assistant {
            content: self.content.clone(),
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
            reasoning: (!reasoning.is_empty()).then(|| reasoning.to_string()),
            llm_api_response: None,
            audio: None,
            seq: self.seq,
            run_id: Some(self.run_id.clone()),
        }
    }
}

pub(crate) async fn append_assistant_delta(
    session_store: &SessionStore,
    active_partial_assistant: &Arc<RwLock<HashMap<String, ActiveAssistantDraft>>>,
    session_key: &str,
    delta: &str,
) -> error::Result<Option<usize>> {
    let mut drafts = active_partial_assistant.write().await;
    let draft = drafts.get_mut(session_key).ok_or_else(|| {
        error::Error::message(format!(
            "active assistant draft is unavailable for session '{session_key}'"
        ))
    })?;
    draft.append_text(delta);
    if !draft.has_visible_text() {
        return Ok(None);
    }
    let message_index = reserve_draft_message_index(session_store, session_key, draft).await?;
    Ok(Some(message_index))
}

pub(crate) async fn reserve_assistant_message_index(
    session_store: &SessionStore,
    active_partial_assistant: &Arc<RwLock<HashMap<String, ActiveAssistantDraft>>>,
    session_key: &str,
) -> error::Result<usize> {
    let mut drafts = active_partial_assistant.write().await;
    let draft = drafts.get_mut(session_key).ok_or_else(|| {
        error::Error::message(format!(
            "active assistant draft is unavailable for session '{session_key}'"
        ))
    })?;
    reserve_draft_message_index(session_store, session_key, draft).await
}

async fn reserve_draft_message_index(
    session_store: &SessionStore,
    session_key: &str,
    draft: &mut ActiveAssistantDraft,
) -> error::Result<usize> {
    if let Some(message_index) = draft.message_index() {
        return Ok(message_index);
    }

    let message_index = session_store.count(session_key).await.map_err(|source| {
        error::Error::external("failed to reserve assistant message index", source)
    })?;
    let message_index = usize::try_from(message_index).map_err(|source| {
        error::Error::external("assistant message index exceeds usize", source)
    })?;
    draft.message_index = Some(message_index);
    Ok(message_index)
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
    if !draft.has_visible_content() {
        drafts.remove(session_key);
        return Ok(None);
    }

    let message_index = reserve_draft_message_index(session_store, session_key, draft).await?;
    let draft = drafts.remove(session_key).ok_or_else(|| {
        error::Error::message(format!(
            "active assistant draft disappeared for session '{session_key}'"
        ))
    })?;
    let partial_value = draft.to_persisted_message(None, None).to_value();
    session_store
        .append_at_index(session_key, &partial_value, message_index)
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
    metadata: Option<serde_json::Map<String, Value>>,
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
        metadata,
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
    message_index: usize,
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
        .append_at_index(session_key, &message.to_value(), message_index)
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
        reasoning: assistant_output.reasoning.or(reasoning),
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
    pub(in crate::service) active_runs: Arc<RwLock<HashMap<String, AbortHandle>>>,
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
    /// Per-session message queue for messages arriving during an active run.
    pub(in crate::service) message_queue: Arc<RwLock<HashMap<String, Vec<QueuedMessage>>>>,
    /// Per-session last-seen client sequence number for ordering diagnostics.
    pub(in crate::service) last_client_seq: Arc<RwLock<HashMap<String, u64>>>,
    /// Per-session accumulated thinking text for active runs, so it can be
    /// returned in `sessions.switch` after a page reload.
    pub(in crate::service) active_thinking_text: Arc<RwLock<HashMap<String, String>>>,
    /// Per-session active tool calls for `chat.peek` snapshot.
    pub(in crate::service) active_tool_calls: Arc<RwLock<HashMap<String, Vec<ActiveToolCall>>>>,
    /// Per-session streamed assistant content buffered so an abort can persist
    /// what the user already saw instead of dropping it on the floor.
    pub(in crate::service) active_partial_assistant:
        Arc<RwLock<HashMap<String, ActiveAssistantDraft>>>,
    /// Per-session reply medium for active runs, so the frontend can restore
    /// `voicePending` state after a page reload.
    pub(in crate::service) active_reply_medium: Arc<RwLock<HashMap<String, ReplyMedium>>>,
    /// Startup configuration snapshot for chat hot-path decisions.
    pub(in crate::service) config: chelix_config::ChelixConfig,
    /// Source used to reload `[tools]` before each new agent run.
    pub(in crate::service) tools_config_source: chelix_config::ToolsConfigSource,
}

impl LiveChatService {
    pub fn new(
        providers: Arc<RwLock<ProviderRegistry>>,
        model_store: Arc<RwLock<DisabledModelsStore>>,
        state: Arc<dyn ChatRuntime>,
        session_store: Arc<SessionStore>,
        session_metadata: Arc<SqliteSessionMetadata>,
        config: chelix_config::ChelixConfig,
        tools_config_source: chelix_config::ToolsConfigSource,
    ) -> Self {
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
            hook_registry: None,
            session_mutations: Arc::new(SessionMutationCoordinator::default()),
            message_queue: Arc::new(RwLock::new(HashMap::new())),
            last_client_seq: Arc::new(RwLock::new(HashMap::new())),
            active_thinking_text: Arc::new(RwLock::new(HashMap::new())),
            active_tool_calls: Arc::new(RwLock::new(HashMap::new())),
            active_partial_assistant: Arc::new(RwLock::new(HashMap::new())),
            active_reply_medium: Arc::new(RwLock::new(HashMap::new())),
            config,
            tools_config_source,
        }
    }

    pub fn with_tools(mut self, registry: Arc<RwLock<ToolRegistry>>) -> Self {
        self.tool_registry = registry;
        self
    }

    pub(in crate::service) async fn load_prompt_persona_for_agent_run(
        &self,
        session_key: &str,
        session_entry: Option<&chelix_sessions::metadata::SessionEntry>,
    ) -> error::Result<PromptPersona> {
        let tools_config = self
            .tools_config_source
            .load()
            .map_err(|error| error::Error::message(format!("reload tools config: {error}")))?;
        let mut persona = load_prompt_persona_for_session(
            &self.config,
            session_key,
            session_entry,
            self.session_state_store.as_deref(),
        )
        .await;
        persona.config.tools = tools_config;
        Ok(persona)
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

    pub(in crate::service) async fn abort_run_handle(
        active_runs: &Arc<RwLock<HashMap<String, AbortHandle>>>,
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

        if terminal_runs.read().await.contains(&target_run_id) {
            return (resolved_run_id, false);
        }

        let abort_handle = active_runs.write().await.remove(&target_run_id);
        let aborted = if let Some(handle) = abort_handle {
            terminal_runs.write().await.insert(target_run_id.clone());
            handle.abort();
            true
        } else {
            false
        };

        let mut by_session = active_runs_by_session.write().await;
        if let Some(key) = session_key
            && by_session.get(key).is_some_and(|id| id == &target_run_id)
        {
            by_session.remove(key);
        }
        by_session.retain(|_, id| id != &target_run_id);

        (resolved_run_id, aborted)
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

    pub(in crate::service) async fn persist_partial_assistant_on_abort(
        &self,
        session_key: &str,
    ) -> error::Result<Option<(Value, usize)>> {
        persist_active_assistant_draft(
            &self.session_store,
            &self.active_partial_assistant,
            session_key,
        )
        .await
    }

    pub(in crate::service) async fn finalize_active_tool_segment_on_abort(
        &self,
        session_key: &str,
        tool_segment_indices: &HashMap<String, usize>,
    ) -> error::Result<Option<(Value, usize)>> {
        let Some(message_index) = latest_tool_segment_index(tool_segment_indices) else {
            return Ok(None);
        };
        let finalized = self
            .session_store
            .update_typed_at(session_key, message_index, |existing| {
                let duration_ms = match &existing {
                    PersistedMessage::Assistant { created_at, .. } => created_at
                        .map(|started_at| now_ms().saturating_sub(started_at))
                        .unwrap_or_default(),
                    _ => 0,
                };
                finalize_aborted_tool_segment(existing, duration_ms)
            })
            .await
            .map_err(|source| {
                error::Error::external(
                    "failed to finalize assistant tool segment after abort",
                    source,
                )
            })?;
        if !matches!(finalized, PersistedMessage::Assistant { .. }) {
            return Err(error::Error::message(format!(
                "message index {message_index} is not an assistant tool segment"
            )));
        };

        Ok(Some((finalized.to_value(), message_index)))
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
        runtime_context.mode = resolve_prompt_mode_context(&persona.config, session_entry.as_ref());
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
        let agent_id = resolve_prompt_agent_id(session_entry.as_ref());
        let discovered_skills = if let Some(preset) = persona.config.agents.get_preset(&agent_id) {
            filter_skills_for_agent(discovered_skills, &preset.skills)
        } else {
            discovered_skills
        };

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
            ActiveAssistantDraft, ActiveToolCall, append_assistant_delta,
            build_persisted_assistant_message, build_persisted_tool_call,
            finalize_aborted_tool_segment, finalize_persisted_assistant_message,
            latest_tool_segment_index, persist_active_assistant_draft,
            persist_final_assistant_segment,
        },
        crate::types::AssistantTurnOutput,
        chelix_agents::model::Usage,
        chelix_sessions::{PersistedMessage, store::SessionStore},
        std::{collections::HashMap, sync::Arc},
        tokio::sync::RwLock,
    };

    #[test]
    fn active_tool_call_serializes_switch_payload_shape() {
        let call = ActiveToolCall {
            run_id: "run-1".to_string(),
            id: "tool-1".to_string(),
            name: "browser".to_string(),
            arguments: serde_json::json!({"url": "https://example.com"}),
            execution_mode: Some("sandbox".to_string()),
            started_at: 42,
        };

        let value = serde_json::to_value(call)
            .unwrap_or_else(|error| panic!("active tool call serializes: {error}"));

        assert_eq!(value.get("runId").and_then(|v| v.as_str()), Some("run-1"));
        assert_eq!(
            value.get("toolCallId").and_then(|v| v.as_str()),
            Some("tool-1")
        );
        assert_eq!(
            value.get("toolName").and_then(|v| v.as_str()),
            Some("browser")
        );
        assert_eq!(
            value.get("executionMode").and_then(|v| v.as_str()),
            Some("sandbox")
        );
        assert_eq!(value.get("startedAt").and_then(|v| v.as_u64()), Some(42));
        assert!(value.get("id").is_none());
        assert!(value.get("name").is_none());
    }

    #[test]
    fn active_tool_call_omits_missing_execution_mode() {
        let call = ActiveToolCall {
            run_id: "run-1".to_string(),
            id: "tool-1".to_string(),
            name: "execute_command".to_string(),
            arguments: serde_json::json!({"command": "true"}),
            execution_mode: None,
            started_at: 42,
        };

        let value = serde_json::to_value(call)
            .unwrap_or_else(|error| panic!("active tool call serializes: {error}"));

        assert!(value.get("executionMode").is_none());
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
        draft.append_text("hello");
        draft.set_reasoning("thinking");

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
                reasoning: Some("thinking".to_string()),
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
    async fn assistant_deltas_reserve_one_stable_message_index() {
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
        let drafts = Arc::new(RwLock::new(HashMap::from([(
            "main".to_string(),
            ActiveAssistantDraft::new("run-1", "model-1", "provider-1", None, Some(7)),
        )])));

        let first_index = append_assistant_delta(&store, &drafts, "main", "first")
            .await
            .unwrap_or_else(|error| panic!("first delta reserves an index: {error}"));
        let second_index = append_assistant_delta(&store, &drafts, "main", " second")
            .await
            .unwrap_or_else(|error| panic!("second delta reuses the index: {error}"));

        assert_eq!(first_index, Some(1));
        assert_eq!(second_index, first_index);
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
    async fn active_partial_persists_at_its_reserved_message_index() {
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
        let drafts = Arc::new(RwLock::new(HashMap::from([(
            "main".to_string(),
            ActiveAssistantDraft::new("run-1", "model-1", "provider-1", None, Some(7)),
        )])));
        let reserved_index = append_assistant_delta(&store, &drafts, "main", "partial")
            .await
            .unwrap_or_else(|error| panic!("delta reserves an index: {error}"));

        let (partial, persisted_index) = persist_active_assistant_draft(&store, &drafts, "main")
            .await
            .unwrap_or_else(|error| panic!("partial persistence succeeds: {error}"))
            .unwrap_or_else(|| panic!("visible partial is persisted"));

        assert_eq!(reserved_index, Some(1));
        assert_eq!(persisted_index, 1);
        assert_eq!(partial["content"], "partial");
        assert!(!drafts.read().await.contains_key("main"));
        let history = store
            .read("main")
            .await
            .unwrap_or_else(|error| panic!("session history reads: {error}"));
        assert_eq!(history.len(), 2);
        assert_eq!(history[persisted_index]["content"], "partial");
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
            reasoning: Some("streamed reasoning".to_string()),
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
            1,
        )
        .await
        .unwrap_or_else(|error| panic!("assistant segment persists at reserved index: {error}"));

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

        let duplicate = persist_final_assistant_segment(
            &store,
            "main",
            &assistant_output,
            "model-1",
            "provider-1",
            Some("high".to_string()),
            Some(7),
            "run-1",
            message_index,
        )
        .await;
        assert!(duplicate.is_err());
        assert_eq!(
            store
                .count("main")
                .await
                .unwrap_or_else(|error| panic!("history count: {error}")),
            2
        );
    }

    #[test]
    fn tool_segment_finalization_preserves_canonical_content_and_tool_calls() {
        let mut draft = ActiveAssistantDraft::new(
            "run-1",
            "gpt-4.1",
            "openai",
            Some("high".to_string()),
            Some(7),
        );
        draft.append_text("Text before tool.");
        draft.set_reasoning("Initial reasoning.");
        let segment = draft.to_persisted_message(
            Some(vec![build_persisted_tool_call(
                "tool-1",
                "execute_command",
                Some(serde_json::json!({"command": "true"})),
                None,
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
                text: "Text before tool.".to_string(),
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
                reasoning: Some("Final reasoning.".to_string()),
                llm_api_response: None,
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
                assert_eq!(reasoning.as_deref(), Some("Final reasoning."));
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
                None,
            )]),
            reasoning: Some("Initial reasoning.".to_string()),
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
                assert_eq!(reasoning.as_deref(), Some("Initial reasoning."));
            },
            _ => panic!("expected assistant message"),
        }
    }
}
