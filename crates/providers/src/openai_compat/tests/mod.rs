mod schema_normalization;

use super::{
    ResponsesEventResult, ResponsesStreamState, SseLineResult, StreamingToolState,
    finalize_responses_stream, finalize_stream, normalize_tool_call_arguments_from_schemas,
    parse_responses_completion, parse_tool_calls, process_openai_sse_line, process_responses_event,
    process_responses_sse_line, to_responses_input,
};

use {
    chelix_agents::model::{ChatMessage, StreamEvent, ToolCall},
    chelix_common::{
        ProviderItemId, ProviderItemPosition, ProviderItemUpdatePayload, ProviderOutputItem,
        ProviderOutputPayload, ProviderSegmentOutcome, ReasoningItem, ReasoningPart,
    },
};

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
fn responses_replay_preserves_canonical_output_order_and_places_tool_outputs_after() {
    let message = ChatMessage::assistant_with_tools(None, Vec::new()).with_provider_items(vec![
        ProviderOutputItem {
            id: ProviderItemId::new("rs_123"),
            position: ProviderItemPosition::new(0),
            payload: ProviderOutputPayload::Reasoning(ReasoningItem {
                id: ProviderItemId::new("rs_123"),
                output_index: ProviderItemPosition::new(0),
                summary_parts: vec![ReasoningPart {
                    part_index: 0,
                    text: "Analyzing".to_string(),
                }],
                visible_text: None,
                encrypted_content: Some("opaque-state".to_string()),
            }),
        },
        ProviderOutputItem {
            id: ProviderItemId::new("call_1"),
            position: ProviderItemPosition::new(1),
            payload: ProviderOutputPayload::FunctionCall {
                call_id: "call_1".to_string(),
                name: "lookup".to_string(),
                arguments: "{\"query\":\"rust\"}".to_string(),
            },
        },
        ProviderOutputItem {
            id: ProviderItemId::new("msg_2"),
            position: ProviderItemPosition::new(2),
            payload: ProviderOutputPayload::Message {
                text: "Done lookup.".to_string(),
            },
        },
    ]);
    let tool_output = ChatMessage::tool("call_1", "found result");

    let input = to_responses_input(&[message, tool_output]);

    assert_eq!(input.len(), 4);
    assert_eq!(
        input[0],
        serde_json::json!({
            "type": "reasoning",
            "id": "rs_123",
            "summary": [{"type": "summary_text", "text": "Analyzing"}],
            "encrypted_content": "opaque-state",
        })
    );
    assert_eq!(
        input[1],
        serde_json::json!({
            "type": "function_call",
            "call_id": "call_1",
            "name": "lookup",
            "arguments": "{\"query\":\"rust\"}",
        })
    );
    assert_eq!(
        input[2],
        serde_json::json!({
            "type": "message",
            "role": "assistant",
            "content": [{"type": "output_text", "text": "Done lookup."}]
        })
    );
    assert_eq!(
        input[3],
        serde_json::json!({
            "type": "function_call_output",
            "call_id": "call_1",
            "output": "found result",
        })
    );
}

#[test]
fn responses_streamed_summary_emits_canonical_provider_update() {
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
            if events.iter().any(|e| matches!(e, StreamEvent::ProviderItemUpdate(u)
                if u.item_id.as_str() == "rs_123"
                    && u.position.as_usize() == 0
                    && matches!(&u.payload, ProviderItemUpdatePayload::ReasoningDelta { part_index: 0, delta } if delta == "Checked the request.")
            ))
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

    let ResponsesEventResult::Events(events) = result else {
        panic!("expected the final reasoning item to produce events");
    };
    let updates = provider_item_updates(&events);
    // The summary of the final item is the reasoning the provider produced, so
    // every part reaches the segment. Position comes from first-appearance
    // order on ingress, not from the transport `output_index`.
    assert!(
        updates
            .iter()
            .all(|u| u.item_id.as_str() == "rs_456" && u.position.as_usize() == 0)
    );
    let parts: Vec<_> = updates
        .iter()
        .filter_map(|u| match &u.payload {
            ProviderItemUpdatePayload::ReasoningPartDone { part_index, text } => {
                Some((*part_index, text.as_str()))
            },
            _ => None,
        })
        .collect();
    assert_eq!(parts, vec![
        (0, "**Analyzing request**"),
        (1, "**Tracing response**")
    ]);
    assert!(updates.iter().any(|u| matches!(
        &u.payload,
        ProviderItemUpdatePayload::ReasoningItemDone { encrypted_content }
            if encrypted_content.as_deref() == Some("opaque-final")
    )));
}

