//! Shared UI history filtering for JSONL session records.

use std::collections::{HashMap, HashSet};

use {
    chelix_common::{ReasoningContent, tool_lifecycle::ToolLifecycleEvent},
    serde_json::Value,
};

use crate::Result;

fn has_visible_reasoning(value: Option<&Value>) -> bool {
    value
        .and_then(|value| serde_json::from_value::<ReasoningContent>(value.clone()).ok())
        .is_some_and(|reasoning| !reasoning.is_blank())
}

/// Segment a record belongs to.
fn segment_id(message: &Value) -> Option<&str> {
    message.get("segmentId").and_then(Value::as_str)
}

/// Whether an assistant frame renders a bubble of its own.
///
/// A frame kept only for its `tool_calls` carries the identity and terminal
/// metadata of the tool results; the UI attaches those to the tool cards and
/// renders no message for the frame itself.
fn assistant_renders_bubble(message: &Value) -> bool {
    let has_content = message
        .get("content")
        .and_then(Value::as_str)
        .is_some_and(|content| !content.trim().is_empty());
    let has_audio = message
        .get("audio")
        .and_then(Value::as_str)
        .is_some_and(|audio| !audio.trim().is_empty());
    has_content || has_audio || has_visible_reasoning(message.get("reasoning"))
}

/// Whether each record of `history` renders a bubble of its own.
///
/// This is the single definition of the unit that message counts and page
/// limits are expressed in, so it follows the renderer exactly. It cannot be
/// decided per record in isolation: a provider segment renders one bubble no
/// matter how many records it spans, and renders none once an assistant message
/// carries that segment in its final form.
///
/// Roles the renderer has no branch for produce no DOM and count as nothing, so
/// a count never promises a bubble that does not exist.
#[must_use]
pub fn rendered_bubble_flags(history: &[Value]) -> Vec<bool> {
    let assistant_segments: HashSet<&str> = history
        .iter()
        .filter(|message| message.get("role").and_then(Value::as_str) == Some("assistant"))
        .filter_map(segment_id)
        .collect();

    let mut rendered_segments: HashSet<&str> = HashSet::new();
    history
        .iter()
        .map(
            |message| match message.get("role").and_then(Value::as_str) {
                Some("user" | "system" | "notice" | "checkpoint" | "tool_lifecycle") => true,
                Some("assistant") => assistant_renders_bubble(message),
                Some("provider_update") => segment_id(message).is_some_and(|id| {
                    !assistant_segments.contains(id) && rendered_segments.insert(id)
                }),
                _ => false,
            },
        )
        .collect()
}

/// Number of bubbles `history` renders as.
#[must_use]
pub fn count_rendered_bubbles(history: &[Value]) -> usize {
    rendered_bubble_flags(history)
        .into_iter()
        .filter(|rendered| *rendered)
        .count()
}

/// Remove provider replay state before a value crosses a UI or API boundary.
pub fn redact_backend_only_provider_state(value: &mut Value) {
    match value {
        Value::Object(object) => {
            object.remove("responsesReasoning");
            object.remove("encrypted_content");
            object.remove("encryptedContent");
            object
                .values_mut()
                .for_each(redact_backend_only_provider_state);
        },
        Value::Array(items) => items
            .iter_mut()
            .for_each(redact_backend_only_provider_state),
        _ => {},
    }
}

