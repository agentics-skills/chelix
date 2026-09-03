use super::*;

fn default_channel_session_key(target: &chelix_channels::ChannelReplyTarget) -> String {
    match &target.thread_id {
        Some(thread_id) => format!(
            "{}:{}:{}:{}",
            target.channel_type, target.account_id, target.chat_id, thread_id
        ),
        None => format!(
            "{}:{}:{}",
            target.channel_type, target.account_id, target.chat_id
        ),
    }
}

async fn is_current_channel_session(
    metadata: &SqliteSessionMetadata,
    entry: &chelix_sessions::metadata::SessionEntry,
) -> Result<bool, ServiceError> {
    let Some(binding_json) = entry.channel_binding.as_deref() else {
        return Ok(false);
    };
    let target = serde_json::from_str::<chelix_channels::ChannelReplyTarget>(binding_json)
        .map_err(ServiceError::message)?;

    let active_key = metadata
        .get_active_session(
            target.channel_type.as_str(),
            &target.account_id,
            &target.chat_id,
            target.thread_id.as_deref(),
        )
        .await
        .map_err(ServiceError::message)?
        .unwrap_or_else(|| default_channel_session_key(&target));
    Ok(active_key == entry.key)
}

async fn is_archivable_entry(
    metadata: &SqliteSessionMetadata,
    entry: &chelix_sessions::metadata::SessionEntry,
) -> Result<bool, ServiceError> {
    Ok(entry.key != "main" && !is_current_channel_session(metadata, entry).await?)
}

/// Live session service backed by JSONL store + SQLite metadata.
pub struct LiveSessionService {
    pub(super) store: Arc<SessionStore>,
    pub(super) metadata: Arc<SqliteSessionMetadata>,
    pub(super) agents_config: Arc<tokio::sync::RwLock<chelix_config::AgentsConfig>>,
    pub(super) model_service: Arc<dyn ModelService>,
    pub(super) voice_persona_store: Option<Arc<crate::voice_persona::VoicePersonaStore>>,
    pub(super) tts_service: Option<Arc<dyn TtsService>>,
    pub(super) share_store: Option<Arc<ShareStore>>,
    pub(super) sandbox_router: Arc<SandboxRouter>,
    pub(super) project_store: Option<Arc<dyn ProjectStore>>,
    pub(super) hook_registry: Option<Arc<HookRegistry>>,
    pub(super) state_store: Option<Arc<SessionStateStore>>,
    pub(super) queued_prompts: Option<Arc<QueuedPrompts>>,
    pub(super) browser_service: Option<Arc<dyn crate::services::BrowserService>>,
    pub(super) memory_manager: Option<DynMemoryRuntime>,
    pub(super) session_mutations: Arc<chelix_service_traits::SessionMutationCoordinator>,
}

impl LiveSessionService {
    pub(crate) fn from_router(
        store: Arc<SessionStore>,
        metadata: Arc<SqliteSessionMetadata>,
        sandbox_router: Arc<SandboxRouter>,
        agents_config: Arc<tokio::sync::RwLock<chelix_config::AgentsConfig>>,
        model_service: Arc<dyn ModelService>,
    ) -> Self {
        Self {
            store,
            metadata,
            agents_config,
            model_service,
            voice_persona_store: None,
            tts_service: None,
            share_store: None,
            sandbox_router,
            project_store: None,
            hook_registry: None,
            state_store: None,
            queued_prompts: None,
            browser_service: None,
            memory_manager: None,
            session_mutations: Arc::new(
                chelix_service_traits::SessionMutationCoordinator::default(),
            ),
        }
    }

    #[cfg(not(test))]
    pub fn new(
        store: Arc<SessionStore>,
        metadata: Arc<SqliteSessionMetadata>,
        sandbox_router: Arc<SandboxRouter>,
        agents_config: Arc<tokio::sync::RwLock<chelix_config::AgentsConfig>>,
        model_service: Arc<dyn ModelService>,
    ) -> Self {
        Self::from_router(
            store,
            metadata,
            sandbox_router,
            agents_config,
            model_service,
        )
    }