/// Canonical item updates carried by a batch of stream events.
fn provider_item_updates(events: &[StreamEvent]) -> Vec<&chelix_common::ProviderItemUpdate> {
    events
        .iter()
        .filter_map(|event| match event {
            StreamEvent::ProviderItemUpdate(update) => Some(update),
            _ => None,
        })
        .collect()
}

#[test]
fn a_summary_part_already_streamed_is_not_emitted_again_by_the_final_item() {
    let mut state = ResponsesStreamState::default();
    let _ = process_responses_event(
        serde_json::json!({
            "type": "response.reasoning_summary_part.done",
            "item_id": "rs_789",
            "output_index": 0,
            "summary_index": 0,
            "part": {"type": "summary_text", "text": "**Analyzing request**"}
        }),
        &mut state,
    );

    let result = process_responses_event(
        serde_json::json!({
            "type": "response.output_item.done",
            "output_index": 0,
            "item": {
                "type": "reasoning",
                "id": "rs_789",
                "summary": [
                    {"type": "summary_text", "text": "**Analyzing request**"},
                    {"type": "summary_text", "text": "**Tracing response**"}
                ],
                "encrypted_content": "opaque-final"
            }
        }),
        &mut state,
    );

    let ResponsesEventResult::Events(events) = result else {
        panic!("expected the final reasoning item to produce events");
    };
    // The final item repeats the whole summary. Only the part the stream never
    // delivered is emitted, so the streamed one is not appended twice.
    let parts: Vec<_> = provider_item_updates(&events)
        .iter()
        .filter_map(|u| match &u.payload {
            ProviderItemUpdatePayload::ReasoningPartDone { part_index, text } => {
                Some((*part_index, text.clone()))
            },
            _ => None,
        })
        .collect();
    assert_eq!(parts, vec![(1, "**Tracing response**".to_string())]);
}

#[test]
fn a_replayed_reasoning_item_without_opaque_state_sends_no_substitute() {
    let message = ChatMessage::assistant_with_tools(None, Vec::new()).with_provider_items(vec![
        ProviderOutputItem {
            id: ProviderItemId::new("rs_1"),
            position: ProviderItemPosition::new(0),
            payload: ProviderOutputPayload::Reasoning(ReasoningItem {
                id: ProviderItemId::new("rs_1"),
                output_index: ProviderItemPosition::new(0),
                summary_parts: Vec::new(),
                visible_text: None,
                encrypted_content: None,
            }),
        },
    ]);

    let input = to_responses_input(&[message]);

    // An absent opaque state is absent, not an empty string: substituting one
    // would send a value the provider never issued.
    assert_eq!(
        input[0],
        serde_json::json!({
            "type": "reasoning",
            "id": "rs_1",
            "summary": [],
            "encrypted_content": null,
        })
    );
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
            if matches!(
                events.as_slice(),
                [
                    StreamEvent::SegmentStart { .. },
                    StreamEvent::ProviderRaw(raw),
                    StreamEvent::SegmentClose {
                        outcome: ProviderSegmentOutcome::Completed,
                        ..
                    },
                ] if raw == &event
            )
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
                [
                    StreamEvent::SegmentStart { .. },
                    StreamEvent::ProviderRaw(raw),
                    StreamEvent::SegmentClose {
                        outcome: ProviderSegmentOutcome::Failed,
                        ..
                    },
                    StreamEvent::Error(message),
                ] if raw == &event && message == "upstream failed"
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
                [
                    StreamEvent::SegmentStart { .. },
                    StreamEvent::ProviderRaw(raw),
                    StreamEvent::SegmentClose {
                        outcome: ProviderSegmentOutcome::Incomplete,
                        ..
                    },
                    StreamEvent::Error(message),
                ] if raw == &event && message == "response incomplete: max_output_tokens"
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
                [
                    StreamEvent::SegmentStart { .. },
                    StreamEvent::ProviderRaw(raw),
                    StreamEvent::SegmentClose {
                        outcome: ProviderSegmentOutcome::Failed,
                        ..
                    },
                    StreamEvent::Error(message),
                ] if raw == &event && message == "request rejected"
            )
    ));
}

