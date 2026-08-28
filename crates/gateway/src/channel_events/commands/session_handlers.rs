use std::sync::Arc;

use tracing::info;

use {
    chelix_channels::{ChannelReplyTarget, Error as ChannelError, Result as ChannelResult},
    chelix_sessions::metadata::SqliteSessionMetadata,
};

use crate::{
    broadcast::{BroadcastOpts, broadcast},
    state::GatewayState,
};

use super::super::{
    format_attachable_sessions_list, format_channel_sessions_list, is_attachable_session,
    parse_numbered_selection, resolve_channel_agent_id, resolve_channel_session_defaults,
    session_list_label,
};

// ── Session management command handlers ──────────────────────────

fn require_channel_model_id(model_id: Option<String>) -> ChannelResult<String> {
    let model_id = model_id
        .ok_or_else(|| ChannelError::invalid_input("channel session model is not configured"))?;
    if model_id.trim().is_empty() {
        return Err(ChannelError::invalid_input(
            "channel session model is not configured",
        ));
    }
    Ok(model_id)
}

pub(in crate::channel_events) async fn handle_new(
    state: &Arc<GatewayState>,
    session_metadata: &SqliteSessionMetadata,
    session_key: &str,
    reply_to: &ChannelReplyTarget,
    sender_id: Option<&str>,
) -> ChannelResult<String> {
    let old_entry = session_metadata.get(session_key).await;
    let channel_defaults = resolve_channel_session_defaults(state, reply_to, sender_id).await;
    let inherited_agent = old_entry
        .as_ref()
        .and_then(|entry| entry.agent_id.as_deref())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string);
    let requested_agent = inherited_agent
        .as_deref()
        .or(channel_defaults.agent_id.as_deref());
    let target_agent = resolve_channel_agent_id(state, session_key, requested_agent).await?;
    let (agent_model, _) =
        crate::session_reasoning::agent_defaults_for_agent(state, Some(&target_agent)).await;
    let model_id = require_channel_model_id(channel_defaults.model.or(agent_model))?;

    // Create a new session with a fresh UUID key.
    let new_key = format!("session:{}", uuid::Uuid::new_v4());
    let binding_json = serde_json::to_string(reply_to)
        .map_err(|e| ChannelError::external("serialize channel binding", e))?;

    // Sequential label: count existing sessions for this chat.
    let existing = session_metadata
        .list_channel_sessions(
            reply_to.channel_type.as_str(),
            &reply_to.account_id,
            &reply_to.chat_id,
        )
        .await;
    let n = existing.len() + 1;

    // Create the new session entry with channel binding.
    session_metadata
        .upsert(
            &new_key,
            Some(format!("{} {n}", reply_to.channel_type.display_name())),
        )
        .await
        .map_err(|e| ChannelError::external("create channel session", e))?;
    session_metadata
        .set_channel_binding(&new_key, Some(binding_json.clone()))
        .await;
    session_metadata
        .set_agent_id(&new_key, Some(&target_agent))
        .await
        .map_err(|e| ChannelError::external("setting session agent", e))?;

    // Validate and persist the complete pair before making the new session active.
    if let Err(patch_error) =
        super::super::patch_channel_session_model(state, &new_key, &model_id).await
    {
        let cleanup = state
            .services
            .session
            .delete(serde_json::json!({ "key": &new_key, "force": true }))
            .await;
        if let Err(cleanup_error) = cleanup {
            tracing::error!(
                session = %new_key,
                error = %patch_error,
                cleanup_error = %cleanup_error,
                "channel /new model validation and rollback failed"
            );
            return Err(ChannelError::unavailable(format!(
                "model validation failed: {patch_error}; session rollback failed: {cleanup_error}"
            )));
        }
        tracing::error!(session = %new_key, error = %patch_error, "channel /new model validation failed");
        return Err(patch_error);
    }

    // Ensure the old session also has a channel binding (for listing).
    if old_entry
        .as_ref()
        .and_then(|entry| entry.channel_binding.as_ref())
        .is_none()
    {
        session_metadata
            .set_channel_binding(session_key, Some(binding_json))
            .await;
    }

    // Update the forward mapping only after the new session is valid.
    session_metadata
        .set_active_session(
            reply_to.channel_type.as_str(),
            &reply_to.account_id,
            &reply_to.chat_id,
            reply_to.thread_id.as_deref(),
            &new_key,
        )
        .await;

    info!(
        old_session = %session_key,
        new_session = %new_key,
        "channel /new: created new session"
    );

    // Export the old session after the active pointer has changed. The hook
    // reads history by session_key directly; export failures remain logged by
    // the hook path and the old session remains available for manual export.
    let hooks = state.inner.read().await.hook_registry.clone();
    if let Some(ref hooks) = hooks {
        crate::session::dispatch_command_hook(hooks, session_key, "new", sender_id).await;
    }

    // Notify web UI so the session list refreshes.
    broadcast(
        state,
        "session",
        serde_json::json!({
            "kind": "created",
            "sessionKey": &new_key,
        }),
        BroadcastOpts {
            drop_if_slow: true,
            ..Default::default()
        },
    )
    .await;

    Ok(format!(
        "New session started. Using *{model_id}*. Use /model to change."
    ))
}

