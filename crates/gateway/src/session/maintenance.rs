use std::collections::HashSet;

use super::*;

impl LiveSessionService {
    pub(super) async fn delete_impl(&self, params: Value) -> ServiceResult {
        let key = params
            .get("key")
            .and_then(|v| v.as_str())
            .ok_or_else(|| "missing 'key' parameter".to_string())?;

        if key == "main" {
            return Err("cannot delete the main session".into());
        }

        let force = params
            .get("force")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        let delete_order = self.collect_session_delete_order(key).await?;
        let mut reservation_keys = delete_order.clone();
        reservation_keys.sort();
        let mut reservations = Vec::with_capacity(reservation_keys.len());
        for session_key in &reservation_keys {
            reservations.push(self.session_mutations.reserve_mutation(session_key).await);
        }
        let mut mutation_permits = Vec::with_capacity(reservations.len());
        for reservation in reservations {
            mutation_permits.push(reservation.acquire().await.map_err(ServiceError::message)?);
        }

        let mut entries = Vec::with_capacity(delete_order.len());
        for session_key in &delete_order {
            let entry = self
                .metadata
                .get(session_key)
                .await
                .map_err(ServiceError::message)?
                .ok_or_else(|| {
                    ServiceError::message(format!("session '{session_key}' not found"))
                })?;
            self.preflight_session_delete(&entry, force).await?;
            entries.push(entry);
        }

        let deleted_entries = self
            .metadata
            .remove_session_tree(key, &delete_order)
            .await
            .map_err(ServiceError::message)?;
        debug_assert_eq!(deleted_entries.len(), entries.len());

        let mut cleanup_errors = Vec::new();
        for entry in &deleted_entries {
            self.cleanup_deleted_session(entry, &mut cleanup_errors)
                .await;
        }
        drop(mutation_permits);

        if cleanup_errors.is_empty() {
            Ok(serde_json::json!({ "ok": true }))
        } else {
            Err(ServiceError::message(format!(
                "session metadata deletion committed, but lifecycle cleanup was incomplete: {}",
                cleanup_errors.join("; ")
            )))
        }
    }

    pub(super) async fn truncate_tail_impl(&self, params: Value) -> ServiceResult {
        let params: TruncateTailParams = parse_params(params)?;
        let key = params.key().map_err(ServiceError::message)?.to_string();
        let ui = self
            .store
            .ui_history
            .session(&key)
            .await
            .map_err(ServiceError::message)?;
        let index = ui
            .canonical_index(&params.target)
            .await
            .map_err(ServiceError::message)?;
        let target = chelix_sessions::store::UserMessageTarget::MessageIndex(index);

        self.metadata
            .get(&key)
            .await
            .map_err(ServiceError::message)?
            .ok_or_else(|| ServiceError::message(format!("session '{key}' not found")))?;

        let truncate = self
            .store
            .truncate_from_user_message(&key, target)
            .await
            .map_err(ServiceError::message)?;
        let retained_history = self
            .store
            .ui_history
            .history(&key)
            .await
            .map_err(ServiceError::message)?;
        let preview = extract_preview(&retained_history);

        let ui_message_count = self
            .store
            .ui_message_count(&key)
            .await
            .map_err(ServiceError::message)?;
        self.metadata
            .touch(&key, ui_message_count)
            .await
            .map_err(ServiceError::message)?;
        self.metadata
            .set_preview(&key, preview.as_deref())
            .await
            .map_err(ServiceError::message)?;

        let entry = self
            .metadata
            .get(&key)
            .await
            .map_err(ServiceError::message)?
            .ok_or_else(|| format!("session '{key}' not found after truncation"))?;

        Ok(serde_json::json!({
            "ok": true,
            "sessionKey": key,
            "generation": ui.subscribe().borrow().generation,
            "totalMessages": ui_message_count,
            "prunedMediaCount": truncate.pruned_media_count,
            "preview": preview,
            "entry": session_entry_value(&entry),
        }))
    }

