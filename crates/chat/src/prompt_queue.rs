//! Chat integration helpers for the primitive queued-prompts service.

use serde_json::Value;

use {
    chelix_channels::{ChannelMessageKind, ChannelReplyTarget},
    chelix_common::MessageMedium,
    chelix_service_traits::{ChatExecutionContext, ChatSendRequest},
    chelix_sessions::{
        ContentBlock, MessageContent, QueuedPromptChannelMetadata, QueuedPromptContent,
        QueuedPromptContentBlock, QueuedPromptDocument, QueuedPromptImageUrl,
        QueuedPromptMessageContent, QueuedPromptsStatus, SessionKey, UserDocument,
        message::ImageUrl, store::SessionStore,
    },
};

use crate::{
    error::{Error, Result},
    message::{
        chat_message_parts, chat_user_audio_path, chat_user_documents, infer_chat_reply_medium,
    },
    runtime::ChatRuntime,
    types::{BroadcastOpts, ReplyMedium, broadcast},
};

const QUEUE_EVENT_STATE: &str = "prompt_queue";

/// Normalize one ordinary `chat.send` request into closed prompt content.
pub(crate) fn normalize_queued_prompt_content(
    request: &ChatSendRequest,
    context: &ChatExecutionContext,
    session_store: &SessionStore,
) -> Result<QueuedPromptContent> {
    let (text, content) = chat_message_parts(&request.message).map_err(Error::message)?;
    let channel = context
        .channel
        .as_ref()
        .map(|metadata| QueuedPromptChannelMetadata {
            channel_type: metadata.channel_type,
            sender_name: metadata.sender_name.clone(),
            username: metadata.username.clone(),
            sender_id: metadata.sender_id.clone(),
            message_kind: metadata.message_kind,
        });
    let input_medium = request.input_medium.unwrap_or_else(|| {
        if channel.as_ref().is_some_and(|metadata| {
            matches!(metadata.message_kind, Some(ChannelMessageKind::Voice))
        }) {
            MessageMedium::Voice
        } else {
            MessageMedium::Text
        }
    });
    let reply_medium = infer_chat_reply_medium(request, context.channel.as_ref(), &text);
    let documents = chat_user_documents(
        &request.documents,
        context.session_id.as_str(),
        session_store,
    )
    .map_err(Error::message)?
    .into_iter()
    .map(|document| QueuedPromptDocument {
        display_name: document.display_name,
        stored_filename: document.stored_filename,
        mime_type: document.mime_type,
        size_bytes: document.size_bytes,
        media_ref: document.media_ref,
    })
    .collect();
    let audio = chat_user_audio_path(
        request.audio_filename.as_deref(),
        context.session_id.as_str(),
    )
    .map_err(Error::message)?;

    Ok(QueuedPromptContent {
        content: queue_message_content(content),
        documents,
        audio,
        client_sequence: request.client_sequence,
        input_medium,
        reply_medium,
        channel,
        channel_reply_target: context.channel_reply_target.clone(),
    })
}

/// Broadcast one canonical queue status without reading or merging state.
pub(crate) async fn broadcast_queued_prompts_status(
    state: &std::sync::Arc<dyn ChatRuntime>,
    status: &QueuedPromptsStatus,
) -> Result<()> {
    let status = serde_json::to_value(status)?;
    broadcast(
        state,
        "chat",
        serde_json::json!({
            "state": QUEUE_EVENT_STATE,
            "status": status,
        }),
        BroadcastOpts::default(),
    )
    .await;
    Ok(())
}

pub(crate) fn queued_message_content(content: &QueuedPromptContent) -> MessageContent {
    match &content.content {
        QueuedPromptMessageContent::Text(text) => MessageContent::Text(text.clone()),
        QueuedPromptMessageContent::Multimodal(blocks) => MessageContent::Multimodal(
            blocks
                .iter()
                .map(|block| match block {
                    QueuedPromptContentBlock::Text { text } => {
                        ContentBlock::Text { text: text.clone() }
                    },
                    QueuedPromptContentBlock::ImageUrl { image_url } => ContentBlock::ImageUrl {
                        image_url: ImageUrl {
                            url: image_url.url.clone(),
                        },
                    },
                })
                .collect(),
        ),
    }
}

pub(crate) fn queued_documents(
    content: &QueuedPromptContent,
    session_id: &SessionKey,
    session_store: &SessionStore,
) -> Vec<UserDocument> {
    content
        .documents
        .iter()
        .map(|document| UserDocument {
            display_name: document.display_name.clone(),
            stored_filename: document.stored_filename.clone(),
            mime_type: document.mime_type.clone(),
            size_bytes: document.size_bytes,
            media_ref: document.media_ref.clone(),
            absolute_path: Some(
                session_store
                    .media_path_for(session_id.as_str(), &document.stored_filename)
                    .to_string_lossy()
                    .to_string(),
            ),
        })
        .collect()
}

pub(crate) fn queued_channel_value(content: &QueuedPromptContent) -> Result<Option<Value>> {
    content
        .channel
        .as_ref()
        .map(serde_json::to_value)
        .transpose()
        .map_err(Error::from)
}

pub(crate) fn queued_channel_reply_target(
    content: &QueuedPromptContent,
) -> Option<ChannelReplyTarget> {
    content.channel_reply_target.clone()
}

pub(crate) fn queued_reply_medium(content: &QueuedPromptContent) -> ReplyMedium {
    content.reply_medium
}

pub(crate) fn queued_sender_name(content: &QueuedPromptContent) -> Option<String> {
    content.channel.as_ref().and_then(|channel| {
        channel
            .sender_name
            .clone()
            .or_else(|| channel.username.clone())
    })
}

pub(crate) fn queued_message_text(content: &QueuedPromptContent) -> String {
    match &content.content {
        QueuedPromptMessageContent::Text(text) => text.clone(),
        QueuedPromptMessageContent::Multimodal(blocks) => blocks
            .iter()
            .find_map(|block| match block {
                QueuedPromptContentBlock::Text { text } => Some(text.clone()),
                QueuedPromptContentBlock::ImageUrl { .. } => None,
            })
            .unwrap_or_else(|| "[Image]".to_string()),
    }
}

fn queue_message_content(content: MessageContent) -> QueuedPromptMessageContent {
    match content {
        MessageContent::Text(text) => QueuedPromptMessageContent::Text(text),
        MessageContent::Multimodal(blocks) => QueuedPromptMessageContent::Multimodal(
            blocks
                .into_iter()
                .map(|block| match block {
                    ContentBlock::Text { text } => QueuedPromptContentBlock::Text { text },
                    ContentBlock::ImageUrl { image_url } => QueuedPromptContentBlock::ImageUrl {
                        image_url: QueuedPromptImageUrl { url: image_url.url },
                    },
                })
                .collect(),
        ),
    }
}
