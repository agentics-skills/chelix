//! Message content conversion and user document handling.

use tracing::{debug, warn};

use {
    chelix_agents::{
        ContentPart, UserContent, multimodal::parse_data_uri, prompt::VOICE_REPLY_SUFFIX,
    },
    chelix_channels::ChannelMessageKind,
    chelix_service_traits::{
        ChatChannelMetadata, ChatSendDocument, ChatSendMessage, ChatSendRequest,
    },
    chelix_sessions::{
        ContentBlock, MessageContent, QueuedPromptContentBlock, UserDocument, store::SessionStore,
    },
};

use crate::types::{
    ReplyMedium, is_safe_user_audio_filename, sanitize_user_document_display_name,
    truncate_at_char_boundary,
};

/// Convert a closed chat message into persisted content and its display text.
pub(crate) fn chat_message_parts(
    message: &ChatSendMessage,
) -> Result<(String, MessageContent), String> {
    match message {
        ChatSendMessage::Text(text) => Ok((text.clone(), MessageContent::Text(text.clone()))),
        ChatSendMessage::Content(blocks) => {
            if blocks.is_empty() {
                return Err("content must contain at least one block".to_string());
            }
            let mut content = Vec::with_capacity(blocks.len());
            for block in blocks {
                content.push(match block {
                    QueuedPromptContentBlock::Text { text } => {
                        ContentBlock::Text { text: text.clone() }
                    },
                    QueuedPromptContentBlock::ImageUrl { image_url } => {
                        if parse_data_uri(&image_url.url).is_none() {
                            return Err(
                                "content image_url must contain a valid data URI".to_string()
                            );
                        }
                        ContentBlock::ImageUrl {
                            image_url: chelix_sessions::message::ImageUrl {
                                url: image_url.url.clone(),
                            },
                        }
                    },
                });
            }
            let text = content
                .iter()
                .find_map(|block| match block {
                    ContentBlock::Text { text } => Some(text.clone()),
                    ContentBlock::ImageUrl { .. } => None,
                })
                .unwrap_or_else(|| "[Image]".to_string());
            Ok((text, MessageContent::Multimodal(content)))
        },
    }
}

/// Resolve closed document metadata to one session-local user document list.
pub(crate) fn chat_user_documents(
    documents: &[ChatSendDocument],
    session_key: &str,
    session_store: &SessionStore,
) -> Result<Vec<UserDocument>, String> {
    let media_dir_key = SessionStore::key_to_filename(session_key);
    documents
        .iter()
        .map(|document| {
            let stored_filename = document.stored_filename.trim();
            if stored_filename != document.stored_filename
                || !is_safe_user_audio_filename(stored_filename)
            {
                return Err(format!(
                    "document storedFilename '{}' is invalid",
                    document.stored_filename
                ));
            }
            let mime_type = document.mime_type.trim();
            if mime_type != document.mime_type || mime_type.is_empty() {
                return Err(format!(
                    "document mimeType for '{}' is invalid",
                    document.stored_filename
                ));
            }
            let display_name = sanitize_user_document_display_name(&document.display_name)
                .filter(|value| value == &document.display_name)
                .ok_or_else(|| {
                    format!(
                        "document displayName for '{}' is invalid",
                        document.stored_filename
                    )
                })?;
            Ok(UserDocument {
                display_name,
                stored_filename: stored_filename.to_string(),
                mime_type: mime_type.to_string(),
                size_bytes: document.size_bytes,
                media_ref: format!("media/{media_dir_key}/{stored_filename}"),
                absolute_path: Some(
                    session_store
                        .media_path_for(session_key, stored_filename)
                        .to_string_lossy()
                        .to_string(),
                ),
            })
        })
        .collect()
}

/// Resolve a closed audio filename to its session-local media reference.
pub(crate) fn chat_user_audio_path(
    audio_filename: Option<&str>,
    session_key: &str,
) -> Result<Option<String>, String> {
    let Some(filename) = audio_filename else {
        return Ok(None);
    };
    if !is_safe_user_audio_filename(filename) {
        return Err(format!("audioFilename '{filename}' is invalid"));
    }
    let key = SessionStore::key_to_filename(session_key);
    Ok(Some(format!("media/{key}/{filename}")))
}

/// Infer the requested reply medium from a closed chat request and execution context.
pub(crate) fn infer_chat_reply_medium(
    request: &ChatSendRequest,
    channel: Option<&ChatChannelMetadata>,
    text: &str,
) -> ReplyMedium {
    if let Some(explicit) = explicit_reply_medium_override(text) {
        return explicit;
    }
    if let Some(input_medium) = request.input_medium {
        return input_medium;
    }
    if channel
        .is_some_and(|metadata| matches!(metadata.message_kind, Some(ChannelMessageKind::Voice)))
    {
        return ReplyMedium::Voice;
    }
    ReplyMedium::Text
}