    async fn collect_session_delete_order(
        &self,
        root_key: &str,
    ) -> Result<Vec<String>, ServiceError> {
        let mut order = Vec::new();
        let mut seen = HashSet::new();
        let mut stack = vec![root_key.to_string()];

        while let Some(key) = stack.pop() {
            if !seen.insert(key.clone()) {
                continue;
            }
            order.push(key.clone());
            for child in self
                .metadata
                .list_children(&key)
                .await
                .map_err(ServiceError::message)?
            {
                stack.push(child.key);
            }
        }

        order.reverse();
        Ok(order)
    }

    async fn preflight_session_delete(
        &self,
        entry: &chelix_sessions::metadata::SessionEntry,
        force: bool,
    ) -> Result<(), ServiceError> {
        if force || entry.worktree_branch.is_none() {
            return Ok(());
        }
        let Some(project_id) = entry.project_id.as_deref() else {
            return Ok(());
        };
        let Some(project_store) = self.project_store.as_ref() else {
            return Ok(());
        };
        let Some(project) = project_store
            .get(project_id)
            .await
            .map_err(ServiceError::message)?
        else {
            return Ok(());
        };
        let worktree_dir = project.directory.join(".chelix-worktrees").join(&entry.key);
        if worktree_dir.exists()
            && chelix_projects::WorktreeManager::has_uncommitted_changes(&worktree_dir)
                .await
                .map_err(ServiceError::message)?
        {
            return Err(ServiceError::message(
                "worktree has uncommitted changes; use force: true to delete anyway",
            ));
        }
        Ok(())
    }

    async fn cleanup_deleted_session(
        &self,
        entry: &chelix_sessions::metadata::SessionEntry,
        errors: &mut Vec<String>,
    ) {
        let key = &entry.key;
        if entry.worktree_branch.is_some()
            && let Some(project_id) = entry.project_id.as_deref()
            && let Some(project_store) = self.project_store.as_ref()
        {
            match project_store.get(project_id).await {
                Ok(Some(project)) => {
                    let project_dir = &project.directory;
                    let worktree_dir = project_dir.join(".chelix-worktrees").join(key);
                    if let Some(command) = project.teardown_command.as_deref()
                        && worktree_dir.exists()
                        && let Err(error) = chelix_projects::WorktreeManager::run_teardown(
                            &worktree_dir,
                            command,
                            project_dir,
                            key,
                        )
                        .await
                    {
                        errors.push(format!("session '{key}' worktree teardown: {error}"));
                    }
                    if let Err(error) =
                        chelix_projects::WorktreeManager::cleanup(project_dir, key).await
                    {
                        errors.push(format!("session '{key}' worktree cleanup: {error}"));
                    }
                },
                Ok(None) => {},
                Err(error) => {
                    errors.push(format!("session '{key}' project lookup: {error}"));
                },
            }
        }

        if let Err(error) = self.store.clear(key).await {
            errors.push(format!("session '{key}' history cleanup: {error}"));
        }
        let owner_key = entry.sandbox_owner_key.as_deref().unwrap_or(key);
        if key == owner_key
            && let Err(error) = self.sandbox_router.cleanup_owner_sandbox(owner_key).await
        {
            errors.push(format!("session '{key}' sandbox cleanup: {error}"));
        }
        if let Some(state_store) = self.state_store.as_ref()
            && let Err(error) = state_store.delete_session(key).await
        {
            errors.push(format!("session '{key}' state cleanup: {error}"));
        }
        if let Some(queued_prompts) = self.queued_prompts.as_ref()
            && let Err(error) = queued_prompts.clear(SessionKey::new(key.clone())).await
        {
            errors.push(format!("session '{key}' prompt queue cleanup: {error}"));
        }
        if let Err(error) = self.cleanup_session_memory_exports(key).await {
            errors.push(format!("session '{key}' memory export cleanup: {error}"));
        }

        if let Some(hooks) = self.hook_registry.as_ref() {
            let payload = chelix_common::hooks::HookPayload::SessionEnd {
                session_key: key.to_string(),
            };
            if let Err(error) = hooks.dispatch(&payload).await {
                warn!(session = %key, %error, "SessionEnd hook failed");
            }
        }
    }