    #[cfg(test)]
    pub fn new(store: Arc<SessionStore>, metadata: Arc<SqliteSessionMetadata>) -> Self {
        let mut agents = chelix_config::AgentsConfig {
            default: "main".to_string(),
            ..Default::default()
        };
        agents.entries.insert(
            "main".to_string(),
            chelix_config::AgentConfig::new(
                "Chelix",
                "test::model",
                chelix_config::schema::ReasoningEffort::from("off"),
            ),
        );
        Self::from_router(
            store,
            metadata,
            Arc::new(SandboxRouter::disabled()),
            Arc::new(tokio::sync::RwLock::new(agents)),
            Arc::new(crate::services::NoopModelService),
        )
    }

    pub fn with_agents_config(
        mut self,
        agents_config: Arc<tokio::sync::RwLock<chelix_config::AgentsConfig>>,
    ) -> Self {
        self.agents_config = agents_config;
        self
    }

    pub fn with_session_mutations(
        mut self,
        session_mutations: Arc<chelix_service_traits::SessionMutationCoordinator>,
    ) -> Self {
        self.session_mutations = session_mutations;
        self
    }

    #[cfg(test)]
    pub fn with_model_service(mut self, model_service: Arc<dyn ModelService>) -> Self {
        self.model_service = model_service;
        self
    }

    pub fn with_voice_persona_store(
        mut self,
        store: Arc<crate::voice_persona::VoicePersonaStore>,
    ) -> Self {
        self.voice_persona_store = Some(store);
        self
    }

    pub fn with_tts_service(mut self, tts: Arc<dyn TtsService>) -> Self {
        self.tts_service = Some(tts);
        self
    }

    pub fn with_share_store(mut self, store: Arc<ShareStore>) -> Self {
        self.share_store = Some(store);
        self
    }

    pub fn with_project_store(mut self, store: Arc<dyn ProjectStore>) -> Self {
        self.project_store = Some(store);
        self
    }

    pub fn with_hooks(mut self, registry: Arc<HookRegistry>) -> Self {
        self.hook_registry = Some(registry);
        self
    }

    pub fn with_state_store(mut self, store: Arc<SessionStateStore>) -> Self {
        self.state_store = Some(store);
        self
    }

    /// Wire queued prompts so deleting a session also clears its pending input.
    #[must_use]
    pub fn with_queued_prompts(mut self, queued_prompts: Arc<QueuedPrompts>) -> Self {
        self.queued_prompts = Some(queued_prompts);
        self
    }

    pub fn with_browser_service(
        mut self,
        browser: Arc<dyn crate::services::BrowserService>,
    ) -> Self {
        self.browser_service = Some(browser);
        self
    }

    pub fn with_memory_manager(mut self, manager: DynMemoryRuntime) -> Self {
        self.memory_manager = Some(manager);
        self
    }

    pub(super) async fn default_agent_id(&self) -> Result<String, ServiceError> {
        let guard = self.agents_config.read().await;
        if guard.default.trim().is_empty() {
            return Err(ServiceError::message("agents.default is not configured"));
        }
        if !guard.entries.contains_key(&guard.default) {
            return Err(ServiceError::message(format!(
                "default agent '{}' not found",
                guard.default
            )));
        }
        Ok(guard.default.clone())
    }

    /// Validate that assigning `parent_key` as the parent of `key` is legal:
    /// the parent must exist, must not be the session itself, and the
    /// assignment must not introduce a cycle in the parent chain.
    pub(super) async fn validate_parent_assignment(
        &self,
        key: &str,
        parent_key: &str,
    ) -> Result<(), ServiceError> {
        if parent_key == key {
            return Err(ServiceError::message(format!(
                "session '{key}' cannot be its own parent"
            )));
        }
        let Some(parent_entry) = self
            .metadata
            .get(parent_key)
            .await
            .map_err(ServiceError::message)?
        else {
            return Err(ServiceError::message(format!(
                "parent session '{parent_key}' not found"
            )));
        };
        // Walk up the ancestor chain from the proposed parent; if we reach
        // `key`, the assignment would create a cycle. Bounded to guard
        // against pre-existing corrupt chains.
        const MAX_ANCESTOR_DEPTH: usize = 64;
        let mut current = parent_entry.parent_session_key;
        let mut depth = 0;
        while let Some(ancestor) = current {
            if ancestor == key {
                return Err(ServiceError::message(format!(
                    "cannot set parent '{parent_key}' for session '{key}': would create a cycle"
                )));
            }
            depth += 1;
            if depth >= MAX_ANCESTOR_DEPTH {
                break;
            }
            current = self
                .metadata
                .get(&ancestor)
                .await
                .map_err(ServiceError::message)?
                .and_then(|entry| entry.parent_session_key);
        }
        Ok(())
    }

