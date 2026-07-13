use crate::multimodal::parse_data_uri;

use super::{
    chat::{ChatMessage, ContentPart, UserContent},
    decode_tool_call_arguments_with_diagnostic,
    types::{TOOL_CALL_METADATA_KEYS, ToolCall},
};

/// Extract allowlisted metadata keys from a tool-call JSON object.
///
/// Returns `None` when no metadata keys are present, keeping the common path
/// allocation-free.
#[must_use]
pub fn extract_tool_call_metadata(
    tc: &serde_json::Value,
) -> Option<serde_json::Map<String, serde_json::Value>> {
    let obj = tc.as_object()?;
    let nested = obj.get("metadata").and_then(serde_json::Value::as_object);
    let gemini = obj
        .get("extra_content")
        .and_then(|extra| extra.get("google"))
        .and_then(serde_json::Value::as_object);
    let meta: serde_json::Map<_, _> = TOOL_CALL_METADATA_KEYS
        .iter()
        .filter_map(|&k| {
            obj.get(k)
                .or_else(|| nested.and_then(|metadata| metadata.get(k)))
                .or_else(|| gemini.and_then(|metadata| metadata.get(k)))
                .map(|v| (k.to_string(), v.clone()))
        })
        .collect();
    if meta.is_empty() {
        None
    } else {
        Some(meta)
    }
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
pub fn values_to_chat_messages(values: &[serde_json::Value]) -> Vec<ChatMessage> {
    values_to_chat_messages_inner(values, true)
}

/// Convert provider-format JSON messages to typed `ChatMessage`s without
/// dropping tool results.
///
/// Hook-modified LLM payloads are already provider-bound, so preserve their
/// tool messages exactly instead of applying session-store orphan filtering.
pub fn provider_values_to_chat_messages(values: &[serde_json::Value]) -> Vec<ChatMessage> {
    values_to_chat_messages_inner(values, false)
}

fn values_to_chat_messages_inner(
    values: &[serde_json::Value],
    filter_orphan_tool_results: bool,
) -> Vec<ChatMessage> {
    // A summarization checkpoint replaces its summarized prefix while keeping
    // the unsummarized triggering tail in its original order. The tail remains
    // physically before the append-only checkpoint and starts at the absolute
    // `messagesSummarized` boundary.
    let latest_checkpoint = values
        .iter()
        .rposition(|val| val["role"].as_str() == Some("checkpoint"));
    let ordered_values: Vec<&serde_json::Value> = if let Some(checkpoint_index) = latest_checkpoint
    {
        let tail_start = values[checkpoint_index]["messagesSummarized"]
            .as_u64()
            .and_then(|value| usize::try_from(value).ok())
            .filter(|start| *start <= checkpoint_index)
            .unwrap_or(checkpoint_index);
        std::iter::once(&values[checkpoint_index])
            .chain(
                values[tail_start..checkpoint_index]
                    .iter()
                    .filter(|value| value["role"].as_str() != Some("checkpoint")),
            )
            .chain(values[checkpoint_index + 1..].iter())
            .collect()
    } else {
        values.iter().collect()
    };
    let mut messages = Vec::with_capacity(ordered_values.len());
    // Track tool_call IDs emitted by assistant messages so we only include
    // tool/tool_result messages that have a matching assistant tool_call.
    // Orphan tool results (e.g. from explicit /sh commands) would cause
    // provider API errors.
    let mut pending_tool_call_ids: std::collections::HashSet<String> =
        std::collections::HashSet::new();
    for (i, val) in ordered_values.into_iter().enumerate() {
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
                let reasoning = val["reasoning"].as_str().and_then(|s| {
                    let trimmed = s.trim();
                    (!trimmed.is_empty()).then(|| trimmed.to_string())
                });
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
                                let metadata = extract_tool_call_metadata(tc);
                                Some(ToolCall {
                                    id,
                                    name,
                                    arguments: decoded.arguments,
                                    argument_diagnostic: decoded.diagnostic,
                                    metadata,
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
            // tool_result entries are persisted tool execution output; convert
            // them to standard tool messages so the LLM sees its own results.
            "tool_result" => {
                let tool_call_id = val["tool_call_id"].as_str().unwrap_or("").to_string();
                let has_matching_assistant = pending_tool_call_ids.remove(&tool_call_id);
                if filter_orphan_tool_results && !has_matching_assistant {
                    tracing::debug!(tool_call_id, "skipping orphan tool_result message");
                    continue;
                }
                if let Some(reasoning) = val["reasoning"].as_str().and_then(|s| {
                    let trimmed = s.trim();
                    (!trimmed.is_empty()).then(|| trimmed.to_string())
                }) {
                    attach_reasoning_to_assistant_tool_call(
                        &mut messages,
                        &tool_call_id,
                        reasoning,
                    );
                }
                let content = if let Some(err) = val["error"].as_str() {
                    format!("Error: {err}")
                } else if let Some(result) = val.get("result") {
                    if let Some(s) = result.as_str() {
                        s.to_string()
                    } else {
                        result.to_string()
                    }
                } else {
                    String::new()
                };
                messages.push(ChatMessage::tool(tool_call_id, content));
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
    messages
}

fn attach_reasoning_to_assistant_tool_call(
    messages: &mut [ChatMessage],
    tool_call_id: &str,
    tool_reasoning: String,
) {
    for message in messages.iter_mut().rev() {
        let ChatMessage::Assistant {
            tool_calls,
            reasoning,
            ..
        } = message
        else {
            continue;
        };

        if tool_calls
            .iter()
            .any(|tool_call| tool_call.id == tool_call_id)
        {
            if reasoning.is_none() {
                *reasoning = Some(tool_reasoning);
            }
            return;
        }
    }
    tracing::debug!(
        tool_call_id,
        "no assistant message found for reasoning attachment"
    );
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
                "tool_calls": [{
                    "id": "call-2",
                    "function": {"name": "read_file", "arguments": "{}"}
                }]
            }),
            serde_json::json!({
                "role": "tool_result",
                "tool_call_id": "call-2",
                "tool_name": "read_file",
                "success": true,
                "result": "latest result"
            }),
            serde_json::json!({
                "role": "checkpoint",
                "summary": "second summary",
                "messagesSummarized": 3
            }),
            serde_json::json!({"role": "user", "content": "after"}),
        ];

        let messages = values_to_chat_messages(&values);

        assert_eq!(messages.len(), 4);
        assert!(matches!(
            &messages[0],
            ChatMessage::User { content: UserContent::Text(text), .. }
                if text.contains("second summary") && !text.contains("first summary")
        ));
        assert!(matches!(
            &messages[1],
            ChatMessage::Assistant { tool_calls, .. } if tool_calls[0].id == "call-2"
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
