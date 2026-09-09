#![allow(clippy::unwrap_used)]

use serde_json::{Value, json};

use crate::ui_history_types::{
    UiContent, UiGeneration, UiHistoryBatch, UiHistoryPage, UiMessageId, UiPresentation, UiSnapshot,
};

fn snapshot(record: Value) -> UiSnapshot {
    UiSnapshot {
        id: UiMessageId("segment:segment-1".into()),
        position: 3,
        revision: 7,
        canonical_committed: true,
        content: serde_json::from_value(record).unwrap(),
        presentation: UiPresentation::default(),
        accumulated_arguments: None,
        assistant_id: None,
        outcome: None,
    }
}

#[test]
fn assistant_snapshot_serialization_preserves_metadata_and_canonical_record() {
    let record = json!({
        "role": "assistant",
        "content": "Complete answer",
        "created_at": 123,
        "model": "provider::model",
        "provider": "provider",
        "reasoningEffort": "high",
        "inputTokens": 100,
        "outputTokens": 20,
        "cacheReadTokens": 10,
        "cacheWriteTokens": 5,
        "durationMs": 456,
        "requestInputTokens": 80,
        "requestOutputTokens": 15,
        "requestCacheReadTokens": 8,
        "requestCacheWriteTokens": 4,
        "tool_calls": [{
            "id": "call-1", "type": "function",
            "function": {"name": "example", "arguments": "{\"llmApiResponse\":\"tool input\"}"}
        }],
        "reasoning": "Visible reasoning",
        "providerItems": [{"id": "item-1", "position": 0, "payload": {"type": "message", "text": "Complete answer"}}],
        "segmentId": "segment-1",
        "llmApiResponse": [{"type": "response.output_text.delta", "delta": "Complete"}],
        "audio": "media/main/voice.ogg",
        "seq": 9,
        "run_id": "run-1",
        "clientMessageId": "00000000-0000-4000-8000-000000000001",
        "tts_provider": "voice-provider"
    });
    let snapshot = snapshot(record.clone());
    let mut expected = record.clone();
    let fields = expected.as_object_mut().unwrap();
    fields.remove("llmApiResponse");
    fields.insert("id".into(), json!("segment:segment-1"));
    fields.insert("position".into(), json!(3));
    fields.insert("revision".into(), json!(7));
    fields.insert("canonicalCommitted".into(), json!(true));
    fields.insert("presentation".into(), json!({}));

    assert_eq!(serde_json::to_value(&snapshot).unwrap(), expected);
    assert_eq!(snapshot.public_value().unwrap(), expected);
    let round_trip: UiSnapshot = serde_json::from_value(expected.clone()).unwrap();
    assert_eq!(round_trip.public_value().unwrap(), expected);
    let UiContent::Record(canonical) = &snapshot.content else {
        panic!("assistant record expected");
    };
    assert_eq!(serde_json::to_value(canonical).unwrap(), record);

    let page = UiHistoryPage {
        generation: UiGeneration("generation-1".into()),
        revision: 7,
        total_messages: 4,
        history: vec![snapshot.clone()],
        has_older: true,
        has_newer: false,
        first_position: Some(3),
        last_position: Some(3),
    };
    let batch = UiHistoryBatch {
        generation: page.generation.clone(),
        from_revision: 6,
        revision: 7,
        total_messages: 4,
        history: vec![snapshot],
    };
    assert_eq!(page.public_value().unwrap()["history"][0], expected);
    assert_eq!(batch.public_value().unwrap()["history"][0], expected);
}

#[test]
fn snapshot_serialization_retains_tool_presentation_and_error_payloads() {
    let payload = json!({"llmApiResponse": {"text": "application data"}});
    let mut tool = snapshot(json!({
        "role": "tool_lifecycle", "toolCallId": "call-1", "toolName": "example",
        "sequence": 1, "emittedAtMs": 123, "runId": "run-1",
        "stage": "input_ready", "arguments": payload
    }));
    tool.assistant_id = Some(UiMessageId("segment:owner".into()));
    tool.accumulated_arguments = Some(payload.to_string());
    tool.presentation
        .metadata
        .insert("llmApiResponse".into(), payload.clone());
    let value = tool.public_value().unwrap();
    assert_eq!(value["arguments"], payload);
    assert_eq!(value["accumulatedArguments"], payload.to_string());
    assert_eq!(value["assistantId"], "segment:owner");
    assert_eq!(value["presentation"]["metadata"]["llmApiResponse"], payload);

    let error = snapshot(json!({
        "role": "error", "error": {
            "runId": "run-1", "segmentId": "segment-1", "createdAt": 123,
            "raw": "Provider rejected the request", "details": payload, "retryAfterMs": 500
        }
    }));
    assert_eq!(error.public_value().unwrap()["error"]["details"], payload);
}
