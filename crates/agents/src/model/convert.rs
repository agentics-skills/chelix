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
    /// `providerItems` was present with a non-array value.
    #[error("assistant message at index {message_index} has non-array providerItems")]
    ProviderItemsCollection { message_index: usize },
    /// One provider output item could not be decoded.
    #[error(
        "assistant message at index {message_index} has invalid provider item at index {item_index}: {source}"
    )]
    ProviderItem {
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
    /// A persisted append-only provider item update could not be decoded.
    #[error("message at index {message_index} has invalid provider update: {source}")]
    ProviderUpdate {
        message_index: usize,
        #[source]
        source: serde_json::Error,
    },
    /// A persisted provider segment close record could not be decoded.
    #[error("message at index {message_index} has invalid provider segment close: {source}")]
    ProviderSegmentClose {
        message_index: usize,
        #[source]
        source: serde_json::Error,
    },
    /// Replaying the persisted provider records did not reproduce a valid segment.
    #[error("message at index {message_index} cannot be replayed into its segment: {source}")]
    ProviderSegmentReplay {
        message_index: usize,
        #[source]
        source: chelix_common::MaterializerError,
    },
    /// A message carries a role this conversion does not know how to handle.
    #[error("message at index {message_index} has unsupported role '{role}'")]
    UnsupportedRole { message_index: usize, role: String },
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
    let mut reconstruction =
        ChatReconstruction::new(filter_orphan_tool_results, assistant_segment_ids(values));
    for (index, value) in ordered_values {
        reconstruction.push(index, value)?;
    }
    reconstruction.finish()
}

/// Emit the assistant message of a finished segment at its position in history.
///
/// The materializer stays in the list after its message is emitted: it holds the
/// closed outcome, which is what rejects a later update claiming the same
/// segment. Removing it would silently reopen the segment instead.
fn flush_replayed_segment(
    messages: &mut Vec<ChatMessage>,
    segments: &[chelix_common::ProviderSegmentMaterializer],
    segment_id: &chelix_common::ProviderSegmentId,
    replayed: &mut std::collections::HashSet<chelix_common::ProviderSegmentId>,
) {
    let Some(index) = segments
        .iter()
        .position(|materializer| materializer.segment.segment_id.as_ref() == Some(segment_id))
    else {
        return;
    };
    let Some(message) = replayed_assistant_message(&segments[index], replayed) else {
        return;
    };
    messages.push(message);
    replayed.insert(segment_id.clone());
}

/// Segment identifiers already carried by a persisted assistant message.
///
/// Such a segment reaches the next provider request through that message, so
/// replaying its append-only records again would duplicate it.
fn assistant_segment_ids(
    values: &[serde_json::Value],
) -> std::collections::HashSet<chelix_common::ProviderSegmentId> {
    values
        .iter()
        .filter(|value| value["role"].as_str() == Some("assistant"))
        .filter_map(|value| value.get("segmentId"))
        .filter_map(|value| {
            serde_json::from_value::<chelix_common::ProviderSegmentId>(value.clone()).ok()
        })
        .collect()
}

/// Persisted shape of a `provider_segment_close` record.
#[derive(serde::Deserialize)]
struct PersistedSegmentClose {
    #[serde(rename = "segmentId")]
    segment_id: chelix_common::ProviderSegmentId,
    outcome: chelix_common::ProviderSegmentOutcome,
}

/// Look up the materializer of a replayed segment, creating it on first sight.
///
/// Segments keep the order in which the history first mentions them.
fn replayed_segment<'a>(
    segments: &'a mut Vec<chelix_common::ProviderSegmentMaterializer>,
    segment_id: &chelix_common::ProviderSegmentId,
) -> &'a mut chelix_common::ProviderSegmentMaterializer {
    let existing = segments
        .iter()
        .position(|materializer| materializer.segment.segment_id.as_ref() == Some(segment_id));
    let index = match existing {
        Some(index) => index,
        None => {
            segments.push(chelix_common::ProviderSegmentMaterializer::new(
                segment_id.clone(),
            ));
            segments.len() - 1
        },
    };
    &mut segments[index]
}