/// Collect every provider item update produced by a Chat Completions stream.
fn chat_provider_updates(lines: &[serde_json::Value]) -> Vec<(String, usize)> {
    let mut state = StreamingToolState::default();
    let mut updates = Vec::new();
    for line in lines {
        let SseLineResult::Events(events) = process_openai_sse_line(&line.to_string(), &mut state)
        else {
            continue;
        };
        for event in events {
            if let StreamEvent::ProviderItemUpdate(update) = event {
                updates.push((
                    update.item_id.as_str().to_string(),
                    update.position.as_usize(),
                ));
            }
        }
    }
    for event in finalize_stream(&mut state) {
        if let StreamEvent::ProviderItemUpdate(update) = event {
            updates.push((
                update.item_id.as_str().to_string(),
                update.position.as_usize(),
            ));
        }
    }
    updates
}

/// Assert that no two distinct item identities share a position, which is the
/// invariant the materializer enforces with `PositionIdConflict`.
fn assert_no_position_conflicts(updates: &[(String, usize)]) {
    let mut by_position: std::collections::HashMap<usize, &str> = std::collections::HashMap::new();
    let mut by_item: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
    for (item_id, position) in updates {
        if let Some(existing) = by_position.get(position) {
            assert_eq!(
                *existing,
                item_id.as_str(),
                "position {position} is shared by `{existing}` and `{item_id}`"
            );
        }
        if let Some(existing) = by_item.get(item_id.as_str()) {
            assert_eq!(
                *existing, *position,
                "item `{item_id}` moved from position {existing} to {position}"
            );
        }
        by_position.insert(*position, item_id.as_str());
        by_item.insert(item_id.as_str(), *position);
    }
}

#[test]
fn chat_completions_reasoning_and_message_get_distinct_positions() {
    // A provider that streams `reasoning_content` before the visible answer.
    // Reasoning and message are two different items and must not collide.
    let updates = chat_provider_updates(&[
        serde_json::json!({
            "id": "chatcmpl-1",
            "choices": [{"delta": {"reasoning_content": "Weighing the options."}}]
        }),
        serde_json::json!({
            "id": "chatcmpl-1",
            "choices": [{"delta": {"content": "Here is the answer."}}]
        }),
    ]);

    assert_no_position_conflicts(&updates);
    assert_eq!(updates, vec![
        ("rs_0".to_string(), 0),
        ("msg_0".to_string(), 1)
    ]);
}

#[test]
fn chat_completions_think_tags_and_message_get_distinct_positions() {
    // The same collision reached through the `<think>` tag state machine.
    let updates = chat_provider_updates(&[
        serde_json::json!({
            "id": "chatcmpl-2",
            "choices": [{"delta": {"content": "<think>Planning.</think>"}}]
        }),
        serde_json::json!({
            "id": "chatcmpl-2",
            "choices": [{"delta": {"content": "Final answer."}}]
        }),
    ]);

    assert_no_position_conflicts(&updates);
    assert_eq!(updates, vec![
        ("rs_0".to_string(), 0),
        ("msg_0".to_string(), 1)
    ]);
}

#[test]
fn chat_completions_reasoning_message_and_tool_calls_never_collide() {
    // Reasoning, visible text and two tool calls in one segment. The tool call
    // transport index restarts at 0 and must not be used as a position.
    let updates = chat_provider_updates(&[
        serde_json::json!({
            "id": "chatcmpl-3",
            "choices": [{"delta": {"reasoning_content": "Deciding on tools."}}]
        }),
        serde_json::json!({
            "id": "chatcmpl-3",
            "choices": [{"delta": {"content": "Looking that up."}}]
        }),
        serde_json::json!({
            "id": "chatcmpl-3",
            "choices": [{"delta": {"tool_calls": [
                {"index": 0, "id": "call_a", "function": {"name": "lookup", "arguments": ""}}
            ]}}]
        }),
        serde_json::json!({
            "id": "chatcmpl-3",
            "choices": [{"delta": {"tool_calls": [
                {"index": 0, "function": {"arguments": "{\"q\":1}"}}
            ]}}]
        }),
        serde_json::json!({
            "id": "chatcmpl-3",
            "choices": [{"delta": {"tool_calls": [
                {"index": 1, "id": "call_b", "function": {"name": "fetch", "arguments": ""}}
            ]}}]
        }),
    ]);

    assert_no_position_conflicts(&updates);
    assert_eq!(updates, vec![
        ("rs_0".to_string(), 0),
        ("msg_0".to_string(), 1),
        ("call_a".to_string(), 2),
        ("call_a".to_string(), 2),
        ("call_b".to_string(), 3),
    ]);
}

