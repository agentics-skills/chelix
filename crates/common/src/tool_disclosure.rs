//! Tool-schema disclosures persisted with a canonical journal record.

use std::collections::BTreeSet;

use serde_json::Value;

use crate::tool_lifecycle::{ToolLifecycleEvent, ToolLifecycleUpdate};

/// Reserved control-plane meta-tool name. A user or MCP tool may not use it.
pub const GET_TOOL_NAME: &str = "get_tool";

/// Names and assistant segment disclosed by one canonical record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordDisclosure {
    pub names: BTreeSet<String>,
    pub segment_id: Option<String>,
}

/// Names revealed by one record, using the same rules as a history scan.
///
/// `get_tool` itself is not included. A `tool_lifecycle` record that cannot be
/// decoded returns the decode error.
pub fn record_disclosure(message: &Value) -> Result<RecordDisclosure, serde_json::Error> {
    let mut names = BTreeSet::new();
    match message.get("role").and_then(Value::as_str) {
        Some("assistant") => collect_direct_tool_calls(message, &mut names),
        Some("tool_lifecycle") => collect_get_tool_reveal(message, &mut names)?,
        _ => {},
    }
    let segment_id = (message.get("role").and_then(Value::as_str) == Some("assistant"))
        .then(|| message.get("segmentId"))
        .flatten()
        .and_then(|value| {
            serde_json::from_value::<crate::ProviderSegmentId>(value.clone())
                .ok()
                .map(|id| id.0)
        });
    Ok(RecordDisclosure { names, segment_id })
}

fn collect_direct_tool_calls(message: &Value, visible: &mut BTreeSet<String>) {
    let Some(tool_calls) = message.get("tool_calls").and_then(Value::as_array) else {
        return;
    };
    for tool_call in tool_calls {
        let Some(name) = tool_call
            .get("function")
            .and_then(|function| function.get("name"))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|name| !name.is_empty() && *name != GET_TOOL_NAME)
        else {
            continue;
        };
        visible.insert(name.to_string());
    }
}

fn collect_get_tool_reveal(
    message: &Value,
    visible: &mut BTreeSet<String>,
) -> Result<(), serde_json::Error> {
    let lifecycle = serde_json::from_value::<ToolLifecycleEvent>(message.clone())?;
    if lifecycle.tool_name != GET_TOOL_NAME {
        return Ok(());
    }
    let ToolLifecycleUpdate::Completed {
        success: true,
        result: Some(result),
        ..
    } = lifecycle.update
    else {
        return Ok(());
    };
    let result = serde_json::from_str::<Value>(&result)?;
    if result.get("schema_visible").and_then(Value::as_bool) != Some(true) {
        return Ok(());
    }
    if let Some(name) = result
        .get("name")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|name| !name.is_empty() && *name != GET_TOOL_NAME)
    {
        visible.insert(name.to_string());
    }
    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn successful_get_tool_reveal_keeps_the_name() {
        let message = serde_json::json!({
            "role": "tool_lifecycle",
            "toolCallId": "call-get-tool",
            "toolName": GET_TOOL_NAME,
            "sequence": 1,
            "emittedAtMs": 1,
            "stage": "completed",
            "arguments": {"name": "ripgrep"},
            "success": true,
            "result": r#"{"schema_visible":true,"name":"ripgrep"}"#,
            "error": null
        });

        let visible = record_disclosure(&message).unwrap();
        assert!(visible.names.contains("ripgrep"));
        assert!(!visible.names.contains(GET_TOOL_NAME));
    }

    #[test]
    fn failed_get_tool_reveal_is_ignored() {
        let message = serde_json::json!({
            "role": "tool_lifecycle",
            "toolCallId": "call-get-tool-failed",
            "toolName": GET_TOOL_NAME,
            "sequence": 1,
            "emittedAtMs": 1,
            "stage": "completed",
            "arguments": {"name": "ripgrep"},
            "success": false,
            "result": r#"{"schema_visible":true,"name":"ripgrep"}"#,
            "error": null
        });

        assert!(
            !record_disclosure(&message)
                .unwrap()
                .names
                .contains("ripgrep")
        );
    }

    #[test]
    fn schema_not_visible_reveal_is_ignored() {
        let message = serde_json::json!({
            "role": "tool_lifecycle",
            "toolCallId": "call-get-tool",
            "toolName": GET_TOOL_NAME,
            "sequence": 1,
            "emittedAtMs": 1,
            "stage": "completed",
            "arguments": {"name": "ripgrep"},
            "success": true,
            "result": r#"{"schema_visible":false,"name":"ripgrep"}"#,
            "error": null
        });

        assert!(
            !record_disclosure(&message)
                .unwrap()
                .names
                .contains("ripgrep")
        );
    }

    #[test]
    fn direct_tool_call_is_disclosed() {
        let message = serde_json::json!({
            "role": "assistant",
            "tool_calls": [{
                "id": "call_1",
                "type": "function",
                "function": {
                    "name": "ripgrep",
                    "arguments": "{\"pattern\":\"**/*.rs\"}"
                }
            }]
        });

        assert!(
            record_disclosure(&message)
                .unwrap()
                .names
                .contains("ripgrep")
        );
    }
}