    async fn cleanup_session_memory_exports(&self, key: &str) -> Result<(), anyhow::Error> {
        let Some(manager) = self.memory_manager.as_ref() else {
            return Ok(());
        };
        let Some(data_dir) = manager.data_dir().map(Path::to_path_buf) else {
            return Ok(());
        };

        let memory_dir = data_dir.join("memory");
        let markers = session_memory_markers(key);
        let mut dirs = vec![memory_dir];

        while let Some(dir) = dirs.pop() {
            let mut entries = match tokio::fs::read_dir(&dir).await {
                Ok(entries) => entries,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(error) => {
                    tracing::warn!(path = %dir.display(), %error, "failed to scan memory directory");
                    continue;
                },
            };

            while let Some(entry) = entries.next_entry().await? {
                let path = entry.path();
                let file_type = match entry.file_type().await {
                    Ok(file_type) => file_type,
                    Err(error) => {
                        tracing::warn!(path = %path.display(), %error, "failed to inspect memory entry");
                        continue;
                    },
                };

                if file_type.is_dir() {
                    dirs.push(path);
                    continue;
                }
                if !file_type.is_file() || !is_session_memory_export_candidate(&path) {
                    continue;
                }

                let content = match tokio::fs::read_to_string(&path).await {
                    Ok(content) => content,
                    Err(error) => {
                        tracing::warn!(path = %path.display(), %error, "failed to read memory export candidate");
                        continue;
                    },
                };
                if !markers.iter().any(|marker| content.contains(marker)) {
                    continue;
                }

                if let Err(error) = manager.remove_path(&path).await {
                    tracing::warn!(path = %path.display(), %error, "failed to remove memory export from index");
                }
                match tokio::fs::remove_file(&path).await {
                    Ok(()) => {},
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {},
                    Err(error) => {
                        tracing::warn!(path = %path.display(), %error, "failed to delete memory export file");
                    },
                }
            }
        }

        Ok(())
    }