#[test]
fn chat_completions_interleaved_reasoning_keeps_stable_positions() {
    // Reasoning resuming after visible text must return to its own position.
    let updates = chat_provider_updates(&[
        serde_json::json!({
            "id": "chatcmpl-4",
            "choices": [{"delta": {"reasoning_content": "First thought."}}]
        }),
        serde_json::json!({
            "id": "chatcmpl-4",
            "choices": [{"delta": {"content": "Partial answer."}}]
        }),
        serde_json::json!({
            "id": "chatcmpl-4",
            "choices": [{"delta": {"reasoning_content": "Second thought."}}]
        }),
        serde_json::json!({
            "id": "chatcmpl-4",
            "choices": [{"delta": {"content": " Rest of answer."}}]
        }),
    ]);

    assert_no_position_conflicts(&updates);
    assert_eq!(updates, vec![
        ("rs_0".to_string(), 0),
        ("msg_0".to_string(), 1),
        ("rs_0".to_string(), 0),
        ("msg_0".to_string(), 1),
    ]);
}

/// Collect every provider item update produced by a Responses stream.
fn responses_provider_updates(events: &[serde_json::Value]) -> Vec<(String, usize)> {
    let mut state = ResponsesStreamState::default();
    let mut updates = Vec::new();
    for event in events {
        let produced = match process_responses_event(event.clone(), &mut state) {
            ResponsesEventResult::Events(events)
            | ResponsesEventResult::Completed(events)
            | ResponsesEventResult::Failed(events) => events,
            ResponsesEventResult::Skip => Vec::new(),
        };
        for event in produced {
            if let StreamEvent::ProviderItemUpdate(update) = event {
                updates.push((
                    update.item_id.as_str().to_string(),
                    update.position.as_usize(),
                ));
            }
        }
    }
    updates
}

#[test]
fn responses_repeated_output_index_yields_distinct_positions() {
    // Observed in production: a provider reuses `output_index` 0 for the
    // reasoning item and for the message item of the same response.
    let updates = responses_provider_updates(&[
        serde_json::json!({
            "type": "response.reasoning_summary_text.delta",
            "item_id": "rs_resp_1_0",
            "output_index": 0,
            "summary_index": 0,
            "delta": "Thinking it through."
        }),
        serde_json::json!({
            "type": "response.output_text.delta",
            "item_id": "msg_resp_1_0",
            "output_index": 0,
            "delta": "The answer."
        }),
    ]);

    assert_no_position_conflicts(&updates);
    assert_eq!(updates, vec![
        ("rs_resp_1_0".to_string(), 0),
        ("msg_resp_1_0".to_string(), 1),
    ]);
}

#[test]
fn responses_missing_output_index_yields_distinct_positions() {
    // The same stream without any index field at all.
    let updates = responses_provider_updates(&[
        serde_json::json!({
            "type": "response.reasoning_summary_text.delta",
            "item_id": "rs_resp_2_0",
            "output_index": 0,
            "summary_index": 0,
            "delta": "Thinking it through."
        }),
        serde_json::json!({
            "type": "response.output_text.delta",
            "item_id": "msg_resp_2_0",
            "delta": "The answer."
        }),
    ]);

    assert_no_position_conflicts(&updates);
    assert_eq!(updates, vec![
        ("rs_resp_2_0".to_string(), 0),
        ("msg_resp_2_0".to_string(), 1),
    ]);
}