/// Filter persisted history for UI delivery while preserving physical indexes.
///
/// Empty assistant frames are required by LLM history coherence but are not
/// visible UI content. Assistant tool-call frames are retained because they
/// provide the canonical identity and terminal metadata for tool results.
pub fn filter_ui_history(messages: Vec<Value>) -> Result<Vec<Value>> {
    let mut last_lifecycle_index = HashMap::new();
    for (history_index, message) in messages.iter().enumerate() {
        if message.get("role").and_then(Value::as_str) != Some("tool_lifecycle") {
            continue;
        }
        let lifecycle = serde_json::from_value::<ToolLifecycleEvent>(message.clone())?;
        last_lifecycle_index.insert(
            (lifecycle.run_id.clone(), lifecycle.tool_call_id),
            history_index,
        );
    }

    let mut filtered = Vec::with_capacity(messages.len());
    for (history_index, mut message) in messages.into_iter().enumerate() {
        match message.get("role").and_then(Value::as_str) {
            Some("assistant") => {
                let has_content = message
                    .get("content")
                    .and_then(Value::as_str)
                    .is_some_and(|content| !content.trim().is_empty());
                let has_reasoning = has_visible_reasoning(message.get("reasoning"));
                let has_audio = message
                    .get("audio")
                    .and_then(Value::as_str)
                    .is_some_and(|audio| !audio.trim().is_empty());
                let has_tool_calls = message
                    .get("tool_calls")
                    .and_then(Value::as_array)
                    .is_some_and(|tool_calls| !tool_calls.is_empty());
                let has_provider_items = message
                    .get("providerItems")
                    .and_then(Value::as_array)
                    .is_some_and(|items| !items.is_empty());
                if !(has_content
                    || has_reasoning
                    || has_audio
                    || has_tool_calls
                    || has_provider_items)
                {
                    continue;
                }
            },
            Some("tool_lifecycle") => {
                let lifecycle = serde_json::from_value::<ToolLifecycleEvent>(message.clone())?;
                let identity = (lifecycle.run_id, lifecycle.tool_call_id);
                if last_lifecycle_index.get(&identity) != Some(&history_index) {
                    continue;
                }
            },
            _ => {},
        }
        redact_backend_only_provider_state(&mut message);
        if let Some(object) = message.as_object_mut() {
            object.insert("historyIndex".to_string(), serde_json::json!(history_index));
        }
        filtered.push(message);
    }
    Ok(filtered)
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::{filter_ui_history, redact_backend_only_provider_state};

    #[test]
    fn keeps_empty_assistant_tool_frames_with_physical_history_index() {
        let filtered = filter_ui_history(vec![serde_json::json!({
            "role": "assistant",
            "content": "",
            "tool_calls": [{ "id": "tool-1", "function": { "name": "execute_command" } }],
        })])
        .unwrap();

        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0]["historyIndex"], 0);
        assert_eq!(filtered[0]["tool_calls"][0]["id"], "tool-1");
    }

    #[test]
    fn removes_empty_assistant_frames_but_keeps_other_roles() {
        let filtered = filter_ui_history(vec![
            serde_json::json!({ "role": "assistant", "content": " \n " }),
            serde_json::json!({ "role": "tool", "content": "" }),
        ])
        .unwrap();

        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0]["historyIndex"], 1);
        assert_eq!(filtered[0]["role"], "tool");
    }

    #[test]
    fn keeps_reasoning_and_audio_only_assistant_frames() {
        let filtered = filter_ui_history(vec![
            serde_json::json!({ "role": "assistant", "reasoning": "plan" }),
            serde_json::json!({
                "role": "assistant",
                "reasoning": [
                    "**Analyzing sources**\nReviewing the request.",
                    "**Checking evidence**\nComparing the results."
                ]
            }),
            serde_json::json!({ "role": "assistant", "audio": "media/reply.ogg" }),
        ])
        .unwrap();

        assert_eq!(filtered.len(), 3);
        assert_eq!(filtered[0]["historyIndex"], 0);
        assert_eq!(
            filtered[1]["reasoning"],
            serde_json::json!([
                "**Analyzing sources**\nReviewing the request.",
                "**Checking evidence**\nComparing the results."
            ])
        );
        assert_eq!(filtered[1]["historyIndex"], 1);
        assert_eq!(filtered[2]["historyIndex"], 2);
    }

    #[test]
    fn redacts_opaque_responses_state_from_ui_history() {
        let filtered = filter_ui_history(vec![serde_json::json!({
            "role": "assistant",
            "content": "answer",
            "reasoning": "visible reasoning",
            "responsesReasoning": [{
                "id": "rs_123",
                "encryptedContent": "opaque-state"
            }],
            "llmApiResponse": [{
                "type": "response.output_item.done",
                "item": {
                    "type": "reasoning",
                    "id": "rs_123",
                    "summary": [{"type": "summary_text", "text": "visible reasoning"}],
                    "encrypted_content": "opaque-state"
                }
            }]
        })])
        .unwrap();

        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0]["reasoning"], "visible reasoning");
        assert!(filtered[0].get("responsesReasoning").is_none());
        assert!(
            filtered[0]["llmApiResponse"][0]["item"]
                .get("encrypted_content")
                .is_none()
        );
        assert_eq!(filtered[0]["llmApiResponse"][0]["item"]["id"], "rs_123");
    }

    #[test]
    fn keeps_only_the_latest_snapshot_for_each_tool_invocation() {
        let filtered = filter_ui_history(vec![
            serde_json::json!({
                "role": "user",
                "content": "run it"
            }),
            serde_json::json!({
                "role": "tool_lifecycle",
                "toolCallId": "call-1",
                "toolName": "overwrite_file",
                "sequence": 0,
                "emittedAtMs": 1,
                "runId": "run-1",
                "stage": "created",
                "providerIndex": 0
            }),
            serde_json::json!({
                "role": "tool_lifecycle",
                "toolCallId": "call-1",
                "toolName": "overwrite_file",
                "sequence": 8,
                "emittedAtMs": 2,
                "runId": "run-1",
                "stage": "completed",
                "arguments": {"filePath": "/tmp/report"},
                "success": true,
                "result": "{\"ok\":true}",
                "error": null
            }),
        ])
        .unwrap();

        assert_eq!(filtered.len(), 2);
        assert_eq!(filtered[0]["role"], "user");
        assert_eq!(filtered[1]["stage"], "completed");
        assert_eq!(filtered[1]["historyIndex"], 2);
    }

    #[test]
    fn redacts_nested_backend_state_from_live_payload_shape() {
        let mut payload = serde_json::json!({
            "assistantMessage": {
                "responsesReasoning": [{
                    "id": "rs_live",
                    "encryptedContent": "opaque-live"
                }]
            },
            "partialMessage": {
                "llmApiResponse": [{
                    "item": {
                        "id": "rs_live",
                        "encrypted_content": "opaque-live"
                    }
                }]
            }
        });

        redact_backend_only_provider_state(&mut payload);

        assert!(
            payload["assistantMessage"]
                .get("responsesReasoning")
                .is_none()
        );
        assert!(
            payload["partialMessage"]["llmApiResponse"][0]["item"]
                .get("encrypted_content")
                .is_none()
        );
        assert_eq!(
            payload["partialMessage"]["llmApiResponse"][0]["item"]["id"],
            "rs_live"
        );
    }
}