    pub(super) async fn fork_impl(&self, params: Value) -> ServiceResult {
        let params: crate::session_types::ForkParams = parse_params(params)?;
        let parent_key = params.key.trim();
        if parent_key.is_empty() || (params.target.is_some() && params.fork_point.is_some()) {
            return Err(ServiceError::message(
                "fork requires a session key and at most one boundary",
            ));
        }
        let label = params.label;
        let reservation = self.session_mutations.reserve_mutation(parent_key).await;
        let _permit = reservation.acquire().await.map_err(ServiceError::message)?;

        let source = self
            .store
            .ui_history
            .session(parent_key)
            .await
            .map_err(ServiceError::message)?;
        let requested_point = if let Some(target) = params.target {
            source
                .canonical_index(&target)
                .await
                .map_err(ServiceError::message)?;
            let page = source
                .page(
                    chelix_sessions::ui_history_types::UiHistoryRange::Around {
                        message_id: target.message_id.clone(),
                    },
                    1,
                )
                .await
                .map_err(ServiceError::message)?;
            Some(
                page.history
                    .first()
                    .ok_or_else(|| ServiceError::message("fork target disappeared"))?
                    .position
                    + 1,
            )
        } else {
            params.fork_point
        };

        let parent = self
            .metadata
            .get(parent_key)
            .await
            .map_err(ServiceError::message)?
            .ok_or_else(|| ServiceError::message(format!("session '{parent_key}' not found")))?;
        let parent_agent = self.resolve_agent_id_for_entry(&parent).await?;
        let model_reasoning = parent.model_reasoning().cloned().ok_or_else(|| {
            ServiceError::message(format!(
                "session '{parent_key}' has no LLM model/reasoning pair"
            ))
        })?;

        let new_key = format!("session:{}", uuid::Uuid::new_v4());
        let fork = self
            .store
            .fork_history(parent_key, &new_key, requested_point)
            .await
            .map_err(ServiceError::message)?;

        self.metadata
            .create_llm_session(
                &new_key,
                label.as_deref(),
                &model_reasoning,
                Some(&parent_agent),
            )
            .await
            .map_err(ServiceError::message)?;

        let ui_message_count = self
            .store
            .ui_message_count(&new_key)
            .await
            .map_err(ServiceError::message)?;
        self.metadata
            .touch(&new_key, ui_message_count)
            .await
            .map_err(ServiceError::message)?;

        if let Some(project_id) = parent.project_id.as_deref() {
            self.metadata
                .set_project_id(&new_key, Some(project_id))
                .await
                .map_err(ServiceError::message)?;
        }
        if parent.mcp_disabled.is_some() {
            self.metadata
                .set_mcp_disabled(&new_key, parent.mcp_disabled)
                .await
                .map_err(ServiceError::message)?;
        }

        self.metadata
            .set_parent(&new_key, Some(parent_key), Some(fork.fork_point))
            .await
            .map_err(ServiceError::message)?;

        // Re-fetch after all mutations to get the final version.
        let final_entry = self
            .metadata
            .get(&new_key)
            .await
            .map_err(ServiceError::message)?
            .ok_or_else(|| format!("forked session '{new_key}' not found after creation"))?;
        Ok(serde_json::json!({
            "sessionKey": new_key,
            "id": final_entry.id,
            "label": final_entry.label,
            "forkPoint": fork.fork_point,
            "sourceEnd": fork.source_end,
            "boundaryAdjusted": fork.boundary_adjusted,
            "boundaryReasons": fork.boundary_reasons,
            "messageCount": ui_message_count,
            "agent_id": final_entry.agent_id,
            "agentId": final_entry.agent_id,
            "version": final_entry.version,
        }))
    }

    pub(super) async fn branches_impl(&self, params: Value) -> ServiceResult {
        let key = params
            .get("key")
            .and_then(|v| v.as_str())
            .ok_or_else(|| "missing 'key' parameter".to_string())?;

        let children = self
            .metadata
            .list_children(key)
            .await
            .map_err(ServiceError::message)?;
        let items: Vec<Value> = children
            .into_iter()
            .map(|e| {
                serde_json::json!({
                    "key": e.key,
                    "label": e.label,
                    "forkPoint": e.fork_point,
                    "messageCount": e.message_count,
                    "createdAt": e.created_at,
                })
            })
            .collect();
        Ok(serde_json::json!(items))
    }

    pub(super) async fn search_impl(&self, params: Value) -> ServiceResult {
        let query = params
            .get("query")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim();

        if query.is_empty() {
            return Ok(serde_json::json!([]));
        }

        let max = params.get("limit").and_then(|v| v.as_u64()).unwrap_or(20) as usize;
        let include_archived = params
            .get("includeArchived")
            .and_then(|v| v.as_bool())
            .or_else(|| params.get("include_archived").and_then(|v| v.as_bool()))
            .unwrap_or(false);
        let entries = self.metadata.list().await.map_err(ServiceError::message)?;
        let keys = entries
            .iter()
            .filter(|entry| include_archived || !entry.archived)
            .map(|entry| entry.key.clone())
            .collect::<Vec<_>>();
        let results = self
            .store
            .search(&keys, query, max)
            .await
            .map_err(ServiceError::message)?;
        let enriched = results
            .into_iter()
            .map(|hit| {
                let entry = entries.iter().find(|entry| entry.key == hit.session_key);
                serde_json::json!({
                    "sessionKey": hit.session_key,
                    "snippet": hit.snippet,
                    "role": hit.role,
                    "messageId": hit.message_id,
                    "generation": hit.generation,
                    "position": hit.position,
                    "label": entry.and_then(|entry| entry.label.as_ref()),
                    "archived": entry.is_some_and(|entry| entry.archived),
                })
            })
            .collect::<Vec<_>>();

        Ok(serde_json::json!(enriched))
    }

