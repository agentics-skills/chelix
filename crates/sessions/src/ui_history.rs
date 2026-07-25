//! Shared UI history filtering for JSONL session records.

use serde_json::Value;

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
                let has_reasoning = message
                    .get("reasoning")
                    .and_then(Value::as_str)
                    .is_some_and(|reasoning| !reasoning.trim().is_empty());
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
    use super::filter_ui_history;

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
            serde_json::json!({ "role": "assistant", "audio": "media/reply.ogg" }),
        ]);

        assert_eq!(filtered.len(), 2);
        assert_eq!(filtered[0]["historyIndex"], 0);
        assert_eq!(filtered[1]["historyIndex"], 1);
    }
}
