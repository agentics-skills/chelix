use super::*;

pub(in crate::channel_events) async fn dispatch_to_chat(
    state: &Arc<tokio::sync::OnceCell<Arc<GatewayState>>>,
    text: &str,
    reply_to: ChannelReplyTarget,
    meta: ChannelMessageMeta,
) {
    if let Some(state) = state.get() {
        // Start typing immediately so pre-run setup (session/model resolution)
        // does not delay channel feedback.
        let typing_done = start_channel_typing_loop(state, &reply_to);

        let Some(session_metadata) = state.services.session_metadata.as_ref() else {
            if let Some(done_tx) = typing_done {
                let _ = done_tx.send(());
            }
            report_channel_error(state, &reply_to, &"session metadata is not available").await;
            return;
        };
        let session_key = match resolve_channel_session(&reply_to, session_metadata).await {
            Ok(session_key) => session_key,
            Err(error) => {
                if let Some(done_tx) = typing_done {
                    let _ = done_tx.send(());
                }
                report_channel_error(state, &reply_to, &error).await;
                return;
            },
        };
        let prepared = match prepare_channel_session(
            state,
            session_metadata,
            &session_key,
            &reply_to,
            meta.agent_id.as_deref(),
            meta.model.as_deref(),
        )
        .await
        {
            Ok(prepared) => prepared,
            Err(error) => {
                if let Some(done_tx) = typing_done {
                    let _ = done_tx.send(());
                }
                report_channel_error(state, &reply_to, &error).await;
                return;
            },
        };
        // Broadcast a "chat" event so the web UI shows the user message
        // in real-time (like typing from the UI).
        //
        // We intentionally omit `messageIndex` here: the broadcast fires
        // *before* chat.send() persists the message, so store.count()
        // would be stale.  Concurrent channel messages would get the same
        // index, causing the client-side dedup to drop the second one.
        // Without a messageIndex the client skips its dedup check and
        // always renders the message.
        let payload = serde_json::json!({
            "state": "channel_user",
            "text": text,
            "channel": &meta,
            "sessionKey": &session_key,
        });
        broadcast(state, "chat", payload, BroadcastOpts {
            drop_if_slow: false,
            ..Default::default()
        })
        .await;

        // Channel platforms do not expose bot read receipts. Use inbound
        // user activity as a heuristic and mark prior session history seen.
        state.services.session.mark_seen(&session_key).await;

        // If the message is a thread reply, fetch prior thread messages
        // for context injection so the LLM sees the conversation history.
        let thread_context = if let Some(ref thread_id) = reply_to.message_id
            && let Some(ref reg) = state.services.channel_registry
        {
            match reg
                .fetch_thread_messages(&reply_to.account_id, &reply_to.chat_id, thread_id, 20)
                .await
            {
                Ok(msgs) if !msgs.is_empty() => {
                    let history: Vec<serde_json::Value> = msgs
                        .iter()
                        .map(|m| {
                            serde_json::json!({
                                "role": if m.is_bot { "assistant" } else { "user" },
                                "text": m.text,
                                "sender_id": m.sender_id,
                                "timestamp": m.timestamp,
                            })
                        })
                        .collect();
                    Some(history)
                },
                Ok(_) => None,
                Err(e) => {
                    debug!("failed to fetch thread context: {e}");
                    None
                },
            }
        } else {
            None
        };

        let chat = state.chat();
        let mut params = serde_json::json!({
            "text": text,
            "channel": &meta,
            "_session_key": &session_key,
            // Defer reply-target registration until chat.send() actually
            // starts executing this message (after semaphore acquire).
            "_channel_reply_target": &reply_to,
        });

        // Attach thread context if available.
        if let Some(thread_history) = thread_context {
            params["_thread_context"] = serde_json::json!(thread_history);
        }
        // Thread saved voice audio filename so chat.rs persists the audio path.
        if let Some(ref audio_filename) = meta.audio_filename {
            params["_audio_filename"] = serde_json::json!(audio_filename);
        }
        if let Some(ref documents) = meta.documents {
            params["_document_files"] = serde_json::json!(documents);
        }

        // Persist a complete model/reasoning pair on first use. Once the shared
        // channel session is initialized, keep per-sender channel models runtime-only.
        let model_reasoning: ChannelResult<Option<(String, String, bool)>> = async {
            if prepared.created {
                let model_reasoning = prepared.entry.model_reasoning().ok_or_else(|| {
                    ChannelError::unavailable("new channel session has no model/reasoning pair")
                })?;
                Ok(Some((
                    model_reasoning.model_id().to_string(),
                    model_reasoning.reasoning_effort().as_str().to_string(),
                    true,
                )))
            } else if let Some(model) = meta.model.as_deref() {
                let resolved = resolve_channel_runtime_model(state, &session_key, model).await?;
                Ok(Some((
                    resolved.model_id().to_string(),
                    resolved.reasoning_effort().as_str().to_string(),
                    false,
                )))
            } else {
                Ok(None)
            }
        }
        .await;

        let model_reasoning = match model_reasoning {
            Ok(model_reasoning) => model_reasoning,
            Err(error) => {
                if let Some(done_tx) = typing_done {
                    let _ = done_tx.send(());
                }
                report_channel_error(state, &reply_to, &error).await;
                return;
            },
        };
        if let Some((model, reasoning_effort, persisted)) = model_reasoning {
            params["model"] = serde_json::json!(&model);
            params["reasoningEffort"] = serde_json::json!(reasoning_effort);
            if persisted {
                let message = format!("Using {model}. Use /model to change.");
                state.push_channel_status_log(&session_key, message).await;
            }
        }

        let send_result = chat.send(params).await;
        if let Some(done_tx) = typing_done {
            let _ = done_tx.send(());
        }

        if let Err(e) = send_result {
            error!("channel dispatch_to_chat failed: {e}");
            // Send the error back to the originating channel so the user
            // knows something went wrong.
            if let Some(outbound) = state.services.channel_outbound_arc() {
                let error_msg = format!("⚠️ {e}");
                if let Err(send_err) = outbound
                    .send_text(
                        &reply_to.account_id,
                        &reply_to.outbound_to(),
                        &error_msg,
                        reply_to.message_id.as_deref(),
                    )
                    .await
                {
                    warn!("failed to send error back to channel: {send_err}");
                }
            }
        }
    } else {
        warn!("channel dispatch_to_chat: gateway not ready");
    }
}
