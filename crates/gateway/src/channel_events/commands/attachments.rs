use std::sync::Arc;

use tracing::{debug, error, warn};

use chelix_channels::{
    ChannelAttachment, ChannelMessageMeta, ChannelReplyTarget, Result as ChannelResult,
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
        // No attachments, use the regular dispatch
        super::super::dispatch::dispatch_to_chat(state, text, reply_to, meta).await;
        return;
    }

    let Some(state) = state.get() else {
        warn!("channel dispatch_to_chat_with_attachments: gateway not ready");
        return;
    };

    // Start typing immediately so image preprocessing/session setup doesn't
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

    // Build multimodal content array (OpenAI format)
    let mut content_parts: Vec<serde_json::Value> = Vec::new();

    // Add text part if not empty
    if !text.is_empty() {
        content_parts.push(serde_json::json!({
            "type": "text",
            "text": text,
        }));
    }

    // Add image parts
    for attachment in &attachments {
        let base64_data =
            base64::Engine::encode(&base64::engine::general_purpose::STANDARD, &attachment.data);
        let data_uri = format!("data:{};base64,{}", attachment.media_type, base64_data);
        content_parts.push(serde_json::json!({
            "type": "image_url",
            "image_url": {
                "url": data_uri,
            },
        }));
    }

    debug!(
        session_key = %session_key,
        text_len = text.len(),
        attachment_count = attachments.len(),
        "dispatching multimodal message to chat"
    );

    // Broadcast a "chat" event so the web UI shows the user message.
    // See the text-only dispatch above for why messageIndex is omitted.
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

    let chat = state.chat();
    let mut params = serde_json::json!({
        "content": content_parts,
        "channel": &meta,
        "_session_key": &session_key,
        // Defer reply-target registration until chat.send() actually
        // starts executing this message (after semaphore acquire).
        "_channel_reply_target": &reply_to,
    });
    if let Some(ref documents) = meta.documents {
        params["_document_files"] = serde_json::json!(documents);
    }

    // Persist a complete model/reasoning pair on first use. Once the shared
    // channel session is initialized, keep per-sender channel models runtime-only.
    let model_reasoning: ChannelResult<Option<(String, String, bool)>> = async {
        if prepared.created {
            let model_reasoning = prepared.entry.model_reasoning().ok_or_else(|| {
                chelix_channels::Error::unavailable(
                    "new channel session has no model/reasoning pair",
                )
            })?;
            Ok(Some((
                model_reasoning.model_id().to_string(),
                model_reasoning.reasoning_effort().as_str().to_string(),
                true,
            )))
        } else if let Some(model) = meta.model.as_deref() {
            let resolved =
                super::super::resolve_channel_runtime_model(state, &session_key, model).await?;
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
        error!("channel dispatch_to_chat_with_attachments failed: {e}");
        if let Some(outbound) = state.services.channel_outbound_arc() {
            let error_msg = format!("\u{26a0}\u{fe0f} {e}");
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
}
