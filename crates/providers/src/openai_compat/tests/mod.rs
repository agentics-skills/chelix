mod schema_normalization;

use super::{
    ResponsesSseLineResult, ResponsesStreamState, finalize_responses_stream,
    normalize_tool_call_arguments_from_schemas, parse_responses_completion, parse_tool_calls,
    process_responses_sse_line,
};

use chelix_agents::model::StreamEvent;

#[test]
fn parse_tool_calls_preserve_issue_693_examples() {
    let msg = serde_json::json!({
        "tool_calls": [
            {
                "id": "call_execute_command",
                "function": {
                    "name": "execute_command",
                    "arguments": {
                        "command": "echo hello",
                        "timeout": 0
                    }
                }
            },
            {
                "id": "call_edit",
                "function": {
                    "name": "Edit",
                    "arguments": {
                        "replace_all": false
                    }
                }
            }
        ]
    });

    let calls = parse_tool_calls(&msg);

    assert_eq!(calls.len(), 2);
    assert_eq!(calls[0].arguments["timeout"], 0);
    assert_eq!(calls[1].arguments["replace_all"], false);
}

#[test]
fn normalize_nullable_enum_none_sentinel_to_null() {
    let tools = vec![serde_json::json!({
        "name": "mcp_tavily_search",
        "description": "Search",
        "parameters": {
            "type": "object",
            "properties": {
                "query": { "type": "string" },
                "time_range": {
                    "type": "string",
                    "enum": ["day", "week", "month", "year"]
                },
                "country": {
                    "type": "string",
                    "enum": ["US", "UK", "FR", "DE"]
                }
            },
            "required": ["query"]
        }
    })];
    let mut tool_calls = vec![chelix_agents::model::ToolCall {
        id: "call_1".to_string(),
        name: "mcp_tavily_search".to_string(),
        arguments: serde_json::json!({
            "query": "rust async",
            "time_range": "None",
            "country": "None"
        }),
        argument_diagnostic: None,
        metadata: None,
    }];

    normalize_tool_call_arguments_from_schemas(&mut tool_calls, &tools);

    assert_eq!(tool_calls[0].arguments["query"], "rust async");
    assert!(tool_calls[0].arguments["time_range"].is_null());
    assert!(tool_calls[0].arguments["country"].is_null());
}

#[test]
fn normalize_preserves_literal_none_enum_value() {
    let tools = vec![serde_json::json!({
        "name": "set_mode",
        "parameters": {
            "type": "object",
            "properties": {
                "mode": { "type": "string", "enum": ["None", "fast"] }
            }
        }
    })];
    let mut tool_calls = vec![chelix_agents::model::ToolCall {
        id: "call_1".to_string(),
        name: "set_mode".to_string(),
        arguments: serde_json::json!({ "mode": "None" }),
        argument_diagnostic: None,
        metadata: None,
    }];

    normalize_tool_call_arguments_from_schemas(&mut tool_calls, &tools);

    assert_eq!(tool_calls[0].arguments["mode"], "None");
}

#[test]
fn normalize_null_only_empty_string_to_null() {
    let tools = vec![serde_json::json!({
        "name": "serialization_probe",
        "parameters": {
            "type": "object",
            "properties": {
                "kind": { "type": "null" },
                "label": { "type": "string" }
            },
            "required": ["kind", "label"]
        }
    })];
    let mut tool_calls = vec![chelix_agents::model::ToolCall {
        id: "call_1".to_string(),
        name: "serialization_probe".to_string(),
        arguments: serde_json::json!({ "kind": "", "label": "" }),
        argument_diagnostic: None,
        metadata: None,
    }];

    normalize_tool_call_arguments_from_schemas(&mut tool_calls, &tools);

    assert!(tool_calls[0].arguments["kind"].is_null());
    assert_eq!(tool_calls[0].arguments["label"], "");
}

