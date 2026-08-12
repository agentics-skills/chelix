//! Shared UI history filtering for JSONL session records.

use {chelix_common::ReasoningContent, serde_json::Value};

fn has_visible_reasoning(value: Option<&Value>) -> bool {
    value
        .and_then(|value| serde_json::from_value::<ReasoningContent>(value.clone()).ok())
        .is_some_and(|reasoning| !reasoning.is_blank())
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
pub fn filter_ui_history(messages: Vec<Value>) -> Vec<Value> {
    messages
        .into_iter()
        .enumerate()
        .filter_map(|(history_index, mut message)| {
            if message.get("role").and_then(Value::as_str) == Some("assistant") {
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
                if !(has_content || has_reasoning || has_audio || has_tool_calls) {
                    return None;
                }
            }
            redact_backend_only_provider_state(&mut message);
            if let Some(object) = message.as_object_mut() {
                object.insert("historyIndex".to_string(), serde_json::json!(history_index));
            }
            Some(message)
        })
        .collect()
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
        })]);

        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0]["historyIndex"], 0);
        assert_eq!(filtered[0]["tool_calls"][0]["id"], "tool-1");
    }

    #[test]
    fn removes_empty_assistant_frames_but_keeps_other_roles() {
        let filtered = filter_ui_history(vec![
            serde_json::json!({ "role": "assistant", "content": " \n " }),
            serde_json::json!({ "role": "tool", "content": "" }),
        ]);

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
        ]);

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
        })]);

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