/// Turn replayed provider segments into assistant messages.
///
/// Every segment recorded in the history reaches the next provider request,
/// whatever its outcome: a retry or an interrupted run closes a segment but
/// never deletes what the provider already produced. Only a segment that a
/// persisted assistant message already carries is skipped, because that message
/// is the same segment in its final form.
fn append_replayed_provider_segments(
    messages: &mut Vec<ChatMessage>,
    segments: &[chelix_common::ProviderSegmentMaterializer],
    replayed: &mut std::collections::HashSet<chelix_common::ProviderSegmentId>,
) {
    for materializer in segments {
        let Some(message) = replayed_assistant_message(materializer, replayed) else {
            continue;
        };
        if let Some(id) = materializer.segment.segment_id.clone() {
            replayed.insert(id);
        }
        messages.push(message);
    }
}

/// Assistant message for a replayed segment, or `None` when it must not produce
/// one.
///
/// A segment is skipped when it produced nothing, or when it already reached the
/// conversation: either through a persisted assistant message, which is the same
/// segment in its final form, or because it was emitted when it closed.
fn replayed_assistant_message(
    materializer: &chelix_common::ProviderSegmentMaterializer,
    replayed: &std::collections::HashSet<chelix_common::ProviderSegmentId>,
) -> Option<ChatMessage> {
    if materializer.segment.items.is_empty() {
        return None;
    }
    if materializer
        .segment
        .segment_id
        .as_ref()
        .is_some_and(|id| replayed.contains(id))
    {
        return None;
    }
    // Function calls are carried by `tool_calls` as well: the Chat Completions
    // serializer reads that field, so a call left only in `provider_items` would
    // disappear from the next request.
    Some(ChatMessage::Assistant {
        content: materializer.segment.message_text(),
        tool_calls: replayed_tool_calls(&materializer.segment.items),
        reasoning: materializer.segment.reasoning_content(),
        provider_items: materializer.segment.items.clone(),
        segment_id: materializer.segment.segment_id.clone(),
    })
}

/// Tool calls of a replayed segment, in canonical item order.
///
/// Arguments are decoded through the same path as a live tool call, so a call
/// whose arguments never finished streaming keeps its decode diagnostic and is
/// rejected by the existing schema validation instead of silently passing as an
/// empty call.
fn replayed_tool_calls(items: &[chelix_common::ProviderOutputItem]) -> Vec<ToolCall> {
    items
        .iter()
        .filter_map(|item| match &item.payload {
            chelix_common::ProviderOutputPayload::FunctionCall {
                call_id,
                name,
                arguments,
            } => {
                let decoded = decode_tool_call_arguments_with_diagnostic(Some(
                    &serde_json::Value::String(arguments.clone()),
                ));
                Some(ToolCall {
                    id: call_id.clone(),
                    name: name.clone(),
                    arguments: decoded.arguments,
                    argument_diagnostic: decoded.diagnostic,
                })
            },
            _ => None,
        })
        .collect()
}