/// Convert session-crate `MessageContent` to agents-crate `UserContent`.
///
/// The two types have different image representations:
/// - `ContentBlock::ImageUrl` stores a data URI string
/// - `ContentPart::Image` stores separated `media_type` + `data` fields
pub(crate) fn format_user_documents_context(documents: &[UserDocument]) -> Option<String> {
    if documents.is_empty() {
        return None;
    }

    let mut sections = Vec::with_capacity(documents.len() + 1);
    sections.push("[Inbound documents available]".to_string());
    for document in documents {
        let size = document
            .size_bytes
            .map(|value| format!("\nsize_bytes: {value}"))
            .unwrap_or_default();
        sections.push(format!(
            "filename: {}\nmime_type: {}{}\nlocal_path: {}\nmedia_ref: {}",
            document.display_name,
            document.mime_type,
            size,
            document
                .absolute_path
                .as_deref()
                .unwrap_or(&document.media_ref),
            document.media_ref
        ));
    }

    Some(sections.join("\n\n"))
}

pub(crate) fn append_user_documents_to_text(text: &str, documents: &[UserDocument]) -> String {
    if let Some(context) = format_user_documents_context(documents) {
        if text.trim().is_empty() {
            context
        } else {
            format!("{text}\n\n{context}")
        }
    } else {
        text.to_string()
    }
}

pub(crate) fn to_user_content(mc: &MessageContent, documents: &[UserDocument]) -> UserContent {
    match mc {
        MessageContent::Text(text) => {
            UserContent::Text(append_user_documents_to_text(text, documents))
        },
        MessageContent::Multimodal(blocks) => {
            let mut parts: Vec<ContentPart> = blocks
                .iter()
                .filter_map(|block| match block {
                    ContentBlock::Text { text } => Some(ContentPart::Text(text.clone())),
                    ContentBlock::ImageUrl { image_url } => match parse_data_uri(&image_url.url) {
                        Some((media_type, data)) => {
                            debug!(
                                media_type,
                                data_len = data.len(),
                                "to_user_content: parsed image from data URI"
                            );
                            Some(ContentPart::Image {
                                media_type: media_type.to_string(),
                                data: data.to_string(),
                            })
                        },
                        None => {
                            warn!(
                                url_prefix = truncate_at_char_boundary(&image_url.url, 80),
                                "to_user_content: failed to parse data URI, dropping image"
                            );
                            None
                        },
                    },
                })
                .collect();
            if let Some(context) = format_user_documents_context(documents) {
                if let Some(ContentPart::Text(text)) = parts
                    .iter_mut()
                    .find(|part| matches!(part, ContentPart::Text(_)))
                {
                    if !text.trim().is_empty() {
                        text.push_str("\n\n");
                    }
                    text.push_str(&context);
                } else {
                    parts.insert(0, ContentPart::Text(context));
                }
            }
            let text_count = parts
                .iter()
                .filter(|p| matches!(p, ContentPart::Text(_)))
                .count();
            let image_count = parts
                .iter()
                .filter(|p| matches!(p, ContentPart::Image { .. }))
                .count();
            debug!(
                text_count,
                image_count,
                total_blocks = blocks.len(),
                "to_user_content: converted multimodal content"
            );
            UserContent::Multimodal(parts)
        },
    }
}

pub(crate) fn rewrite_multimodal_text_blocks(
    blocks: &[ContentBlock],
    new_text: &str,
) -> Vec<ContentBlock> {
    let mut rewritten = Vec::with_capacity(blocks.len().max(1));
    let mut inserted_text = false;

    for block in blocks {
        match block {
            ContentBlock::Text { .. } if !inserted_text => {
                rewritten.push(ContentBlock::Text {
                    text: new_text.to_string(),
                });
                inserted_text = true;
            },
            ContentBlock::Text { .. } => {},
            _ => rewritten.push(block.clone()),
        }
    }

    if !inserted_text {
        rewritten.insert(0, ContentBlock::Text {
            text: new_text.to_string(),
        });
    }

    rewritten
}

pub(crate) fn apply_message_received_rewrite(message_content: &mut MessageContent, new_text: &str) {
    match message_content {
        MessageContent::Text(text) => *text = new_text.to_string(),
        MessageContent::Multimodal(blocks) => {
            *blocks = rewrite_multimodal_text_blocks(blocks, new_text);
        },
    }
}

pub(crate) fn explicit_reply_medium_override(text: &str) -> Option<ReplyMedium> {
    let lower = text.to_lowercase();
    let voice_markers = [
        "talk to me",
        "say it",
        "say this",
        "speak",
        "voice message",
        "respond with voice",
        "reply with voice",
        "audio reply",
    ];
    if voice_markers.iter().any(|m| lower.contains(m)) {
        return Some(ReplyMedium::Voice);
    }

    let text_markers = [
        "text only",
        "reply in text",
        "respond in text",
        "don't use voice",
        "do not use voice",
        "no audio",
    ];
    if text_markers.iter().any(|m| lower.contains(m)) {
        return Some(ReplyMedium::Text);
    }

    None
}

pub(crate) fn apply_voice_reply_suffix(
    system_prompt: String,
    desired_reply_medium: ReplyMedium,
) -> String {
    if desired_reply_medium != ReplyMedium::Voice {
        return system_prompt;
    }

    format!("{system_prompt}{VOICE_REPLY_SUFFIX}")
}

pub(crate) fn user_documents_for_persistence(
    documents: &[UserDocument],
) -> Option<Vec<UserDocument>> {
    if documents.is_empty() {
        return None;
    }

    Some(
        documents
            .iter()
            .cloned()
            .map(|mut document| {
                document.absolute_path = None;
                document
            })
            .collect(),
    )
}
