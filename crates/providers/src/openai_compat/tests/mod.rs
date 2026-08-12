mod schema_normalization;

use super::{
    ResponsesEventResult, ResponsesStreamState, finalize_responses_stream,
    normalize_tool_call_arguments_from_schemas, parse_responses_completion, parse_tool_calls,
    process_responses_event, process_responses_sse_line, to_responses_input,
};

use chelix_agents::model::{ChatMessage, StreamEvent, ToolCall};

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
                    "name": "sample_tool",
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
    let mut tool_calls = vec![ToolCall {
        id: "call_1".to_string(),
        name: "mcp_tavily_search".to_string(),
        arguments: serde_json::json!({
            "query": "rust async",
            "time_range": "None",
            "country": "None"
        }),
        argument_diagnostic: None,
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
    let mut tool_calls = vec![ToolCall {
        id: "call_1".to_string(),
        name: "set_mode".to_string(),
        arguments: serde_json::json!({ "mode": "None" }),
        argument_diagnostic: None,
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
    let mut tool_calls = vec![ToolCall {
        id: "call_1".to_string(),
        name: "serialization_probe".to_string(),
        arguments: serde_json::json!({ "kind": "", "label": "" }),
        argument_diagnostic: None,
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
    let mut tool_calls = vec![ToolCall {
        id: "call_1".to_string(),
        name: "spawn_agent".to_string(),
        arguments: serde_json::json!({
            "task": "list files",
            "allow_tools": "[\"list_directory\",\"read_file\"]"
        }),
        argument_diagnostic: None,
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
fn responses_replay_places_provider_reasoning_before_assistant_output() {
    let message = ChatMessage::assistant_with_tools(Some("answer".to_string()), vec![ToolCall {
        id: "call_1".to_string(),
        name: "lookup".to_string(),
        arguments: serde_json::json!({"query": "rust"}),
        argument_diagnostic: None,
    }])
    .with_responses_reasoning(vec![
        chelix_common::ResponsesReasoningItem {
            id: "rs_123".to_string(),
            encrypted_content: "opaque-state".to_string(),
        },
        chelix_common::ResponsesReasoningItem {
            id: "invalid_123".to_string(),
            encrypted_content: "must-not-replay".to_string(),
        },
    ]);

    let input = to_responses_input(&[message]);

    assert_eq!(input.len(), 3);
    assert_eq!(
        input[0],
        serde_json::json!({
            "type": "reasoning",
            "id": "rs_123",
            "summary": [],
            "encrypted_content": "opaque-state",
        })
    );
    assert_eq!(input[1]["type"], "message");
    assert_eq!(input[2]["type"], "function_call");
}

#[test]
fn responses_streamed_summary_is_not_duplicated_by_final_reasoning_item() {
    let mut state = ResponsesStreamState::default();
    let delta = serde_json::json!({
        "type": "response.reasoning_summary_text.delta",
        "item_id": "rs_123",
        "output_index": 0,
        "summary_index": 0,
        "delta": "Checked the request."
    });

    let streamed = process_responses_event(delta.clone(), &mut state);
    assert!(matches!(
        streamed,
        ResponsesEventResult::Events(events)
            if matches!(
                events.as_slice(),
                [
                    StreamEvent::ProviderRaw(raw),
                    StreamEvent::ResponsesReasoningDelta {
                        item_id,
                        output_index,
                        summary_index,
                        delta: text,
                    }
                ] if raw == &delta
                    && item_id == "rs_123"
                    && *output_index == 0
                    && *summary_index == 0
                    && text == "Checked the request."
            )
    ));

    let done = serde_json::json!({
        "type": "response.output_item.done",
        "output_index": 0,
        "item": {
            "type": "reasoning",
            "id": "rs_123",
            "summary": [{"type": "summary_text", "text": "Checked the request."}],
            "encrypted_content": "opaque-state"
        }
    });
    let finalized = process_responses_event(done.clone(), &mut state);

    assert!(matches!(
        finalized,
        ResponsesEventResult::Events(events)
            if matches!(
                events.as_slice(),
                [
                    StreamEvent::ProviderRaw(raw),
                    StreamEvent::ResponsesReasoningItem(item)
                ] if raw == &done
                    && item.id == "rs_123"
                    && item.encrypted_content == "opaque-state"
            )
    ));
}

#[test]
fn responses_final_item_emits_only_summary_parts_missing_from_stream() {
    let mut state = ResponsesStreamState::default();
    let part_done = serde_json::json!({
        "type": "response.reasoning_summary_part.done",
        "item_id": "rs_parts",
        "output_index": 0,
        "summary_index": 0,
        "part": {
            "type": "summary_text",
            "text": "**Analyzing request**"
        }
    });
    let _ = process_responses_event(part_done, &mut state);

    let final_item = serde_json::json!({
        "type": "response.output_item.done",
        "output_index": 0,
        "item": {
            "type": "reasoning",
            "id": "rs_parts",
            "summary": [
                {"type": "summary_text", "text": "**Analyzing request**"},
                {"type": "summary_text", "text": "**Tracing response**"}
            ],
            "encrypted_content": "opaque-parts"
        }
    });

    let finalized = process_responses_event(final_item, &mut state);

    assert!(matches!(
        finalized,
        ResponsesEventResult::Events(events)
            if matches!(
                events.as_slice(),
                [
                    StreamEvent::ProviderRaw(_),
                    StreamEvent::ResponsesReasoningPartDone {
                        item_id,
                        output_index,
                        summary_index,
                        text,
                    },
                    StreamEvent::ResponsesReasoningItem(item)
                ] if item_id == "rs_parts"
                    && *output_index == 0
                    && *summary_index == 1
                    && text == "**Tracing response**"
                    && item.id == "rs_parts"
                    && item.encrypted_content == "opaque-parts"
            )
    ));
}

#[test]
fn responses_summary_dedup_is_scoped_to_each_reasoning_item() {
    let mut state = ResponsesStreamState::default();
    let streamed_a = serde_json::json!({
        "type": "response.reasoning_summary_text.delta",
        "item_id": "rs_a",
        "output_index": 0,
        "summary_index": 0,
        "delta": "Streamed A."
    });
    let _ = process_responses_event(streamed_a, &mut state);

    let done_b = serde_json::json!({
        "type": "response.output_item.done",
        "output_index": 1,
        "item": {
            "type": "reasoning",
            "id": "rs_b",
            "summary": [{"type": "summary_text", "text": "Final B."}],
            "encrypted_content": "opaque-b"
        }
    });
    let finalized_b = process_responses_event(done_b, &mut state);
    assert!(matches!(
        finalized_b,
        ResponsesEventResult::Events(events)
            if matches!(
                events.as_slice(),
                [
                    StreamEvent::ProviderRaw(_),
                    StreamEvent::ResponsesReasoningPartDone {
                        item_id,
                        output_index,
                        summary_index,
                        text,
                    },
                    StreamEvent::ResponsesReasoningItem(item)
                ] if item_id == "rs_b"
                    && *output_index == 1
                    && *summary_index == 0
                    && text == "Final B."
                    && item.id == "rs_b"
                    && item.encrypted_content == "opaque-b"
            )
    ));

    let done_a = serde_json::json!({
        "type": "response.output_item.done",
        "output_index": 0,
        "item": {
            "type": "reasoning",
            "id": "rs_a",
            "summary": [{"type": "summary_text", "text": "Streamed A."}],
            "encrypted_content": "opaque-a"
        }
    });
    let finalized_a = process_responses_event(done_a, &mut state);
    assert!(matches!(
        finalized_a,
        ResponsesEventResult::Events(events)
            if matches!(
                events.as_slice(),
                [
                    StreamEvent::ProviderRaw(_),
                    StreamEvent::ResponsesReasoningItem(item)
                ] if item.id == "rs_a" && item.encrypted_content == "opaque-a"
            )
    ));
}

#[test]
fn responses_reasoning_summary_part_done_preserves_item_identity() {
    let event = serde_json::json!({
        "type": "response.reasoning_summary_part.done",
        "item_id": "rs_part",
        "output_index": 2,
        "summary_index": 1,
        "part": {
            "type": "summary_text",
            "text": "**Tracing response**"
        }
    });
    let mut state = ResponsesStreamState::default();

    let result = process_responses_event(event.clone(), &mut state);

    assert!(matches!(
        result,
        ResponsesEventResult::Events(events)
            if matches!(
                events.as_slice(),
                [
                    StreamEvent::ProviderRaw(raw),
                    StreamEvent::ResponsesReasoningPartDone {
                        item_id,
                        output_index,
                        summary_index,
                        text,
                    }
                ] if raw == &event
                    && item_id == "rs_part"
                    && *output_index == 2
                    && *summary_index == 1
                    && text == "**Tracing response**"
            )
    ));
}

#[test]
fn responses_final_reasoning_item_emits_summary_and_opaque_state() {
    let event = serde_json::json!({
        "type": "response.output_item.done",
        "output_index": 3,
        "item": {
            "type": "reasoning",
            "id": "rs_456",
            "summary": [
                {"type": "summary_text", "text": "**Analyzing request**"},
                {"type": "summary_text", "text": "**Tracing response**"}
            ],
            "encrypted_content": "opaque-final"
        }
    });
    let mut state = ResponsesStreamState::default();

    let result = process_responses_event(event.clone(), &mut state);

    assert!(matches!(
        result,
        ResponsesEventResult::Events(events)
            if matches!(
                events.as_slice(),
                [
                    StreamEvent::ProviderRaw(raw),
                    StreamEvent::ResponsesReasoningPartDone {
                        item_id: first_item_id,
                        output_index: first_output_index,
                        summary_index: first_summary_index,
                        text: first_text,
                    },
                    StreamEvent::ResponsesReasoningPartDone {
                        item_id: second_item_id,
                        output_index: second_output_index,
                        summary_index: second_summary_index,
                        text: second_text,
                    },
                    StreamEvent::ResponsesReasoningItem(item)
                ] if raw == &event
                    && first_item_id == "rs_456"
                    && *first_output_index == 3
                    && *first_summary_index == 0
                    && first_text == "**Analyzing request**"
                    && second_item_id == "rs_456"
                    && *second_output_index == 3
                    && *second_summary_index == 1
                    && second_text == "**Tracing response**"
                    && item.id == "rs_456"
                    && item.encrypted_content == "opaque-final"
            )
    ));
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
        ResponsesEventResult::Completed(events)
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
        ResponsesEventResult::Completed(events) if events.is_empty()
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
        ResponsesEventResult::Failed(events)
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
        ResponsesEventResult::Failed(events)
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
        ResponsesEventResult::Failed(events)
            if matches!(
                events.as_slice(),
                [StreamEvent::ProviderRaw(raw), StreamEvent::Error(message)]
                    if raw == &event && message == "request rejected"
            )
    ));
}
