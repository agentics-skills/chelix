//! Live integration tests for the Z.AI (Zhipu) provider.
//!
//! Requires `Z_API_KEY`. Run with:
//!   cargo test --test zai_integration -- --ignored

#![allow(clippy::unwrap_used, clippy::expect_used)]

#[path = "support/reasoning.rs"]
mod reasoning;

use std::sync::Arc;

use {
    chelix_agents::model::{ChatMessage, LlmProvider, StreamEvent, ToolCall},
    chelix_providers::openai::OpenAiProvider,
    futures::StreamExt,
    secrecy::Secret,
};

const BASE_URL: &str = "https://api.z.ai/api/paas/v4";
const TEST_MODEL: &str = "glm-4.5-flash";

fn api_key() -> Secret<String> {
    Secret::new(std::env::var("Z_API_KEY").expect("Z_API_KEY must be set for integration tests"))
}

fn make_provider(model: &str) -> Arc<dyn LlmProvider> {
    reasoning::configure(
        OpenAiProvider::new_with_name(
            api_key(),
            model.to_string(),
            BASE_URL.to_string(),
            "zai".to_string(),
        ),
        vec!["off".into()],
        "off".into(),
    )
}

fn weather_tool() -> serde_json::Value {
    serde_json::json!({
        "name": "get_weather",
        "description": "Get current weather for a location. You MUST call this tool when asked about weather.",
        "parameters": {
            "type": "object",
            "properties": {
                "location": { "type": "string", "description": "City name" }
            },
            "required": ["location"]
        }
    })
}

// ── System prompt ────────────────────────────────────────────────────────────

#[tokio::test]
#[ignore]
async fn system_prompt_is_received_collected_stream() {
    let p = make_provider(TEST_MODEL);
    let keyword = "LOQUAT";
    let messages = vec![
        ChatMessage::system(format!(
            "You MUST include the exact word \"{keyword}\" in every response, no matter what."
        )),
        ChatMessage::user("What is 2+2?"),
    ];
    let response = chelix_agents::model::collect_stream(p.stream(messages))
        .await
        .expect("should succeed");
    let text = response.text.expect("must have text");
    assert!(
        text.to_lowercase().contains(&keyword.to_lowercase()),
        "system prompt not received: {text:?}"
    );
}

#[tokio::test]
#[ignore]
async fn system_prompt_is_received_streaming() {
    let p = make_provider(TEST_MODEL);
    let keyword = "MULBERRY";
    let messages = vec![
        ChatMessage::system(format!(
            "You MUST include the exact word \"{keyword}\" in every response, no matter what."
        )),
        ChatMessage::user("What is 3+3?"),
    ];
    let mut stream = p.stream_with_tools(messages, vec![]);
    let mut full_text = String::new();
    let mut saw_done = false;
    while let Some(event) = stream.next().await {
        match event {
            StreamEvent::Delta(chunk) => full_text.push_str(&chunk),
            StreamEvent::Done(_) => {
                saw_done = true;
                break;
            },
            StreamEvent::Error(err) => panic!("stream error: {err}"),
            _ => {},
        }
    }
    assert!(saw_done, "stream must emit Done");
    assert!(
        full_text.to_lowercase().contains(&keyword.to_lowercase()),
        "system prompt not received: {full_text:?}"
    );
}

// ── Tool calling ─────────────────────────────────────────────────────────────

#[tokio::test]
#[ignore]
async fn tool_call_round_trip_collected_stream() {
    let p = make_provider(TEST_MODEL);
    let response = chelix_agents::model::collect_stream(p.stream_with_tools(
        vec![ChatMessage::user(
            "What's the weather in Tokyo? Use the get_weather tool.",
        )],
        vec![weather_tool()],
    ))
    .await
    .expect("should succeed");
    assert!(
        !response.tool_calls.is_empty(),
        "should call tool, got text: {:?}",
        response.text
    );
    assert_eq!(response.tool_calls[0].name, "get_weather");
}

#[tokio::test]
#[ignore]
async fn tool_call_round_trip_streaming() {
    let p = make_provider(TEST_MODEL);
    let mut stream = p.stream_with_tools(
        vec![ChatMessage::user(
            "What's the weather in Paris? Use the get_weather tool.",
        )],
        vec![weather_tool()],
    );
    let mut saw_tool = false;
    let mut saw_done = false;
    let mut tool_name = String::new();
    while let Some(event) = stream.next().await {
        match event {
            StreamEvent::ToolCallStart { name, .. } => {
                saw_tool = true;
                tool_name = name;
            },
            StreamEvent::Done(_) => {
                saw_done = true;
                break;
            },
            StreamEvent::Error(err) => panic!("stream error: {err}"),
            _ => {},
        }
    }
    assert!(saw_done, "must emit Done");
    assert!(saw_tool, "should include tool call");
    assert_eq!(tool_name, "get_weather");
}

#[tokio::test]
#[ignore]
async fn multi_turn_tool_use() {
    let p = make_provider(TEST_MODEL);
    let tools = vec![weather_tool()];
    let r = chelix_agents::model::collect_stream(p.stream_with_tools(
        vec![ChatMessage::user("Weather in London? Use get_weather.")],
        tools.clone(),
    ))
    .await
    .expect("first turn");
    assert!(!r.tool_calls.is_empty(), "should call tool");
    let tc = &r.tool_calls[0];
    let r2 = chelix_agents::model::collect_stream(p.stream_with_tools(
        vec![
            ChatMessage::user("Weather in London? Use get_weather."),
            ChatMessage::assistant_with_tools(r.text.clone(), vec![ToolCall {
                id: tc.id.clone(),
                name: tc.name.clone(),
                arguments: tc.arguments.clone(),
                argument_diagnostic: tc.argument_diagnostic.clone(),
            }]),
            ChatMessage::tool(&tc.id, r#"{"temperature": 15, "condition": "cloudy"}"#),
        ],
        tools.clone(),
    ))
    .await
    .expect("second turn");
    assert!(r2.text.is_some(), "should have text after tool result");
}

// ── Streaming ────────────────────────────────────────────────────────────────

#[tokio::test]
#[ignore]
async fn stream_emits_delta_and_done() {
    let p = make_provider(TEST_MODEL);
    let mut stream = p.stream(vec![ChatMessage::user("Say hello in one word.")]);
    let mut saw_delta = false;
    let mut saw_done = false;
    while let Some(event) = stream.next().await {
        match event {
            StreamEvent::Delta(_) => saw_delta = true,
            StreamEvent::Done(_) => {
                saw_done = true;
                break;
            },
            StreamEvent::Error(err) => panic!("stream error: {err}"),
            _ => {},
        }
    }
    assert!(saw_delta && saw_done);
}
