use super::*;

use {
    chelix_service_traits::{
        ChatChannelMetadata, ChatExecutionContext, ChatSendDocument, ChatSendRequest,
    },
    chelix_sessions::SessionKey,
};

pub(in crate::channel_events) async fn dispatch_to_chat(
    state: &Arc<tokio::sync::OnceCell<Arc<GatewayState>>>,
    text: &str,
    reply_to: ChannelReplyTarget,
    meta: ChannelMessageMeta,
) {
    if let Some(state) = state.get() {
        // Start typing immediately so pre-run setup does not delay channel feedback.
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
            meta.model_override.as_ref(),
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

        // Broadcast before chat.send persists the message. Without a message index,
        // concurrent channel messages cannot be incorrectly deduplicated by the client.
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

        let mut request = ChatSendRequest::text(text);
        request.audio_filename = meta.audio_filename.clone();
        request.documents = meta
            .documents
            .as_deref()
            .unwrap_or_default()
            .iter()
            .map(|document| ChatSendDocument {
                display_name: document.display_name.clone(),
                stored_filename: document.stored_filename.clone(),
                mime_type: document.mime_type.clone(),
                size_bytes: document.size_bytes,
            })
            .collect();

        if prepared.created {
            let Some(model_reasoning) = prepared.entry.model_reasoning() else {
                if let Some(done_tx) = typing_done {
                    let _ = done_tx.send(());
                }
                report_channel_error(
                    state,
                    &reply_to,
                    &"new channel session has no model/reasoning pair",
                )
                .await;
                return;
            };
            let message = format!(
                "Using {}. Use /model to change.",
                model_reasoning.model_id()
            );
            state.push_channel_status_log(&session_key, message).await;
        }

        let mut context = ChatExecutionContext::internal(SessionKey::new(session_key.clone()));
        context.channel = Some(ChatChannelMetadata::from(meta));
        context.channel_reply_target = Some(reply_to.clone());

        let send_result = state.chat().send(request, context).await;
        if let Some(done_tx) = typing_done {
            let _ = done_tx.send(());
        }

        if let Err(error) = send_result {
            error!(%error, "channel dispatch_to_chat failed");
            report_channel_error(state, &reply_to, &error).await;
        }
    } else {
        warn!("channel dispatch_to_chat: gateway not ready");
    }
}