#[test]
fn responses_reasoning_message_and_tool_calls_keep_ingress_order() {
    // Full response where the function call slots restart at 0 while the
    // reasoning and message items already occupy earlier positions.
    let updates = responses_provider_updates(&[
        serde_json::json!({
            "type": "response.reasoning_summary_text.delta",
            "item_id": "rs_resp_3_0",
            "output_index": 0,
            "summary_index": 0,
            "delta": "Choosing a tool."
        }),
        serde_json::json!({
            "type": "response.output_text.delta",
            "item_id": "msg_resp_3_0",
            "output_index": 0,
            "delta": "Looking that up."
        }),
        serde_json::json!({
            "type": "response.output_item.added",
            "output_index": 0,
            "item": {"type": "function_call", "call_id": "call_a", "name": "lookup"}
        }),
        serde_json::json!({
            "type": "response.function_call_arguments.delta",
            "output_index": 0,
            "delta": "{\"q\":1}"
        }),
        serde_json::json!({
            "type": "response.output_item.added",
            "output_index": 1,
            "item": {"type": "function_call", "call_id": "call_b", "name": "fetch"}
        }),
        serde_json::json!({
            "type": "response.function_call_arguments.done",
            "output_index": 1,
            "arguments": "{\"q\":2}"
        }),
    ]);

    assert_no_position_conflicts(&updates);
    assert_eq!(updates, vec![
        ("rs_resp_3_0".to_string(), 0),
        ("msg_resp_3_0".to_string(), 1),
        ("call_a".to_string(), 2),
        ("call_a".to_string(), 2),
        ("call_b".to_string(), 3),
        ("call_b".to_string(), 3),
    ]);
}

/// Assert that a malformed provider event fails the stream instead of
/// silently substituting an identity or a slot.
fn assert_responses_rejects(state: &mut ResponsesStreamState, event: serde_json::Value) {
    let result = process_responses_event(event, state);
    let ResponsesEventResult::Failed(events) = result else {
        panic!("malformed provider event must fail the stream, got {result:?}");
    };
    assert!(
        events
            .iter()
            .any(|event| matches!(event, StreamEvent::Error(_))),
        "failed stream must report an error"
    );
    assert!(
        events
            .iter()
            .any(|event| matches!(event, StreamEvent::SegmentClose {
                outcome: ProviderSegmentOutcome::Failed,
                ..
            })),
        "failed stream must close the segment as failed"
    );
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, StreamEvent::ProviderItemUpdate(_))),
        "no item update may be emitted for a malformed event"
    );
}

#[test]
fn responses_output_text_without_item_id_fails_the_stream() {
    let mut state = ResponsesStreamState::default();
    assert_responses_rejects(
        &mut state,
        serde_json::json!({
            "type": "response.output_text.delta",
            "output_index": 0,
            "delta": "The answer."
        }),
    );
}

#[test]
fn responses_reasoning_item_without_id_fails_the_stream() {
    let mut state = ResponsesStreamState::default();
    assert_responses_rejects(
        &mut state,
        serde_json::json!({
            "type": "response.output_item.done",
            "output_index": 0,
            "item": {"type": "reasoning", "encrypted_content": "opaque"}
        }),
    );
}

#[test]
fn responses_function_call_without_call_id_fails_the_stream() {
    let mut state = ResponsesStreamState::default();
    assert_responses_rejects(
        &mut state,
        serde_json::json!({
            "type": "response.output_item.added",
            "output_index": 0,
            "item": {"type": "function_call", "name": "lookup"}
        }),
    );
}

#[test]
fn responses_arguments_for_unopened_slot_fail_the_stream() {
    let mut state = ResponsesStreamState::default();
    assert_responses_rejects(
        &mut state,
        serde_json::json!({
            "type": "response.function_call_arguments.done",
            "output_index": 7,
            "arguments": "{}"
        }),
    );
}

#[test]
fn responses_arguments_without_slot_fail_the_stream() {
    let mut state = ResponsesStreamState::default();
    let opened = process_responses_event(
        serde_json::json!({
            "type": "response.output_item.added",
            "output_index": 0,
            "item": {"type": "function_call", "call_id": "call_a", "name": "lookup"}
        }),
        &mut state,
    );
    assert!(matches!(opened, ResponsesEventResult::Events(_)));

    assert_responses_rejects(
        &mut state,
        serde_json::json!({
            "type": "response.function_call_arguments.delta",
            "delta": "{\"q\":1}"
        }),
    );
}

#[test]
fn chat_completions_tool_call_without_index_fails_the_stream() {
    let mut state = StreamingToolState::default();
    let line = serde_json::json!({
        "id": "chatcmpl-5",
        "choices": [{"delta": {"tool_calls": [
            {"id": "call_a", "function": {"name": "lookup", "arguments": ""}}
        ]}}]
    })
    .to_string();

    let SseLineResult::Events(events) = process_openai_sse_line(&line, &mut state) else {
        panic!("malformed tool call must produce events reporting the failure");
    };

    assert!(
        events
            .iter()
            .any(|event| matches!(event, StreamEvent::Error(_))),
        "malformed tool call must report an error"
    );
    assert!(
        events
            .iter()
            .any(|event| matches!(event, StreamEvent::SegmentClose {
                outcome: ProviderSegmentOutcome::Failed,
                ..
            })),
        "malformed tool call must close the segment as failed"
    );
}