    pub(super) async fn mark_seen_impl(&self, key: &str) {
        if let Err(error) = self.metadata.mark_seen(key).await {
            tracing::error!(session = %key, %error, "failed to mark session as seen");
        }
    }

    pub(super) async fn clear_all_impl(&self) -> ServiceResult {
        let all = self.metadata.list().await.map_err(ServiceError::message)?;
        let mut deleted = 0u32;
        let mut failures = Vec::new();

        for entry in &all {
            // Keep main, channel-bound (telegram) and cron sessions.
            if entry.key == "main"
                || entry.channel_binding.is_some()
                || entry.key.starts_with("telegram:")
                || entry.key.starts_with("cron:")
            {
                continue;
            }
            if self
                .metadata
                .get(&entry.key)
                .await
                .map_err(ServiceError::message)?
                .is_none()
            {
                continue;
            }

            // Reuse delete logic via params.
            let params = serde_json::json!({ "key": entry.key, "force": true });
            if let Err(error) = self.delete_impl(params).await {
                warn!(session = %entry.key, %error, "clear_all: failed to delete session");
                failures.push(format!("session '{}': {error}", entry.key));
                continue;
            }
            deleted += 1;
        }

        // Close all browser containers since all user sessions are being cleared.
        if let Some(ref browser) = self.browser_service {
            info!("closing all browser sessions after clear_all");
            browser.close_all().await;
        }

        if failures.is_empty() {
            Ok(serde_json::json!({ "deleted": deleted }))
        } else {
            Err(ServiceError::message(format!(
                "clear_all deleted {deleted} sessions, but {} deletions failed: {}",
                failures.len(),
                failures.join("; ")
            )))
        }
    }

    pub(super) async fn run_detail_impl(&self, params: Value) -> ServiceResult {
        let session_key = params
            .get("sessionKey")
            .and_then(|v| v.as_str())
            .ok_or_else(|| "missing 'sessionKey' parameter".to_string())?;
        let run_id = params
            .get("runId")
            .and_then(|v| v.as_str())
            .ok_or_else(|| "missing 'runId' parameter".to_string())?;

        let messages = self
            .store
            .read_by_run_id(session_key, run_id)
            .await
            .map_err(|e| e.to_string())?;

        // Build summary counts.
        let mut user_messages = 0u32;
        let mut tool_calls = 0u32;
        let mut assistant_messages = 0u32;

        for msg in &messages {
            match msg.get("role").and_then(|v| v.as_str()) {
                Some("user") => user_messages += 1,
                Some("assistant") => assistant_messages += 1,
                Some("tool_lifecycle") => {
                    let lifecycle = serde_json::from_value::<ToolLifecycleEvent>(msg.clone())
                        .map_err(|error| {
                            format!("invalid tool lifecycle in run history: {error}")
                        })?;
                    if lifecycle.stage().is_terminal() {
                        tool_calls += 1;
                    }
                },
                _ => {},
            }
        }

        Ok(serde_json::json!({
            "runId": run_id,
            "messages": messages,
            "summary": {
                "userMessages": user_messages,
                "toolCalls": tool_calls,
                "assistantMessages": assistant_messages,
            }
        }))
    }
}

fn session_memory_markers(key: &str) -> Vec<String> {
    vec![
        format!("- **Session**: {key}"),
        format!("session_id: {key}"),
    ]
}

fn is_session_memory_export_candidate(path: &Path) -> bool {
    path.extension().and_then(|extension| extension.to_str()) == Some("md")
        && path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.starts_with("session-"))
}