pub(in crate::channel_events) async fn handle_title(
    state: &Arc<GatewayState>,
    session_key: &str,
) -> ChannelResult<String> {
    let generated = crate::session::title::generate_title_for_session(state, session_key)
        .await
        .map_err(ChannelError::unavailable)?;
    let label = if let Some(label) = generated {
        label
    } else if let Some(ref meta) = state.services.session_metadata {
        meta.get(session_key)
            .await
            .and_then(|e| e.label)
            .unwrap_or_else(|| "untitled".to_string())
    } else {
        "untitled".to_string()
    };
    Ok(format!("Title: {label}"))
}

pub(in crate::channel_events) async fn handle_fork(
    state: &Arc<GatewayState>,
    session_key: &str,
    args: &str,
) -> ChannelResult<String> {
    let label = if args.is_empty() {
        None
    } else {
        Some(args.trim())
    };

    let mut params = serde_json::json!({ "key": session_key });
    if let Some(l) = label {
        params["label"] = serde_json::json!(l);
    }

    let res = state
        .services
        .session
        .fork(params)
        .await
        .map_err(ChannelError::unavailable)?;

    let new_key = res
        .get("sessionKey")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown");
    let fork_point = res.get("forkPoint").and_then(|v| v.as_u64()).unwrap_or(0);

    broadcast(
        state,
        "session",
        serde_json::json!({
            "kind": "created",
            "sessionKey": new_key,
        }),
        BroadcastOpts {
            drop_if_slow: true,
            ..Default::default()
        },
    )
    .await;

    let label_str = res.get("label").and_then(|v| v.as_str()).unwrap_or(new_key);
    Ok(format!(
        "Forked at message {fork_point} into: {label_str}\nUse /sessions to switch."
    ))
}

pub(in crate::channel_events) async fn handle_clear(
    state: &Arc<GatewayState>,
    session_key: &str,
) -> ChannelResult<String> {
    let chat = state.chat();
    let params = serde_json::json!({ "_session_key": session_key });
    chat.clear(params)
        .await
        .map_err(ChannelError::unavailable)?;
    Ok("Session cleared.".to_string())
}

pub(in crate::channel_events) async fn handle_compact(
    state: &Arc<GatewayState>,
    session_key: &str,
) -> ChannelResult<String> {
    let chat = state.chat();
    let params = serde_json::json!({ "_session_key": session_key });
    chat.compact(params)
        .await
        .map_err(ChannelError::unavailable)?;
    Ok("Session compacted.".to_string())
}

pub(in crate::channel_events) async fn handle_context(
    state: &Arc<GatewayState>,
    session_key: &str,
) -> ChannelResult<String> {
    let chat = state.chat();
    let params = serde_json::json!({ "_session_key": session_key });
    let res = chat
        .context(params)
        .await
        .map_err(ChannelError::unavailable)?;

    let session_info = res.get("session").cloned().unwrap_or_default();
    let msg_count = session_info
        .get("messageCount")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    let provider = session_info
        .get("provider")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown");
    let model = session_info
        .get("model")
        .and_then(|v| v.as_str())
        .unwrap_or("default");

    let tokens = res.get("tokenUsage").cloned().unwrap_or_default();
    let total = tokens.get("total").and_then(|v| v.as_u64()).unwrap_or(0);
    let context_window = tokens
        .get("contextWindow")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);

    // Sandbox section
    let sandbox = res.get("sandbox").cloned().unwrap_or_default();
    let sandbox_enabled = sandbox
        .get("enabled")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let sandbox_line = if sandbox_enabled {
        let image = sandbox
            .get("image")
            .and_then(|v| v.as_str())
            .unwrap_or("default");
        format!("**Sandbox:** on \u{00b7} `{image}`")
    } else {
        "**Sandbox:** off".to_string()
    };

    // Skills/plugins section
    let skills = res
        .get("skills")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    let skills_line = if skills.is_empty() {
        "**Plugins:** none".to_string()
    } else {
        let names: Vec<_> = skills
            .iter()
            .filter_map(|s| s.get("name").and_then(|v| v.as_str()))
            .collect();
        format!("**Plugins:** {}", names.join(", "))
    };

    Ok(format!(
        "**Session:** `{session_key}`\n**Messages:** {msg_count}\n**Provider:** {provider}\n**Model:** `{model}`\n{sandbox_line}\n{skills_line}\n**Tokens:** ~{total}/{context_window}"
    ))
}