    pub(super) async fn resolve_agent_id_for_entry(
        &self,
        entry: &chelix_sessions::metadata::SessionEntry,
    ) -> Result<String, ServiceError> {
        let Some(agent_id) = entry
            .agent_id
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
        else {
            return self.default_agent_id().await;
        };

        if self
            .agents_config
            .read()
            .await
            .entries
            .contains_key(agent_id)
        {
            return Ok(agent_id.to_string());
        }

        Err(ServiceError::message(format!(
            "session '{}' references unknown agent '{agent_id}'",
            entry.key
        )))
    }

    async fn resolved_agent_pair(
        &self,
        agent_id: &str,
    ) -> Result<chelix_service_traits::ResolvedModelReasoning, ServiceError> {
        let (model, reasoning_effort) = {
            let agents = self.agents_config.read().await;
            let agent = agents.get(agent_id).ok_or_else(|| {
                ServiceError::message(format!("agent '{agent_id}' is not configured"))
            })?;
            (agent.model.clone(), agent.reasoning_effort.clone())
        };
        self.model_service
            .resolve_model_reasoning(&model, Some(&reasoning_effort))
            .await
    }

    async fn selected_agent_id(
        &self,
        inherit_from_key: Option<&str>,
    ) -> Result<String, ServiceError> {
        if let Some(parent_key) = inherit_from_key
            && let Some(parent) = self
                .metadata
                .get(parent_key)
                .await
                .map_err(ServiceError::message)?
        {
            return self.resolve_agent_id_for_entry(&parent).await;
        }
        self.default_agent_id().await
    }

    async fn ensure_agent_backed_entry(
        &self,
        entry: chelix_sessions::metadata::SessionEntry,
        agent_id: &str,
        model_reasoning: &chelix_common::ResolvedModelReasoning,
    ) -> Result<chelix_sessions::metadata::SessionEntry, ServiceError> {
        if entry
            .agent_id
            .as_deref()
            .is_some_and(|id| !id.trim().is_empty())
        {
            self.resolve_agent_id_for_entry(&entry).await?;
            return Ok(entry);
        }
        let persisted_model_reasoning = entry
            .model_reasoning()
            .cloned()
            .unwrap_or_else(|| model_reasoning.clone());
        self.metadata
            .assign_agent(&entry.key, agent_id, &persisted_model_reasoning)
            .await
            .map_err(ServiceError::message)
    }

    async fn ensure_session_entry(
        &self,
        key: &str,
        inherit_from_key: Option<&str>,
    ) -> Result<chelix_sessions::metadata::SessionEntry, ServiceError> {
        if let Some(entry) = self
            .metadata
            .get(key)
            .await
            .map_err(ServiceError::message)?
        {
            if entry
                .agent_id
                .as_deref()
                .is_some_and(|id| !id.trim().is_empty())
            {
                self.resolve_agent_id_for_entry(&entry).await?;
                return Ok(entry);
            }
            let agent_id = self.selected_agent_id(inherit_from_key).await?;
            let model_reasoning = self.resolved_agent_pair(&agent_id).await?;
            return self
                .metadata
                .assign_agent(key, &agent_id, &model_reasoning)
                .await
                .map_err(ServiceError::message);
        }

        let agent_id = self.selected_agent_id(inherit_from_key).await?;
        let model_reasoning = self.resolved_agent_pair(&agent_id).await?;
        let outcome = self
            .metadata
            .ensure_llm_session(key, None, &model_reasoning, Some(&agent_id))
            .await
            .map_err(ServiceError::message)?;
        match outcome {
            chelix_sessions::metadata::EnsureLlmSessionOutcome::Created(entry) => Ok(entry),
            chelix_sessions::metadata::EnsureLlmSessionOutcome::ExistingLlm(entry) => {
                self.ensure_agent_backed_entry(entry, &agent_id, &model_reasoning)
                    .await
            },
            chelix_sessions::metadata::EnsureLlmSessionOutcome::ExistingExternal(_) => {
                let promoted = self
                    .metadata
                    .promote_external_to_llm(key, &model_reasoning, &agent_id)
                    .await
                    .map_err(ServiceError::message)?
                    .into_entry();
                self.ensure_agent_backed_entry(promoted, &agent_id, &model_reasoning)
                    .await
            },
        }
    }
}

