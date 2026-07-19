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
) -> bool {
    let Some(binding_json) = entry.channel_binding.as_deref() else {
        return false;
    };
    let Ok(target) = serde_json::from_str::<chelix_channels::ChannelReplyTarget>(binding_json)
    else {
        return false;
    };

    let active_key = metadata
        .get_active_session(
            target.channel_type.as_str(),
            &target.account_id,
            &target.chat_id,
            target.thread_id.as_deref(),
        )
        .await
        .unwrap_or_else(|| default_channel_session_key(&target));
    active_key == entry.key
}

async fn is_archivable_entry(
    metadata: &SqliteSessionMetadata,
    entry: &chelix_sessions::metadata::SessionEntry,
) -> bool {
    entry.key != "main" && !is_current_channel_session(metadata, entry).await
}

/// Live session service backed by JSONL store + SQLite metadata.
pub struct LiveSessionService {
    pub(super) store: Arc<SessionStore>,
    pub(super) metadata: Arc<SqliteSessionMetadata>,
    pub(super) agent_persona_store: Option<Arc<AgentPersonaStore>>,
    pub(super) voice_persona_store: Option<Arc<crate::voice_persona::VoicePersonaStore>>,
    pub(super) tts_service: Option<Arc<dyn TtsService>>,
    pub(super) share_store: Option<Arc<ShareStore>>,
    pub(super) sandbox_router: Arc<SandboxRouter>,
    pub(super) project_store: Option<Arc<dyn ProjectStore>>,
    pub(super) hook_registry: Option<Arc<HookRegistry>>,
    pub(super) state_store: Option<Arc<SessionStateStore>>,
    pub(super) browser_service: Option<Arc<dyn crate::services::BrowserService>>,
    pub(super) memory_manager: Option<DynMemoryRuntime>,
    #[cfg(feature = "fs-tools")]
    pub(super) fs_state: Option<FsState>,
}

impl LiveSessionService {
    pub(crate) fn from_router(
        store: Arc<SessionStore>,
        metadata: Arc<SqliteSessionMetadata>,
        sandbox_router: Arc<SandboxRouter>,
    ) -> Self {
        Self {
            store,
            metadata,
            agent_persona_store: None,
            voice_persona_store: None,
            tts_service: None,
            share_store: None,
            sandbox_router,
            project_store: None,
            hook_registry: None,
            state_store: None,
            browser_service: None,
            memory_manager: None,
            #[cfg(feature = "fs-tools")]
            fs_state: None,
        }
    }

    #[cfg(not(test))]
    pub fn new(
        store: Arc<SessionStore>,
        metadata: Arc<SqliteSessionMetadata>,
        sandbox_router: Arc<SandboxRouter>,
    ) -> Self {
        Self::from_router(store, metadata, sandbox_router)
    }

    #[cfg(test)]
    pub fn new(store: Arc<SessionStore>, metadata: Arc<SqliteSessionMetadata>) -> Self {
        Self::from_router(store, metadata, Arc::new(SandboxRouter::disabled()))
    }