pub(in crate::channel_events) async fn handle_sessions(
    state: &Arc<GatewayState>,
    session_metadata: &SqliteSessionMetadata,
    session_key: &str,
    reply_to: &ChannelReplyTarget,
    args: &str,
) -> ChannelResult<String> {
    let sessions = session_metadata
        .list_channel_sessions(
            reply_to.channel_type.as_str(),
            &reply_to.account_id,
            &reply_to.chat_id,
        )
        .await;

    if sessions.is_empty() {
        return Ok("No sessions found. Send a message to start one.".to_string());
    }

    if args.is_empty() {
        Ok(format_channel_sessions_list(&sessions, session_key))
    } else {
        // Switch mode.
        let n = parse_numbered_selection(args, "sessions")?;
        if n == 0 || n > sessions.len() {
            return Err(ChannelError::invalid_input(format!(
                "invalid session number. Use 1\u{2013}{}.",
                sessions.len()
            )));
        }
        let target_session = &sessions[n - 1];

        // Update forward mapping.
        session_metadata
            .set_active_session(
                reply_to.channel_type.as_str(),
                &reply_to.account_id,
                &reply_to.chat_id,
                reply_to.thread_id.as_deref(),
                &target_session.key,
            )
            .await;

        let label = target_session
            .label
            .as_deref()
            .unwrap_or(&target_session.key);
        info!(
            session = %target_session.key,
            "channel /sessions: switched session"
        );

        broadcast(
            state,
            "session",
            serde_json::json!({
                "kind": "switched",
                "sessionKey": &target_session.key,
            }),
            BroadcastOpts {
                drop_if_slow: true,
                ..Default::default()
            },
        )
        .await;

        Ok(format!("Switched to: {label}"))
    }
}

pub(in crate::channel_events) async fn handle_attach(
    state: &Arc<GatewayState>,
    session_metadata: &SqliteSessionMetadata,
    session_key: &str,
    reply_to: &ChannelReplyTarget,
    args: &str,
) -> ChannelResult<String> {
    let sessions: Vec<_> = session_metadata
        .list_account_sessions(reply_to.channel_type.as_str(), &reply_to.account_id)
        .await
        .into_iter()
        .filter(is_attachable_session)
        .collect();

    if sessions.is_empty() {
        return Ok("No attachable sessions found yet.".to_string());
    }

    if args.is_empty() {
        return Ok(format_attachable_sessions_list(&sessions, session_key));
    }

    let n = parse_numbered_selection(args, "attach")?;
    if n == 0 || n > sessions.len() {
        return Err(ChannelError::invalid_input(format!(
            "invalid session number. Use 1\u{2013}{}.",
            sessions.len()
        )));
    }

    let target_session = &sessions[n - 1];
    let binding_json = serde_json::to_string(reply_to)
        .map_err(|e| ChannelError::external("serialize channel binding", e))?;

    session_metadata
        .clear_active_session_mappings(&target_session.key)
        .await;
    session_metadata
        .set_channel_binding(&target_session.key, Some(binding_json))
        .await;
    session_metadata
        .set_active_session(
            reply_to.channel_type.as_str(),
            &reply_to.account_id,
            &reply_to.chat_id,
            reply_to.thread_id.as_deref(),
            &target_session.key,
        )
        .await;

    let label = session_list_label(target_session);
    info!(
        session = %target_session.key,
        "channel /attach: rebound existing session to current chat"
    );

    broadcast(
        state,
        "session",
        serde_json::json!({
            "kind": "switched",
            "sessionKey": &target_session.key,
        }),
        BroadcastOpts {
            drop_if_slow: true,
            ..Default::default()
        },
    )
    .await;

    Ok(format!("Attached here: {label}"))
}

#[cfg(test)]
mod tests {
    use super::require_channel_model_id;

    #[test]
    fn channel_new_requires_an_explicit_model_id() {
        assert_eq!(
            require_channel_model_id(Some("provider::model".to_string()))
                .ok()
                .as_deref(),
            Some("provider::model")
        );
        assert!(require_channel_model_id(None).is_err());
        assert!(require_channel_model_id(Some("   ".to_string())).is_err());
    }
}
