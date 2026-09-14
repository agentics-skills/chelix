#![allow(clippy::unwrap_used, clippy::expect_used)]

use {
    crate::openai_compat::{
        ResponsesEventResult, ResponsesStreamState, SseLineResult, StreamingToolState,
        finalize_responses_stream, finalize_stream, process_openai_sse_line,
        process_responses_event,
    },
    chelix_agents::model::{StreamEvent, collect_stream},
};

#[tokio::test]
async fn collected_tool_arguments_preserve_native_falsy_values() {
    let arguments = serde_json::json!({"offset":0,"multiline":false,"value":null});
    let mut state = StreamingToolState::default();
    let chunk = serde_json::json!({
        "id":"chat_falsy",
        "choices":[{"delta":{"tool_calls":[{"index":3,"id":"call_falsy","function":{"name":"probe","arguments":arguments.to_string()}}]},"finish_reason":"tool_calls"}]
    });
    let SseLineResult::Events(mut events) = process_openai_sse_line(&chunk.to_string(), &mut state)
    else {
        panic!("chat chunk must emit events")
    };
    events.extend(finalize_stream(&mut state));
    let chat = collect_stream(Box::pin(tokio_stream::iter(events)))
        .await
        .unwrap();
    assert_eq!(chat.tool_calls.len(), 1);
    assert_eq!(chat.tool_calls[0].arguments, arguments);

    let mut state = ResponsesStreamState::default();
    let mut events = Vec::<StreamEvent>::new();
    for event in [
        serde_json::json!({"type":"response.output_item.added","output_index":3,"item":{"type":"function_call","id":"fc_falsy","call_id":"call_falsy","name":"probe","arguments":""}}),
        serde_json::json!({"type":"response.function_call_arguments.done","output_index":3,"arguments":arguments.to_string()}),
        serde_json::json!({"type":"response.completed","response":{"id":"resp_falsy","usage":{"input_tokens":20,"output_tokens":10}}}),
    ] {
        match process_responses_event(event, &mut state) {
            ResponsesEventResult::Events(batch) | ResponsesEventResult::Completed(batch) => {
                events.extend(batch)
            },
            other => panic!("expected successful Responses events: {other:?}"),
        }
    }
    events.extend(finalize_responses_stream(&mut state));
    let responses = collect_stream(Box::pin(tokio_stream::iter(events)))
        .await
        .unwrap();
    assert_eq!(responses.tool_calls.len(), 1);
    assert_eq!(responses.tool_calls[0].arguments, arguments);
    assert_eq!(responses.usage.input_tokens, 20);
    assert_eq!(responses.usage.output_tokens, 10);
}

/// Run a Responses event sequence through the adapter and the collector.
async fn collect_responses(
    events: Vec<serde_json::Value>,
) -> anyhow::Result<chelix_agents::model::CompletionResponse> {
    let mut state = ResponsesStreamState::default();
    let mut produced = Vec::<StreamEvent>::new();
    for event in events {
        match process_responses_event(event, &mut state) {
            ResponsesEventResult::Events(batch)
            | ResponsesEventResult::Completed(batch)
            | ResponsesEventResult::Failed(batch) => produced.extend(batch),
        }
    }
    produced.extend(finalize_responses_stream(&mut state));
    collect_stream(Box::pin(tokio_stream::iter(produced))).await
}

#[tokio::test]
async fn collected_responses_call_uses_final_item_arguments_over_partial_deltas() {
    let collected = collect_responses(vec![
        serde_json::json!({"type":"response.output_item.added","output_index":0,"item":{"type":"function_call","call_id":"call_1","name":"lookup","arguments":""}}),
        serde_json::json!({"type":"response.function_call_arguments.delta","output_index":0,"delta":"{\"q\":"}),
        serde_json::json!({"type":"response.output_item.done","output_index":0,"item":{"type":"function_call","call_id":"call_1","name":"lookup","arguments":"{\"q\":\"full\"}"}}),
        serde_json::json!({"type":"response.completed","response":{"id":"resp_1"}}),
    ])
    .await
    .unwrap();

    assert_eq!(collected.tool_calls.len(), 1);
    assert_eq!(
        collected.tool_calls[0].arguments,
        serde_json::json!({"q": "full"})
    );
    assert!(collected.tool_calls[0].argument_diagnostic.is_none());
}

#[tokio::test]
async fn collected_responses_call_present_only_in_final_output_is_executed() {
    let collected = collect_responses(vec![serde_json::json!({
        "type":"response.completed",
        "response":{
            "id":"resp_1",
            "output":[{"type":"function_call","call_id":"call_1","name":"lookup","arguments":"{\"q\":\"full\"}"}]
        }
    })])
    .await
    .unwrap();

    assert_eq!(collected.tool_calls.len(), 1);
    assert_eq!(collected.tool_calls[0].name, "lookup");
    assert_eq!(
        collected.tool_calls[0].arguments,
        serde_json::json!({"q": "full"})
    );
}

#[tokio::test]
async fn collected_responses_non_json_first_frame_surfaces_the_parse_error() {
    let mut state = ResponsesStreamState::default();
    let ResponsesEventResult::Failed(events) =
        crate::openai_compat::process_responses_sse_line("not json", &mut state)
    else {
        panic!("non-JSON line must fail the stream")
    };

    let error = collect_stream(Box::pin(tokio_stream::iter(events)))
        .await
        .unwrap_err();

    assert!(error.to_string().contains("non-JSON Responses SSE line"));
}