    pub fn with_agent_persona_store(mut self, store: Arc<AgentPersonaStore>) -> Self {
        self.agent_persona_store = Some(store);
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

    #[cfg(feature = "fs-tools")]
    pub fn with_fs_state(mut self, fs_state: FsState) -> Self {
        self.fs_state = Some(fs_state);
        self
    }

    pub(super) async fn default_agent_id(&self) -> String {
        if let Some(ref store) = self.agent_persona_store {
            return store
                .default_id()
                .await
                .unwrap_or_else(|_| "main".to_string());
        }
        "main".to_string()
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
        let Some(parent_entry) = self.metadata.get(parent_key).await else {
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
                .and_then(|e| e.parent_session_key);
        }
        Ok(())
    }

    pub(super) async fn resolve_agent_id_for_entry(
        &self,
        entry: &chelix_sessions::metadata::SessionEntry,
        patch_if_invalid: bool,
    ) -> String {
        let fallback = self.default_agent_id().await;
        let Some(agent_id) = entry
            .agent_id
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
        else {
            return fallback;
        };

        if let Some(ref store) = self.agent_persona_store {
            match store.get(agent_id).await {
                Ok(Some(_)) => {
                    return agent_id.to_string();
                },
                Ok(None) => {
                    warn!(
                        session = %entry.key,
                        agent_id,
                        fallback = %fallback,
                        "session references unknown agent, falling back to default"
                    );
                },
                Err(error) => {
                    warn!(
                        session = %entry.key,
                        agent_id,
                        fallback = %fallback,
                        %error,
                        "failed to resolve session agent, falling back to default"
                    );
                },
            }
        } else {
            return agent_id.to_string();
        }

        if patch_if_invalid {
            let _ = self
                .metadata
                .set_agent_id(&entry.key, Some(&fallback))
                .await;
        }
        fallback
    }

    async fn ensure_entry_agent_id(
        &self,
        key: &str,
        inherit_from_key: Option<&str>,
    ) -> Option<chelix_sessions::metadata::SessionEntry> {
        let entry = self.metadata.get(key).await?;
        if entry
            .agent_id
            .as_deref()
            .is_some_and(|id| !id.trim().is_empty())
        {
            let effective = self.resolve_agent_id_for_entry(&entry, true).await;
            if entry.agent_id.as_deref() == Some(effective.as_str()) {
                return Some(entry);
            }
            let mut updated = entry;
            updated.agent_id = Some(effective);
            return Some(updated);
        }

        let fallback = if let Some(parent_key) = inherit_from_key {
            if let Some(parent) = self.metadata.get(parent_key).await {
                self.resolve_agent_id_for_entry(&parent, false).await
            } else {
                self.default_agent_id().await
            }
        } else {
            self.default_agent_id().await
        };

        let _ = self.metadata.set_agent_id(key, Some(&fallback)).await;
        self.metadata.get(key).await
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
        let all = self.metadata.list().await;

        let mut entries: Vec<Value> = Vec::with_capacity(all.len());
        for mut e in all {
            let agent_id = self.resolve_agent_id_for_entry(&e, false).await;
            // Check if this session is the active one for its channel binding.
            let active_channel = is_current_channel_session(&self.metadata, &e).await;

            // Backfill preview for sessions that have messages but no preview yet.
            if e.preview.is_none()
                && e.message_count > 0
                && let Ok(history) = self.store.read(&e.key).await
            {
                let new_preview = extract_preview(&history);
                if let Some(ref preview) = new_preview {
                    self.metadata.set_preview(&e.key, Some(preview)).await;
                    e.preview = new_preview;
                }
            }

            let preview = e
                .preview
                .as_deref()
                .map(|p| truncate_preview(p, SESSION_PREVIEW_MAX_CHARS));

            entries.push(serde_json::json!({
                "id": e.id,
                "key": e.key,
                "label": e.label,
                "model": e.model,
                "reasoningEffort": e.reasoning_effort,
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
                "mode_id": e.mode_id,
                "modeId": e.mode_id,
                "node_id": e.node_id,
                "external_agent_kind": e.external_agent_kind.map(|kind| kind.as_str()),
                "externalAgentKind": e.external_agent_kind.map(|kind| kind.as_str()),
                "externalSessionId": e.external_session_id,
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

        let messages = self
            .store
            .read_last_n(key, limit)
            .await
            .map_err(ServiceError::message)?;
        Ok(serde_json::json!({ "messages": filter_ui_history(messages) }))
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

        self.metadata
            .upsert(key, None)
            .await
            .map_err(ServiceError::message)?;
        let entry = self
            .ensure_entry_agent_id(key, inherit_from_key)
            .await
            .ok_or_else(|| format!("session '{key}' not found after resolve"))?;
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

            return Ok(serde_json::json!({
                "entry": {
                    "id": entry.id,
                    "key": entry.key,
                    "label": entry.label,
                    "model": entry.model,
                    "reasoningEffort": entry.reasoning_effort,
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
                    "mode_id": entry.mode_id,
                    "modeId": entry.mode_id,
                    "node_id": entry.node_id,
                    "external_agent_kind": entry.external_agent_kind.map(|kind| kind.as_str()),
                    "externalAgentKind": entry.external_agent_kind.map(|kind| kind.as_str()),
                    "externalSessionId": entry.external_session_id,
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
                self.metadata.set_preview(key, new_preview.as_deref()).await;
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

        let (history, dropped_count) = trim_ui_history(filter_ui_history(raw_history));

        Ok(serde_json::json!({
            "entry": {
                "id": entry.id,
                "key": entry.key,
                "label": entry.label,
                "model": entry.model,
                "reasoningEffort": entry.reasoning_effort,
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
                "mode_id": entry.mode_id,
                "modeId": entry.mode_id,
                "node_id": entry.node_id,
                "version": entry.version,
            },
            "history": history,
            "historyTruncated": dropped_count > 0,
            "historyDroppedCount": dropped_count,
        }))
    }

    async fn patch(&self, params: Value) -> ServiceResult {
        let p: PatchParams = parse_params(params)?;
        let key = &p.key;

        let entry = self
            .metadata
            .get(key)
            .await
            .ok_or_else(|| format!("session '{key}' not found"))?;
        if p.archived == Some(true) && !is_archivable_entry(&self.metadata, &entry).await {
            return Err(ServiceError::message(format!(
                "session '{key}' cannot be archived"
            )));
        }
        if p.label.is_some() {
            let _ = self.metadata.upsert(key, p.label).await;
        }
        if p.model.is_some() {
            self.metadata.set_model(key, p.model).await;
        }
        if p.reasoning_effort.is_some() {
            self.metadata
                .set_reasoning_effort(key, p.reasoning_effort)
                .await;
        }
        if let Some(archived) = p.archived {
            self.metadata.set_archived(key, archived).await;
        }
        if let Some(project_id_opt) = p.project_id {
            let project_id = project_id_opt.filter(|s| !s.is_empty());
            self.metadata.set_project_id(key, project_id).await;
        }
        if let Some(worktree_branch_opt) = p.worktree_branch {
            let worktree_branch = worktree_branch_opt.filter(|s| !s.is_empty());
            self.metadata
                .set_worktree_branch(key, worktree_branch)
                .await;
        }
        if let Some(mode_id_opt) = p.mode_id {
            let mode_id = mode_id_opt.filter(|s| !s.is_empty());
            self.metadata
                .set_mode_id(key, mode_id.as_deref())
                .await
                .map_err(|e| ServiceError::message(e.to_string()))?;
        }
        if let Some(mcp_disabled) = p.mcp_disabled {
            self.metadata.set_mcp_disabled(key, mcp_disabled).await;
        }
        if let Some(parent_opt) = p.parent_session_key {
            let parent = parent_opt.filter(|s| !s.is_empty());
            if let Some(ref parent_key) = parent {
                self.validate_parent_assignment(key, parent_key).await?;
            }
            // Changing the parent invalidates any fork point recorded for the
            // previous relationship.
            self.metadata.set_parent(key, parent, None).await;
        }

        let entry = self
            .metadata
            .get(key)
            .await
            .ok_or_else(|| format!("session '{key}' not found after update"))?;
        Ok(serde_json::json!({
            "id": entry.id,
            "key": entry.key,
            "label": entry.label,
            "model": entry.model,
            "reasoningEffort": entry.reasoning_effort,
            "archived": entry.archived,
            "worktree_branch": entry.worktree_branch,
            "mcpDisabled": entry.mcp_disabled,
            "parentSessionKey": entry.parent_session_key,
            "forkPoint": entry.fork_point,
            "agent_id": entry.agent_id,
            "agentId": entry.agent_id,
            "mode_id": entry.mode_id,
            "modeId": entry.mode_id,
            "node_id": entry.node_id,
            "version": entry.version,
        }))
    }

    async fn reset(&self, params: Value) -> ServiceResult {
        let key = params
            .get("key")
            .and_then(|v| v.as_str())
            .ok_or_else(|| "missing 'key' parameter".to_string())?;

        self.store.clear(key).await.map_err(ServiceError::message)?;
        self.metadata.touch(key, 0).await;
        self.metadata.set_preview(key, None).await;

        Ok(serde_json::json!({}))
    }

    async fn compact(&self, _params: Value) -> ServiceResult {
        Ok(serde_json::json!({}))
    }
}