include!("chat_reconstruction.rs");

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
                "providerItems": [{
                    "id": "rs_checkpoint_tail",
                    "position": 0,
                    "payload": {
                        "type": "reasoning",
                        "id": "rs_checkpoint_tail",
                        "outputIndex": 0,
                        "summaryParts": [],
                        "encryptedContent": "opaque-checkpoint-tail"
                    }
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
                provider_items,
                ..
            } if tool_calls[0].id == "call-2"
                && provider_items.len() == 1
                && provider_items[0].id.as_str() == "rs_checkpoint_tail"
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

#[cfg(test)]
mod provider_segment_replay_tests {
    use super::*;

    fn update(segment: &str, item: &str, position: u32, seq: u64, text: &str) -> serde_json::Value {
        serde_json::json!({
            "role": "provider_update",
            "segmentId": segment,
            "itemId": item,
            "position": position,
            "updateSeq": seq,
            "payload": {"update_type": "message_done", "text": text}
        })
    }

    #[test]
    fn unclosed_segment_is_replayed_into_the_next_request() {
        let values = vec![
            serde_json::json!({"role": "user", "content": "hi"}),
            update("seg-1", "msg_0", 1, 1, "partial answer"),
            serde_json::json!({
                "role": "provider_update",
                "segmentId": "seg-1",
                "itemId": "rs_0",
                "position": 0,
                "updateSeq": 1,
                "payload": {
                    "update_type": "reasoning_part_done",
                    "part_index": 0,
                    "text": "thinking"
                }
            }),
        ];

        let messages = values_to_chat_messages(&values)
            .unwrap_or_else(|error| panic!("valid provider history: {error}"));

        assert_eq!(messages.len(), 2);
        let ChatMessage::Assistant {
            content,
            provider_items,
            segment_id,
            ..
        } = &messages[1]
        else {
            panic!("unclosed segment must be replayed as an assistant message");
        };
        assert_eq!(content.as_deref(), Some("partial answer"));
        assert_eq!(segment_id.as_ref().map(|id| id.0.as_str()), Some("seg-1"));
        // Items keep the provider positions assigned on ingress.
        assert_eq!(provider_items.len(), 2);
        assert_eq!(provider_items[0].id.as_str(), "rs_0");
        assert_eq!(provider_items[1].id.as_str(), "msg_0");
    }

    #[test]
    fn closed_segment_is_not_replayed() {
        let values = vec![
            serde_json::json!({"role": "user", "content": "hi"}),
            update("seg-1", "msg_0", 0, 1, "done"),
            serde_json::json!({
                "role": "provider_segment_close",
                "segmentId": "seg-1",
                "outcome": "completed"
            }),
            serde_json::json!({"role": "assistant", "content": "done", "segmentId": "seg-1"}),
        ];

        let messages = values_to_chat_messages(&values)
            .unwrap_or_else(|error| panic!("valid provider history: {error}"));

        assert_eq!(messages.len(), 2);
        assert!(matches!(
            &messages[1],
            ChatMessage::Assistant { content, .. } if content.as_deref() == Some("done")
        ));
    }

    #[test]
    fn retry_segments_are_replayed_separately_in_history_order() {
        let values = vec![
            serde_json::json!({"role": "user", "content": "hi"}),
            update("seg-1", "msg_0", 0, 1, "first attempt"),
            serde_json::json!({
                "role": "provider_segment_close",
                "segmentId": "seg-1",
                "outcome": "transport_error"
            }),
            update("seg-2", "msg_0", 0, 1, "second attempt"),
        ];

        let messages = values_to_chat_messages(&values)
            .unwrap_or_else(|error| panic!("valid provider history: {error}"));

        // A retry closes one segment and opens the next; it deletes nothing, so
        // both attempts reach the next provider request in history order.
        assert_eq!(messages.len(), 3);
        assert!(matches!(
            &messages[1],
            ChatMessage::Assistant { content, segment_id, .. }
                if content.as_deref() == Some("first attempt")
                    && segment_id.as_ref().map(|id| id.0.as_str()) == Some("seg-1")
        ));
        assert!(matches!(
            &messages[2],
            ChatMessage::Assistant { content, segment_id, .. }
                if content.as_deref() == Some("second attempt")
                    && segment_id.as_ref().map(|id| id.0.as_str()) == Some("seg-2")
        ));
    }

    #[test]
    fn a_failed_attempt_stays_before_the_turns_that_followed_it() {
        let values = vec![
            serde_json::json!({"role": "user", "content": "hi"}),
            update("seg-1", "msg_0", 0, 1, "cut off"),
            serde_json::json!({
                "role": "provider_segment_close",
                "segmentId": "seg-1",
                "outcome": "transport_error"
            }),
            serde_json::json!({"role": "user", "content": "again"}),
            serde_json::json!({"role": "assistant", "content": "answer"}),
        ];

        let messages = values_to_chat_messages(&values)
            .unwrap_or_else(|error| panic!("valid provider history: {error}"));

        // The failed attempt happened before the later turn, so it must stay
        // there: appending replayed segments at the end would reorder the
        // conversation the provider sees.
        let order: Vec<Option<&str>> = messages
            .iter()
            .map(|message| match message {
                ChatMessage::User {
                    content: UserContent::Text(text),
                    ..
                } => Some(text.as_str()),
                ChatMessage::Assistant { content, .. } => content.as_deref(),
                _ => None,
            })
            .collect();
        assert_eq!(order, vec![
            Some("hi"),
            Some("cut off"),
            Some("again"),
            Some("answer")
        ]);
    }

    #[test]
    fn an_interrupted_tool_call_is_replayed_with_an_aborted_result() {
        let values = vec![
            serde_json::json!({"role": "user", "content": "hi"}),
            serde_json::json!({
                "role": "provider_update",
                "segmentId": "seg-1",
                "itemId": "call_1",
                "position": 0,
                "updateSeq": 1,
                "payload": {"update_type": "function_call_start", "name": "search"}
            }),
            serde_json::json!({
                "role": "provider_segment_close",
                "segmentId": "seg-1",
                "outcome": "transport_error"
            }),
        ];

        let messages = values_to_chat_messages(&values)
            .unwrap_or_else(|error| panic!("valid provider history: {error}"));

        // The call survives the interruption, and it carries a result so the
        // next provider request stays valid.
        let ChatMessage::Assistant { tool_calls, .. } = &messages[1] else {
            panic!("expected the replayed segment to carry its function call");
        };
        assert_eq!(tool_calls.len(), 1);
        assert_eq!(tool_calls[0].id, "call_1");
        let ChatMessage::Tool {
            tool_call_id,
            content,
        } = &messages[2]
        else {
            panic!("expected an aborted result after the interrupted call");
        };
        assert_eq!(tool_call_id, "call_1");
        assert_eq!(content, "aborted");
    }

    #[test]
    fn segment_closed_by_a_transport_error_is_replayed_once() {
        // The stream was cut off after partial output: the segment is closed as
        // a transport error and the partial draft is persisted as an assistant
        // message carrying the same segment id. The result must appear once.
        let values = vec![
            serde_json::json!({"role": "user", "content": "hi"}),
            update("seg-1", "msg_0", 0, 1, "cut off"),
            serde_json::json!({
                "role": "provider_segment_close",
                "segmentId": "seg-1",
                "outcome": "transport_error"
            }),
            serde_json::json!({
                "role": "assistant",
                "content": "cut off",
                "segmentId": "seg-1"
            }),
        ];

        let messages = values_to_chat_messages(&values)
            .unwrap_or_else(|error| panic!("valid provider history: {error}"));

        assert_eq!(messages.len(), 2);
        assert!(matches!(
            &messages[1],
            ChatMessage::Assistant { content, .. } if content.as_deref() == Some("cut off")
        ));
    }

    #[test]
    fn update_after_close_is_rejected() {
        let values = vec![
            update("seg-1", "msg_0", 0, 1, "done"),
            serde_json::json!({
                "role": "provider_segment_close",
                "segmentId": "seg-1",
                "outcome": "completed"
            }),
            update("seg-1", "msg_0", 0, 2, "sneaked in"),
        ];

        let Err(error) = values_to_chat_messages(&values) else {
            panic!("a closed segment must reject further updates");
        };

        assert!(
            matches!(error, ChatMessageConversionError::ProviderSegmentReplay {
                message_index: 2,
                ..
            }),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn unknown_role_is_rejected() {
        let values = vec![serde_json::json!({"role": "mystery", "content": "x"})];

        let Err(error) = values_to_chat_messages(&values) else {
            panic!("unknown roles must fail");
        };

        assert!(
            matches!(
                error,
                ChatMessageConversionError::UnsupportedRole { message_index: 0, ref role }
                    if role == "mystery"
            ),
            "unexpected error: {error}"
        );
    }
}