#[async_trait]
impl SessionService for LiveSessionService {
    async fn voice_generate(&self, params: Value) -> ServiceResult {
        self.voice_generate_impl(params).await
    }

    async fn share_create(&self, params: Value) -> ServiceResult {
        self.share_create_impl(params).await
    }

    async fn share_list(&self, params: Value) -> ServiceResult {
        self.share_list_impl(params).await
    }

    async fn share_revoke(&self, params: Value) -> ServiceResult {
        self.share_revoke_impl(params).await
    }

    async fn delete(&self, params: Value) -> ServiceResult {
        self.delete_impl(params).await
    }

    async fn truncate_tail(&self, params: Value) -> ServiceResult {
        self.truncate_tail_impl(params).await
    }

    async fn search(&self, params: Value) -> ServiceResult {
        self.search_impl(params).await
    }

    async fn fork(&self, params: Value) -> ServiceResult {
        self.fork_impl(params).await
    }

    async fn branches(&self, params: Value) -> ServiceResult {
        self.branches_impl(params).await
    }

    async fn run_detail(&self, params: Value) -> ServiceResult {
        self.run_detail_impl(params).await
    }

    async fn clear_all(&self) -> ServiceResult {
        self.clear_all_impl().await
    }

    async fn mark_seen(&self, key: &str) {
        self.mark_seen_impl(key).await;
    }

    async fn list(&self) -> ServiceResult {
        let all = self.metadata.list().await.map_err(ServiceError::message)?;

        let mut entries: Vec<Value> = Vec::with_capacity(all.len());
        for mut e in all {
            let agent_id = self.resolve_agent_id_for_entry(&e).await?;
            // Check if this session is the active one for its channel binding.
            let active_channel = is_current_channel_session(&self.metadata, &e).await?;

            // Backfill preview for sessions that have messages but no preview yet.
            if e.preview.is_none()
                && e.message_count > 0
                && let Ok(history) = self.store.read(&e.key).await
            {
                let new_preview = extract_preview(&history);
                if let Some(ref preview) = new_preview {
                    self.metadata
                        .set_preview(&e.key, Some(preview))
                        .await
                        .map_err(ServiceError::message)?;
                    e.preview = new_preview;
                }
            }

            let preview = e
                .preview
                .as_deref()
                .map(|p| truncate_preview(p, SESSION_PREVIEW_MAX_CHARS));

            let model = e.model().map(str::to_string);
            let reasoning_effort = e
                .reasoning_effort()
                .map(|effort| effort.as_str().to_string());
            let external_agent_kind = e.external_agent_kind().map(|kind| kind.as_str());
            let external_session_id = e.external_session_id().map(str::to_string);
            entries.push(serde_json::json!({
                "id": e.id,
                "key": e.key,
                "label": e.label,
                "model": model,
                "reasoningEffort": reasoning_effort,
                "createdAt": e.created_at,
                "updatedAt": e.updated_at,
                "messageCount": e.message_count,
                "lastSeenMessageCount": e.last_seen_message_count,
                "projectId": e.project_id,
                "worktree_branch": e.worktree_branch,
                "channelBinding": e.channel_binding,
                "activeChannel": active_channel,
                "parentSessionKey": e.parent_session_key,
                "forkPoint": e.fork_point,
                "mcpDisabled": e.mcp_disabled,
                "preview": preview,
                "archived": e.archived,
                "agent_id": agent_id,
                "agentId": agent_id,
                "external_agent_kind": external_agent_kind,
                "externalAgentKind": external_agent_kind,
                "externalSessionId": external_session_id,
                "version": e.version,
            }));
        }
        Ok(serde_json::json!(entries))
    }

    async fn preview(&self, params: Value) -> ServiceResult {
        let key = params
            .get("key")
            .and_then(|v| v.as_str())
            .ok_or_else(|| "missing 'key' parameter".to_string())?;
        let limit = params.get("limit").and_then(|v| v.as_u64()).unwrap_or(5) as usize;

        let messages = self.store.read(key).await.map_err(ServiceError::message)?;
        let mut messages = filter_ui_history(messages).map_err(ServiceError::message)?;
        if messages.len() > limit {
            let drop_count = messages.len() - limit;
            messages.drain(0..drop_count);
        }
        Ok(serde_json::json!({ "messages": messages }))
    }