fn segment_close_outcome(events: &[StreamEvent]) -> ProviderSegmentOutcome {
    events
        .iter()
        .find_map(|event| match event {
            StreamEvent::SegmentClose { outcome, .. } => Some(*outcome),
            _ => None,
        })
        .unwrap_or_else(|| panic!("stream must close its segment"))
}

#[test]
fn chat_completions_stream_without_terminal_marker_is_a_transport_error() {
    let mut state = StreamingToolState::default();
    let line = serde_json::json!({
        "id": "chatcmpl-6",
        "choices": [{"delta": {"content": "truncated"}}]
    })
    .to_string();
    let SseLineResult::Events(_) = process_openai_sse_line(&line, &mut state) else {
        panic!("a content delta must produce events");
    };

    let events = finalize_stream(&mut state);

    assert_eq!(
        segment_close_outcome(&events),
        ProviderSegmentOutcome::TransportError
    );
    assert!(
        events
            .iter()
            .any(|event| matches!(event, StreamEvent::Error(_))),
        "a truncated stream must report an error"
    );
}

#[test]
fn chat_completions_stream_with_finish_reason_completes() {
    let mut state = StreamingToolState::default();
    let line = serde_json::json!({
        "id": "chatcmpl-7",
        "choices": [{"delta": {"content": "done"}, "finish_reason": "stop"}]
    })
    .to_string();
    let SseLineResult::Events(_) = process_openai_sse_line(&line, &mut state) else {
        panic!("a terminal chunk must produce events");
    };

    let events = finalize_stream(&mut state);

    assert_eq!(
        segment_close_outcome(&events),
        ProviderSegmentOutcome::Completed
    );
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, StreamEvent::Error(_))),
        "a terminated stream must not report an error"
    );
}

#[test]
fn chat_completions_stream_with_done_frame_completes() {
    let mut state = StreamingToolState::default();
    let line = serde_json::json!({
        "id": "chatcmpl-8",
        "choices": [{"delta": {"content": "done"}}]
    })
    .to_string();
    let SseLineResult::Events(_) = process_openai_sse_line(&line, &mut state) else {
        panic!("a content delta must produce events");
    };
    assert!(matches!(
        process_openai_sse_line("[DONE]", &mut state),
        SseLineResult::Done
    ));

    let events = finalize_stream(&mut state);

    assert_eq!(
        segment_close_outcome(&events),
        ProviderSegmentOutcome::Completed
    );
}

#[test]
fn responses_stream_without_terminal_event_is_a_transport_error() {
    let mut state = ResponsesStreamState::default();
    let event = serde_json::json!({
        "type": "response.output_text.delta",
        "item_id": "msg_a",
        "delta": "truncated"
    });
    let ResponsesEventResult::Events(_) = process_responses_event(event, &mut state) else {
        panic!("a text delta must produce events");
    };

    let events = finalize_responses_stream(&mut state);

    assert_eq!(
        segment_close_outcome(&events),
        ProviderSegmentOutcome::TransportError
    );
    assert!(
        events
            .iter()
            .any(|event| matches!(event, StreamEvent::Error(_))),
        "a truncated Responses stream must report an error"
    );
}

#[test]
fn responses_stream_with_completed_event_does_not_close_twice() {
    let mut state = ResponsesStreamState::default();
    let delta = serde_json::json!({
        "type": "response.output_text.delta",
        "item_id": "msg_a",
        "delta": "done"
    });
    let ResponsesEventResult::Events(_) = process_responses_event(delta, &mut state) else {
        panic!("a text delta must produce events");
    };
    let completed = serde_json::json!({
        "type": "response.completed",
        "response": {"id": "resp_a"}
    });
    let ResponsesEventResult::Completed(events) = process_responses_event(completed, &mut state)
    else {
        panic!("response.completed must complete the stream");
    };

    assert_eq!(
        segment_close_outcome(&events),
        ProviderSegmentOutcome::Completed
    );
    let finalized = finalize_responses_stream(&mut state);
    assert!(
        !finalized
            .iter()
            .any(|event| matches!(event, StreamEvent::SegmentClose { .. })),
        "an already closed segment must not be closed again"
    );
}
