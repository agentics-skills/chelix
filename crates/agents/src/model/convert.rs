use {
    crate::multimodal::parse_data_uri,
    chelix_common::tool_lifecycle::{ToolLifecycleEvent, ToolLifecycleUpdate},
};

/// Failure to reconstruct provider input from persisted or hook-modified JSON.
#[derive(Debug, thiserror::Error)]
pub enum ChatMessageConversionError {
    /// Visible reasoning content could not be decoded.
    #[error("message at index {message_index} has invalid reasoning: {source}")]
    Reasoning {
        message_index: usize,
        #[source]
        source: serde_json::Error,
    },
    /// `responsesReasoning` was present with a non-array value.
    #[error("assistant message at index {message_index} has non-array responsesReasoning")]
    ResponsesReasoningCollection { message_index: usize },
    /// One opaque Responses reasoning item could not be decoded.
    #[error(
        "assistant message at index {message_index} has invalid responsesReasoning item at index {item_index}: {source}"
    )]
    ResponsesReasoningItem {
        message_index: usize,
        item_index: usize,
        #[source]
        source: serde_json::Error,
    },
    /// A persisted tool lifecycle record could not be decoded.
    #[error("message at index {message_index} has invalid tool lifecycle: {source}")]
    ToolLifecycle {
        message_index: usize,
        #[source]
        source: serde_json::Error,
    },
}

use super::{
    chat::{ChatMessage, ContentPart, UserContent},
    decode_tool_call_arguments_with_diagnostic,
    types::ToolCall,
};

fn decode_reasoning(
    value: Option<&serde_json::Value>,
    message_index: usize,
) -> Result<Option<chelix_common::ReasoningContent>, ChatMessageConversionError> {
    let Some(value) = value.filter(|value| !value.is_null()) else {
        return Ok(None);
    };
    let reasoning = serde_json::from_value::<chelix_common::ReasoningContent>(value.clone())
        .map_err(|source| ChatMessageConversionError::Reasoning {
            message_index,
            source,
        })?;
    Ok((!reasoning.is_blank()).then_some(reasoning))
}

fn document_absolute_path_from_media_ref(media_ref: &str) -> String {
    use std::path::Path;
    if Path::new(media_ref).is_absolute() {
        return media_ref.to_string();
    }

    chelix_config::data_dir()
        .join("sessions")
        .join(media_ref)
        .to_string_lossy()
        .to_string()
}

/// Convert persisted JSON messages (from session store) to typed `ChatMessage`s.
///
/// Skips messages that don't have a valid `role` field, logging a warning.
/// Metadata fields (`created_at`, `model`, `provider`, `inputTokens`,
/// `outputTokens`, `channel`) are silently dropped — they only exist in
/// the persisted JSON, not in `ChatMessage`.
pub fn values_to_chat_messages(
    values: &[serde_json::Value],
) -> Result<Vec<ChatMessage>, ChatMessageConversionError> {
    values_to_chat_messages_inner(values, true)
}

/// Convert provider-format JSON messages to typed `ChatMessage`s without
/// dropping tool results.
///
/// Hook-modified LLM payloads are already provider-bound, so preserve their
/// tool messages exactly instead of applying session-store orphan filtering.
pub fn provider_values_to_chat_messages(
    values: &[serde_json::Value],
) -> Result<Vec<ChatMessage>, ChatMessageConversionError> {
    values_to_chat_messages_inner(values, false)
}