#[test]
fn normalize_array_schema_decodes_json_string_array() {
    let tools = vec![serde_json::json!({
        "name": "spawn_agent",
        "parameters": {
            "type": "object",
            "properties": {
                "task": { "type": "string" },
                "allow_tools": {
                    "type": "array",
                    "items": { "type": "string" }
                }
            },
            "required": ["task"]
        }
    })];
    let mut tool_calls = vec![chelix_agents::model::ToolCall {
        id: "call_1".to_string(),
        name: "spawn_agent".to_string(),
        arguments: serde_json::json!({
            "task": "list files",
            "allow_tools": "[\"list_directory\",\"read_file\"]"
        }),
        argument_diagnostic: None,
        metadata: None,
    }];

    normalize_tool_call_arguments_from_schemas(&mut tool_calls, &tools);

    assert_eq!(
        tool_calls[0].arguments["allow_tools"],
        serde_json::json!(["list_directory", "read_file"])
    );
}

#[test]
fn parse_responses_completion_preserves_native_falsy_types() {
    let resp = serde_json::json!({
        "output": [{
            "type": "function_call",
            "call_id": "call_abc",
            "name": "grep",
            "arguments": {
                "offset": 0,
                "multiline": false,
                "type": null
            }
        }],
        "usage": {"input_tokens": 20, "output_tokens": 10}
    });

    let result = parse_responses_completion(&resp);

    assert_eq!(result.tool_calls.len(), 1);
    assert_eq!(result.tool_calls[0].arguments["offset"], 0);
    assert_eq!(result.tool_calls[0].arguments["multiline"], false);
    assert!(result.tool_calls[0].arguments["type"].is_null());
}

#[test]
fn responses_completed_emits_raw_event_and_final_usage() {
    let event = serde_json::json!({
        "type": "response.completed",
        "response": {
            "usage": {
                "input_tokens": 12,
                "output_tokens": 7,
                "input_tokens_details": { "cached_tokens": 3 }
            }
        }
    });
    let mut state = ResponsesStreamState::default();

    let result = process_responses_sse_line(&event.to_string(), &mut state);

    assert!(matches!(
        result,
        ResponsesSseLineResult::Completed(events)
            if matches!(events.as_slice(), [StreamEvent::ProviderRaw(raw)] if raw == &event)
    ));

    let finalized = finalize_responses_stream(&mut state);
    assert!(matches!(
        finalized.as_slice(),
        [StreamEvent::Done(usage)]
            if usage.input_tokens == 12
                && usage.output_tokens == 7
                && usage.cache_read_tokens == 3
    ));
}

#[test]
fn responses_done_sentinel_is_successful_terminal_event() {
    let mut state = ResponsesStreamState::default();

    let result = process_responses_sse_line("[DONE]", &mut state);

    assert!(matches!(
        result,
        ResponsesSseLineResult::Completed(events) if events.is_empty()
    ));
}

#[test]
fn responses_failed_is_terminal_error() {
    let event = serde_json::json!({
        "type": "response.failed",
        "response": { "error": { "message": "upstream failed" } }
    });
    let mut state = ResponsesStreamState::default();

    let result = process_responses_sse_line(&event.to_string(), &mut state);

    assert!(matches!(
        result,
        ResponsesSseLineResult::Failed(events)
            if matches!(
                events.as_slice(),
                [StreamEvent::ProviderRaw(raw), StreamEvent::Error(message)]
                    if raw == &event && message == "upstream failed"
            )
    ));
}

#[test]
fn responses_incomplete_is_terminal_error() {
    let event = serde_json::json!({
        "type": "response.incomplete",
        "response": { "incomplete_details": { "reason": "max_output_tokens" } }
    });
    let mut state = ResponsesStreamState::default();

    let result = process_responses_sse_line(&event.to_string(), &mut state);

    assert!(matches!(
        result,
        ResponsesSseLineResult::Failed(events)
            if matches!(
                events.as_slice(),
                [StreamEvent::ProviderRaw(raw), StreamEvent::Error(message)]
                    if raw == &event && message == "response incomplete: max_output_tokens"
            )
    ));
}

#[test]
fn responses_error_event_is_terminal_error() {
    let event = serde_json::json!({
        "type": "error",
        "error": { "message": "request rejected" }
    });
    let mut state = ResponsesStreamState::default();

    let result = process_responses_sse_line(&event.to_string(), &mut state);

    assert!(matches!(
        result,
        ResponsesSseLineResult::Failed(events)
            if matches!(
                events.as_slice(),
                [StreamEvent::ProviderRaw(raw), StreamEvent::Error(message)]
                    if raw == &event && message == "request rejected"
            )
    ));
}
