//! Chat integration helpers for the primitive queued-prompts service.

use serde_json::Value;

use {
    chelix_channels::{ChannelMessageKind, ChannelReplyTarget},
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
        infer_reply_medium, parse_message_params, user_audio_path_from_params,
        user_documents_from_params,
    },
    runtime::ChatRuntime,
    types::{BroadcastOpts, ReplyMedium, broadcast},
};

const QUEUE_EVENT_STATE: &str = "prompt_queue";

/// Normalize one ordinary `chat.send` request into closed prompt content.
pub(crate) fn normalize_queued_prompt_content(
    params: &Value,
    session_id: &SessionKey,
    session_store: &SessionStore,
) -> Result<QueuedPromptContent> {
    let (_, content) = parse_message_params(params).map_err(Error::message)?;
    let channel = params
        .get("channel")
        .map(parse_channel_metadata)
        .transpose()?;
    let input_medium = parse_input_medium(params, channel.as_ref())?;
    let reply_medium = infer_reply_medium(params, &message_text(&content));
    let documents = user_documents_from_params(params, session_id.as_str(), session_store)
        .unwrap_or_default()
        .into_iter()
        .map(|document| QueuedPromptDocument {
            display_name: document.display_name,
            stored_filename: document.stored_filename,
            mime_type: document.mime_type,
            size_bytes: document.size_bytes,
            media_ref: document.media_ref,
        })
        .collect();
    let channel_reply_target = params
        .get("_channel_reply_target")
        .map(|value| {
            serde_json::from_value::<ChannelReplyTarget>(value.clone()).map_err(Error::from)
        })
        .transpose()?;

    Ok(QueuedPromptContent {
        content: queue_message_content(content),
        documents,
        audio: user_audio_path_from_params(params, session_id.as_str()),
        client_sequence: params.get("_seq").and_then(Value::as_u64),
        input_medium,
        reply_medium,
        channel,
        channel_reply_target,
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

fn parse_channel_metadata(value: &Value) -> Result<QueuedPromptChannelMetadata> {
    let object = value
        .as_object()
        .ok_or_else(|| Error::message("'channel' must be an object"))?;
    let channel_type = object
        .get("channel_type")
        .ok_or_else(|| Error::message("channel metadata is missing 'channel_type'"))?;
    let channel_type = serde_json::from_value(channel_type.clone())?;
    let message_kind = object
        .get("message_kind")
        .filter(|value| !value.is_null())
        .map(|value| serde_json::from_value(value.clone()))
        .transpose()?;

    Ok(QueuedPromptChannelMetadata {
        channel_type,
        sender_name: optional_string(object, "sender_name")?,
        username: optional_string(object, "username")?,
        sender_id: optional_string(object, "sender_id")?,
        message_kind,
    })
}

fn optional_string(object: &serde_json::Map<String, Value>, field: &str) -> Result<Option<String>> {
    match object.get(field) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) => Ok(Some(value.clone())),
        Some(_) => Err(Error::message(format!(
            "channel metadata field '{field}' must be a string or null"
        ))),
    }
}

fn parse_input_medium(
    params: &Value,
    channel: Option<&QueuedPromptChannelMetadata>,
) -> Result<ReplyMedium> {
    if let Some(value) = params.get("_input_medium") {
        return Ok(serde_json::from_value(value.clone())?);
    }
    if channel
        .is_some_and(|metadata| matches!(metadata.message_kind, Some(ChannelMessageKind::Voice)))
    {
        return Ok(ReplyMedium::Voice);
    }
    Ok(ReplyMedium::Text)
}

fn message_text(content: &MessageContent) -> String {
    match content {
        MessageContent::Text(text) => text.clone(),
        MessageContent::Multimodal(blocks) => blocks
            .iter()
            .find_map(|block| match block {
                ContentBlock::Text { text } => Some(text.clone()),
                ContentBlock::ImageUrl { .. } => None,
            })
            .unwrap_or_else(|| "[Image]".to_string()),
    }
}