fn values_to_chat_messages_inner(
    values: &[serde_json::Value],
    filter_orphan_tool_results: bool,
) -> Result<Vec<ChatMessage>, ChatMessageConversionError> {
    // A summarization checkpoint replaces its summarized prefix while keeping
    // the unsummarized triggering tail in its original order. The tail remains
    // physically before the append-only checkpoint and starts at the absolute
    // `messagesSummarized` boundary.
    let latest_checkpoint = values
        .iter()
        .rposition(|val| val["role"].as_str() == Some("checkpoint"));
    let ordered_values: Vec<(usize, &serde_json::Value)> =
        if let Some(checkpoint_index) = latest_checkpoint {
            let tail_start = values[checkpoint_index]["messagesSummarized"]
                .as_u64()
                .and_then(|value| usize::try_from(value).ok())
                .filter(|start| *start <= checkpoint_index)
                .unwrap_or(checkpoint_index);
            std::iter::once((checkpoint_index, &values[checkpoint_index]))
                .chain(
                    values[tail_start..checkpoint_index]
                        .iter()
                        .enumerate()
                        .filter(|(_, value)| value["role"].as_str() != Some("checkpoint"))
                        .map(|(offset, value)| (tail_start + offset, value)),
                )
                .chain(
                    values[checkpoint_index + 1..]
                        .iter()
                        .enumerate()
                        .map(|(offset, value)| (checkpoint_index + 1 + offset, value)),
                )
                .collect()
        } else {
            values.iter().enumerate().collect()
        };
    let mut messages = Vec::with_capacity(ordered_values.len());
    // Track tool_call IDs emitted by assistant messages so we only include
    // tool/tool_result messages that have a matching assistant tool_call.
    // Orphan tool results would cause provider API errors.
    let mut pending_tool_call_ids: std::collections::HashSet<String> =
        std::collections::HashSet::new();
    for (i, val) in ordered_values {
        let Some(role) = val["role"].as_str() else {
            tracing::warn!(index = i, "skipping message with missing/invalid role");
            continue;
        };
        match role {
            "system" => {
                let content = val["content"].as_str().unwrap_or("").to_string();
                messages.push(ChatMessage::system(content));
            },
            "user" => {
                // Extract sender name from persisted channel metadata.
                let sender_name = val
                    .get("channel")
                    .and_then(|ch| {
                        ch["sender_name"]
                            .as_str()
                            .or_else(|| ch["username"].as_str())
                    })
                    .or_else(|| val["name"].as_str())
                    .map(|s| s.to_string());

                let document_context = val["documents"].as_array().and_then(|documents| {
                    let mut sections = Vec::new();
                    for document in documents {
                        let Some(display_name) = document["display_name"].as_str() else {
                            continue;
                        };
                        let Some(mime_type) = document["mime_type"].as_str() else {
                            continue;
                        };
                        let Some(media_ref) = document["media_ref"].as_str() else {
                            continue;
                        };
                        let absolute_path = document_absolute_path_from_media_ref(media_ref);
                        sections.push(format!(
                            "filename: {display_name}\nmime_type: {mime_type}\nlocal_path: {absolute_path}\nmedia_ref: {media_ref}"
                        ));
                    }
                    if sections.is_empty() {
                        None
                    } else {
                        let mut rendered = vec!["[Inbound documents available]".to_string()];
                        rendered.extend(sections);
                        Some(rendered.join("\n\n"))
                    }
                });

                // Content can be a string or an array (multimodal).
                if let Some(text) = val["content"].as_str() {
                    let content = if let Some(ref document_context) = document_context {
                        if text.trim().is_empty() {
                            document_context.clone()
                        } else {
                            format!("{text}\n\n{document_context}")
                        }
                    } else {
                        text.to_string()
                    };
                    messages.push(ChatMessage::User {
                        content: UserContent::Text(content),
                        name: sender_name,
                    });
                } else if let Some(arr) = val["content"].as_array() {
                    let mut parts: Vec<ContentPart> = arr
                        .iter()
                        .filter_map(|block| {
                            let block_type = block["type"].as_str()?;
                            match block_type {
                                "text" => {
                                    let text = block["text"].as_str()?.to_string();
                                    Some(ContentPart::Text(text))
                                },
                                "image_url" => {
                                    let url = block["image_url"]["url"].as_str()?;
                                    let (media_type, data) = parse_data_uri(url)?;
                                    Some(ContentPart::Image {
                                        media_type: media_type.to_string(),
                                        data: data.to_string(),
                                    })
                                },
                                _ => None,
                            }
                        })
                        .collect();
                    if let Some(document_context) = document_context {
                        if let Some(ContentPart::Text(text)) = parts
                            .iter_mut()
                            .find(|part| matches!(part, ContentPart::Text(_)))
                        {
                            if !text.trim().is_empty() {
                                text.push_str("\n\n");
                            }
                            text.push_str(&document_context);
                        } else {
                            parts.insert(0, ContentPart::Text(document_context));
                        }
                    }
                    messages.push(ChatMessage::User {
                        content: UserContent::Multimodal(parts),
                        name: sender_name,
                    });
                } else {
                    messages.push(ChatMessage::User {
                        content: UserContent::Text(document_context.unwrap_or_default()),
                        name: sender_name,
                    });
                }
            },
            "assistant" => {
                let content = val["content"].as_str().map(|s| s.to_string());
                let reasoning = decode_reasoning(val.get("reasoning"), i)?;
                let responses_reasoning = match val.get("responsesReasoning") {
                    None => Vec::new(),
                    Some(serde_json::Value::Array(items)) => items
                        .iter()
                        .enumerate()
                        .map(|(item_index, item)| {
                            serde_json::from_value::<chelix_common::ResponsesReasoningItem>(
                                item.clone(),
                            )
                            .map_err(|source| {
                                ChatMessageConversionError::ResponsesReasoningItem {
                                    message_index: i,
                                    item_index,
                                    source,
                                }
                            })
                        })
                        .collect::<Result<Vec<_>, _>>()?,
                    Some(_) => {
                        return Err(ChatMessageConversionError::ResponsesReasoningCollection {
                            message_index: i,
                        });
                    },
                };
                let tool_calls: Vec<ToolCall> = val["tool_calls"]
                    .as_array()
                    .map(|tcs| {
                        tcs.iter()
                            .filter_map(|tc| {
                                let id = tc["id"].as_str()?.to_string();
                                let name = tc["function"]["name"].as_str()?.to_string();
                                let decoded = decode_tool_call_arguments_with_diagnostic(
                                    tc["function"].get("arguments"),
                                );
                                Some(ToolCall {
                                    id,
                                    name,
                                    arguments: decoded.arguments,
                                    argument_diagnostic: decoded.diagnostic,
                                })
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                for tc in &tool_calls {
                    pending_tool_call_ids.insert(tc.id.clone());
                }
                messages.push(ChatMessage::Assistant {
                    content,
                    tool_calls,
                    reasoning,
                    responses_reasoning,
                });
            },
            "tool" => {
                let tool_call_id = val["tool_call_id"].as_str().unwrap_or("").to_string();
                let has_matching_assistant = pending_tool_call_ids.remove(&tool_call_id);
                if filter_orphan_tool_results && !has_matching_assistant {
                    tracing::debug!(tool_call_id, "skipping orphan tool message");
                    continue;
                }
                let content = if let Some(s) = val["content"].as_str() {
                    s.to_string()
                } else {
                    val["content"].to_string()
                };
                messages.push(ChatMessage::tool(tool_call_id, content));
            },
            "tool_lifecycle" => {
                let lifecycle = serde_json::from_value::<ToolLifecycleEvent>(val.clone()).map_err(
                    |source| ChatMessageConversionError::ToolLifecycle {
                        message_index: i,
                        source,
                    },
                )?;
                let content = match lifecycle.update {
                    ToolLifecycleUpdate::Completed { result, error, .. } => {
                        result.unwrap_or_else(|| {
                            error.map_or_else(String::new, |error| format!("Error: {error}"))
                        })
                    },
                    ToolLifecycleUpdate::Rejected { result, .. } => result,
                    ToolLifecycleUpdate::Cancelled { reason, .. } => {
                        format!("Tool call cancelled: {reason}")
                    },
                    _ => continue,
                };
                let has_matching_assistant = pending_tool_call_ids.remove(&lifecycle.tool_call_id);
                if filter_orphan_tool_results && !has_matching_assistant {
                    tracing::debug!(
                        tool_call_id = lifecycle.tool_call_id,
                        "skipping orphan terminal tool lifecycle message"
                    );
                    continue;
                }
                messages.push(ChatMessage::tool(lifecycle.tool_call_id, content));
            },
            // notice entries are UI-only informational messages.
            "notice" => continue,
            // checkpoint entries carry a conversation summary that replaces
            // all history before them (see context_start above).
            "checkpoint" => {
                let summary = val["summary"].as_str().unwrap_or("");
                messages.push(ChatMessage::user(format!(
                    "<conversation-summary>\n{summary}\n</conversation-summary>"
                )));
            },
            other => {
                tracing::warn!(
                    index = i,
                    role = other,
                    "skipping message with unknown role"
                );
            },
        }
    }
    Ok(messages)
}

#[cfg(test)]
mod checkpoint_tests {
    use super::*;

    #[test]
    fn latest_checkpoint_restores_its_pre_checkpoint_tail_only() {
        let values = vec![
            serde_json::json!({"role": "user", "content": "old"}),
            serde_json::json!({
                "role": "checkpoint",
                "summary": "first summary",
                "messagesSummarized": 1
            }),
            serde_json::json!({"role": "user", "content": "between"}),
            serde_json::json!({
                "role": "assistant",
                "content": "working",
                "responsesReasoning": [{
                    "id": "rs_checkpoint_tail",
                    "encryptedContent": "opaque-checkpoint-tail"
                }],
                "tool_calls": [{
                    "id": "call-2",
                    "function": {"name": "read_file", "arguments": "{}"}
                }]
            }),
            serde_json::json!({
                "role": "tool_lifecycle",
                "toolCallId": "call-2",
                "toolName": "read_file",
                "sequence": 1,
                "emittedAtMs": 1,
                "stage": "completed",
                "arguments": {},
                "success": true,
                "result": "latest result",
                "error": null
            }),
            serde_json::json!({
                "role": "checkpoint",
                "summary": "second summary",
                "messagesSummarized": 3
            }),
            serde_json::json!({"role": "user", "content": "after"}),
        ];

        let messages = values_to_chat_messages(&values)
            .unwrap_or_else(|error| panic!("valid checkpoint history: {error}"));

        assert_eq!(messages.len(), 4);
        assert!(matches!(
            &messages[0],
            ChatMessage::User { content: UserContent::Text(text), .. }
                if text.contains("second summary") && !text.contains("first summary")
        ));
        assert!(matches!(
            &messages[1],
            ChatMessage::Assistant {
                tool_calls,
                responses_reasoning,
                ..
            } if tool_calls[0].id == "call-2"
                && responses_reasoning.len() == 1
                && responses_reasoning[0].id == "rs_checkpoint_tail"
                && responses_reasoning[0].encrypted_content == "opaque-checkpoint-tail"
        ));
        assert!(
            matches!(&messages[2], ChatMessage::Tool { tool_call_id, .. } if tool_call_id == "call-2")
        );
        assert!(matches!(
            &messages[3],
            ChatMessage::User { content: UserContent::Text(text), .. } if text == "after"
        ));
    }
}
