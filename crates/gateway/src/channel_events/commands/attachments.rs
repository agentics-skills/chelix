use std::sync::Arc;

use tracing::{debug, error, warn};

use {
    chelix_channels::{ChannelAttachment, ChannelMessageMeta, ChannelReplyTarget},
    chelix_service_traits::{
        ChatChannelMetadata, ChatExecutionContext, ChatSendDocument, ChatSendMessage,
        ChatSendRequest,
    },
    chelix_sessions::{QueuedPromptContentBlock, QueuedPromptImageUrl, SessionKey},
};

use crate::{
    broadcast::{BroadcastOpts, broadcast},
    state::GatewayState,
};

use super::super::{
    prepare_channel_session, report_channel_error, resolve_channel_session,
    start_channel_typing_loop,
};

pub(in crate::channel_events) async fn dispatch_to_chat_with_attachments(
    state: &Arc<tokio::sync::OnceCell<Arc<GatewayState>>>,
    text: &str,
    attachments: Vec<ChannelAttachment>,
    reply_to: ChannelReplyTarget,
    meta: ChannelMessageMeta,
) {
    if attachments.is_empty() {
        super::super::dispatch::dispatch_to_chat(state, text, reply_to, meta).await;
        return;
    }

    let Some(state) = state.get() else {
        warn!("channel dispatch_to_chat_with_attachments: gateway not ready");
        return;
    };

    // Start typing immediately so image preprocessing and session setup do not
    // delay channel feedback.
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

    let mut content = Vec::with_capacity(attachments.len() + usize::from(!text.is_empty()));
    if !text.is_empty() {
        content.push(QueuedPromptContentBlock::Text {
            text: text.to_string(),
        });
    }
    for attachment in &attachments {
        let base64_data =
            base64::Engine::encode(&base64::engine::general_purpose::STANDARD, &attachment.data);
        content.push(QueuedPromptContentBlock::ImageUrl {
            image_url: QueuedPromptImageUrl {
                url: format!("data:{};base64,{}", attachment.media_type, base64_data),
            },
        });
    }

    debug!(
        session_key = %session_key,
        text_len = text.len(),
        attachment_count = attachments.len(),
        "dispatching multimodal message to chat"
    );

    // Broadcast before chat.send persists the message. Without a message index,
    // concurrent channel messages cannot be incorrectly deduplicated by the client.
    let payload = serde_json::json!({
        "state": "channel_user",
        "text": if text.is_empty() { "[Image]" } else { text },
        "channel": &meta,
        "sessionKey": &session_key,
        "hasAttachments": true,
    });
    broadcast(state, "chat", payload, BroadcastOpts {
        drop_if_slow: true,
        ..Default::default()
    })
    .await;

    // Channel platforms do not expose bot read receipts. Use inbound
    // user activity as a heuristic and mark prior session history seen.
    state.services.session.mark_seen(&session_key).await;

    let request = ChatSendRequest {
        message: ChatSendMessage::Content(content),
        model_override: None,
        tool_choice: None,
        documents: meta
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
            .collect(),
        audio_filename: meta.audio_filename.clone(),
        input_medium: None,
        client_sequence: None,
    };

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

    let mut context = ChatExecutionContext::internal(SessionKey::new(session_key));
    context.channel = Some(ChatChannelMetadata::from(meta));
    context.channel_reply_target = Some(reply_to.clone());

    let send_result = state.chat().send(request, context).await;
    if let Some(done_tx) = typing_done {
        let _ = done_tx.send(());
    }

    if let Err(error) = send_result {
        error!(%error, "channel dispatch_to_chat_with_attachments failed");
        report_channel_error(state, &reply_to, &error).await;
    }
}