    async fn resolve(&self, params: Value) -> ServiceResult {
        let key = params
            .get("key")
            .and_then(|v| v.as_str())
            .ok_or_else(|| "missing 'key' parameter".to_string())?;
        let include_history = params
            .get("include_history")
            .and_then(|v| v.as_bool())
            .unwrap_or(true);
        let inherit_from_key = params
            .get("inherit_agent_from")
            .and_then(|v| v.as_str())
            .filter(|value| !value.trim().is_empty());

        let entry = self.ensure_session_entry(key, inherit_from_key).await?;
        if !include_history {
            if entry.message_count == 0
                && let Some(ref hooks) = self.hook_registry
            {
                let channel = resolve_hook_channel_binding(key, Some(&entry));
                let payload = chelix_common::hooks::HookPayload::SessionStart {
                    session_key: key.to_string(),
                    channel,
                };
                if let Err(e) = hooks.dispatch(&payload).await {
                    warn!(session = %key, error = %e, "SessionStart hook failed");
                }
            }

            let model = entry.model().map(str::to_string);
            let reasoning_effort = entry
                .reasoning_effort()
                .map(|effort| effort.as_str().to_string());
            let external_agent_kind = entry.external_agent_kind().map(|kind| kind.as_str());
            let external_session_id = entry.external_session_id().map(str::to_string);
            return Ok(serde_json::json!({
                "entry": {
                    "id": entry.id,
                    "key": entry.key,
                    "label": entry.label,
                    "model": model,
                    "reasoningEffort": reasoning_effort,
                    "createdAt": entry.created_at,
                    "updatedAt": entry.updated_at,
                    "messageCount": entry.message_count,
                    "projectId": entry.project_id,
                    "archived": entry.archived,
                    "worktree_branch": entry.worktree_branch,
                    "mcpDisabled": entry.mcp_disabled,
                    "parentSessionKey": entry.parent_session_key,
                    "forkPoint": entry.fork_point,
                    "agent_id": entry.agent_id,
                    "agentId": entry.agent_id,
                    "external_agent_kind": external_agent_kind,
                    "externalAgentKind": external_agent_kind,
                    "externalSessionId": external_session_id,
                    "version": entry.version,
                },
                "history": [],
                "historyTruncated": false,
                "historyDroppedCount": 0,
            }));
        }

        let raw_history = self.store.read(key).await.map_err(ServiceError::message)?;

        // Recompute preview from combined messages every time resolve runs,
        // so sessions get the latest multi-message preview algorithm.
        if !raw_history.is_empty() {
            let new_preview = extract_preview(&raw_history);
            if new_preview.as_deref() != entry.preview.as_deref() {
                self.metadata
                    .set_preview(key, new_preview.as_deref())
                    .await
                    .map_err(ServiceError::message)?;
            }
        }

        // Dispatch SessionStart hook for newly created sessions (empty history).
        if raw_history.is_empty()
            && let Some(ref hooks) = self.hook_registry
        {
            let channel = resolve_hook_channel_binding(key, Some(&entry));
            let payload = chelix_common::hooks::HookPayload::SessionStart {
                session_key: key.to_string(),
                channel,
            };
            if let Err(e) = hooks.dispatch(&payload).await {
                warn!(session = %key, error = %e, "SessionStart hook failed");
            }
        }

        let history = filter_ui_history(raw_history).map_err(ServiceError::message)?;
        let (history, dropped_count) = trim_ui_history(history);

        let model = entry.model().map(str::to_string);
        let reasoning_effort = entry
            .reasoning_effort()
            .map(|effort| effort.as_str().to_string());
        Ok(serde_json::json!({
            "entry": {
                "id": entry.id,
                "key": entry.key,
                "label": entry.label,
                "model": model,
                "reasoningEffort": reasoning_effort,
                "createdAt": entry.created_at,
                "updatedAt": entry.updated_at,
                "messageCount": entry.message_count,
                "projectId": entry.project_id,
                "archived": entry.archived,
                "worktree_branch": entry.worktree_branch,
                "mcpDisabled": entry.mcp_disabled,
                "parentSessionKey": entry.parent_session_key,
                "forkPoint": entry.fork_point,
                "agent_id": entry.agent_id,
                "agentId": entry.agent_id,
                "version": entry.version,
            },
            "history": history,
            "historyTruncated": dropped_count > 0,
            "historyDroppedCount": dropped_count,
        }))
    }

    async fn patch(&self, params: Value) -> ServiceResult {
        let p: PatchParams = parse_params(params)?;
        let key = p.key.clone();
        let mutation_reservation = self.session_mutations.reserve_mutation(&key).await;
        let _mutation_permit = mutation_reservation
            .acquire()
            .await
            .map_err(ServiceError::message)?;

        let entry = self
            .metadata
            .get(&key)
            .await
            .map_err(ServiceError::message)?
            .ok_or_else(|| format!("session '{key}' not found"))?;
        if p.archived == Some(true) && !is_archivable_entry(&self.metadata, &entry).await? {
            return Err(ServiceError::message(format!(
                "session '{key}' cannot be archived"
            )));
        }
        if p.parent_session_key.is_some()
            && entry.prompt_profile == chelix_sessions::metadata::PromptProfile::Subagent
        {
            return Err(ServiceError::message(format!(
                "session '{key}' is a sub-agent session and cannot be reparented"
            )));
        }

        let resolved_model = if p.model.is_some() || p.reasoning_effort.is_some() {
            if p.model.is_some() && p.reasoning_effort.is_none() {
                return Err(ServiceError::message(
                    "model and reasoningEffort must be provided together",
                ));
            }
            let model = match p.model.as_ref() {
                Some(model) => model.as_deref(),
                None => entry.model(),
            }
            .ok_or_else(|| ServiceError::message("model is required"))?
            .to_string();
            let reasoning_effort = p.reasoning_effort.as_ref().and_then(Option::as_ref);
            Some(
                self.model_service
                    .resolve_model_reasoning(&model, reasoning_effort)
                    .await?,
            )
        } else {
            None
        };

        if let Some(Some(parent_key)) = p.parent_session_key.as_ref()
            && !parent_key.is_empty()
        {
            self.validate_parent_assignment(&key, parent_key).await?;
        }

        let metadata_patch = chelix_sessions::metadata::SessionMetadataPatch {
            label: p.label,
            model_reasoning: resolved_model,
            archived: p.archived,
            project_id: p
                .project_id
                .map(|value| value.filter(|project_id| !project_id.is_empty())),
            worktree_branch: p
                .worktree_branch
                .map(|value| value.filter(|branch| !branch.is_empty())),
            mcp_disabled: p.mcp_disabled,
            parent_session_key: p
                .parent_session_key
                .map(|value| value.filter(|parent| !parent.is_empty())),
        };
        let entry = self
            .metadata
            .patch_session(&key, metadata_patch)
            .await
            .map_err(ServiceError::message)?;
        let model = entry.model().map(str::to_string);
        let reasoning_effort = entry
            .reasoning_effort()
            .map(|effort| effort.as_str().to_string());
        Ok(serde_json::json!({
            "id": entry.id,
            "key": entry.key,
            "label": entry.label,
            "model": model,
            "reasoningEffort": reasoning_effort,
            "archived": entry.archived,
            "worktree_branch": entry.worktree_branch,
            "mcpDisabled": entry.mcp_disabled,
            "parentSessionKey": entry.parent_session_key,
            "forkPoint": entry.fork_point,
            "agent_id": entry.agent_id,
            "agentId": entry.agent_id,
            "version": entry.version,
        }))
    }

    async fn reset(&self, params: Value) -> ServiceResult {
        let key = params
            .get("key")
            .and_then(|v| v.as_str())
            .ok_or_else(|| "missing 'key' parameter".to_string())?;

        self.store.clear(key).await.map_err(ServiceError::message)?;
        self.metadata
            .touch(key, 0)
            .await
            .map_err(ServiceError::message)?;
        self.metadata
            .set_preview(key, None)
            .await
            .map_err(ServiceError::message)?;

        Ok(serde_json::json!({}))
    }

    async fn compact(&self, _params: Value) -> ServiceResult {
        Ok(serde_json::json!({}))
    }
}
