//! Integration regressions for round-aware tool-loop detection in both runners.

use std::{pin::Pin, sync::Arc};

use {
    super::helpers::*,
    crate::{
        model::{
            AgentToolControls, ChatMessage, CompletionResponse, LlmProvider, StreamEvent, ToolCall,
            Usage,
        },
        tool_registry::AgentTool,
    },
    anyhow::Result,
    async_trait::async_trait,
    tokio_stream::Stream,
};

const GREP_ERROR: &str =
    "Grep requires an absolute 'path' argument (no workspace root is configured)";

struct IncidentMemorySearchTool;

#[async_trait]
impl AgentTool for IncidentMemorySearchTool {
    fn name(&self) -> &str {
        "memory_search"
    }

    fn description(&self) -> &str {
        "Search memory"
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "query": { "type": "string" }
            },
            "required": ["query"]
        })
    }

    async fn execute(&self, _params: serde_json::Value) -> Result<serde_json::Value> {
        Ok(serde_json::json!({"matches": []}))
    }
}

struct IncidentGrepTool;

#[async_trait]
impl AgentTool for IncidentGrepTool {
    fn name(&self) -> &str {
        "Grep"
    }

    fn description(&self) -> &str {
        "Search file contents"
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "pattern": { "type": "string" },
                "path": {}
            },
            "required": ["pattern"]
        })
    }

    async fn execute(&self, _params: serde_json::Value) -> Result<serde_json::Value> {
        anyhow::bail!(GREP_ERROR)
    }
}

struct RoundAwareLoopProvider {
    call_count: std::sync::atomic::AtomicUsize,
}

impl RoundAwareLoopProvider {
    fn next_call(&self, messages: &[ChatMessage], tools: &[serde_json::Value]) -> ProviderRound {
        let call = self
            .call_count
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        match call {
            0 => {
                assert!(!tools.is_empty(), "initial tool schemas must be available");
                assert!(!history_contains_intervention(messages));
                ProviderRound::Tools(incident_batch())
            },
            1 => {
                assert!(
                    !tools.is_empty(),
                    "ordinary tool schemas must remain available after sibling failures"
                );
                assert!(
                    !history_contains_intervention(messages),
                    "one model round must not trigger a loop intervention"
                );
                assert_eq!(
                    messages
                        .iter()
                        .filter(|message| matches!(message, ChatMessage::Tool { .. }))
                        .count(),
                    3,
                    "all first-round results must be visible to the model"
                );
                ProviderRound::Tools(vec![grep_call(
                    "grep_round_2",
                    "language service diagnostics",
                )])
            },
            2 => {
                assert!(
                    !tools.is_empty(),
                    "stage 1 must keep ordinary tool schemas available"
                );
                assert!(
                    messages.iter().any(|message| matches!(
                        message,
                        ChatMessage::User {
                            content: UserContent::Text(text),
                            ..
                        } if text.contains("Equivalent tool failures were repeated across 2 distinct model rounds")
                    )),
                    "the nudge must appear only after the second failed model round"
                );
                ProviderRound::Tools(vec![grep_call("grep_round_3", "tsserver diagnostics")])
            },
            3 => {
                assert!(tools.is_empty(), "stage 2 must strip tool schemas");
                assert!(
                    messages.iter().any(|message| matches!(
                        message,
                        ChatMessage::User {
                            content: UserContent::Text(text),
                            ..
                        } if text.contains("TOOLS DISABLED FOR THIS TURN")
                    )),
                    "the forced-text turn must include the stage-2 message"
                );
                ProviderRound::Text(
                    "Recovered after the forced text turn. The repeated Grep failures came from a missing absolute path, so I stopped retrying the same operation and will ask the user for the workspace root before using that tool again."
                        .to_string(),
                )
            },
            _ => panic!("unexpected provider call {call}"),
        }
    }
}

#[derive(Debug)]
enum ProviderRound {
    Tools(Vec<ToolCall>),
    Text(String),
}

#[async_trait]
impl LlmProvider for RoundAwareLoopProvider {
    fn name(&self) -> &str {
        "round-aware-loop-provider"
    }

    fn id(&self) -> &str {
        "round-aware-loop-model"
    }

    fn context_window(&self) -> Option<u32> {
        Some(TEST_CONTEXT_WINDOW)
    }

    fn max_input_tokens(&self) -> Option<u32> {
        Some(TEST_MAX_INPUT_TOKENS)
    }

    fn max_output_tokens(&self) -> Option<u32> {
        Some(TEST_MAX_OUTPUT_TOKENS)
    }

    fn supports_tools(&self) -> bool {
        true
    }

    async fn complete(
        &self,
        messages: &[ChatMessage],
        tools: &[serde_json::Value],
    ) -> Result<CompletionResponse> {
        Ok(match self.next_call(messages, tools) {
            ProviderRound::Tools(tool_calls) => CompletionResponse {
                text: None,
                tool_calls,
                usage: Usage::default(),
            },
            ProviderRound::Text(text) => CompletionResponse {
                text: Some(text),
                tool_calls: Vec::new(),
                usage: Usage::default(),
            },
        })
    }

    fn stream(
        &self,
        messages: Vec<ChatMessage>,
    ) -> Pin<Box<dyn Stream<Item = StreamEvent> + Send + '_>> {
        Box::pin(tokio_stream::iter(stream_events(
            self.next_call(&messages, &[]),
        )))
    }

    fn stream_with_tools_and_options(
        &self,
        messages: Vec<ChatMessage>,
        tools: Vec<serde_json::Value>,
        _options: AgentToolControls,
    ) -> Pin<Box<dyn Stream<Item = StreamEvent> + Send + '_>> {
        Box::pin(tokio_stream::iter(stream_events(
            self.next_call(&messages, &tools),
        )))
    }
}

fn incident_batch() -> Vec<ToolCall> {
    vec![
        ToolCall {
            id: "memory_round_1".to_string(),
            name: "memory_search".to_string(),
            arguments: serde_json::json!({"query": "code checker preferences"}),
            argument_diagnostic: None,
            metadata: None,
        },
        grep_call("grep_round_1_a", "code_checker|code checker|codeChecker"),
        grep_call(
            "grep_round_1_b",
            "diagnostic|lsp|language service|tsserver|eslint|ruff|mypy|cargo check",
        ),
    ]
}

fn grep_call(id: &str, pattern: &str) -> ToolCall {
    ToolCall {
        id: id.to_string(),
        name: "Grep".to_string(),
        arguments: serde_json::json!({"pattern": pattern, "path": null}),
        argument_diagnostic: None,
        metadata: None,
    }
}

fn stream_events(round: ProviderRound) -> Vec<StreamEvent> {
    match round {
        ProviderRound::Tools(tool_calls) => {
            let mut events = Vec::with_capacity(tool_calls.len() * 3 + 1);
            for (index, tool_call) in tool_calls.into_iter().enumerate() {
                events.push(StreamEvent::ToolCallStart {
                    id: tool_call.id,
                    name: tool_call.name,
                    index,
                    metadata: None,
                });
                events.push(StreamEvent::ToolCallArgumentsDelta {
                    index,
                    delta: tool_call.arguments.to_string(),
                });
                events.push(StreamEvent::ToolCallComplete { index });
            }
            events.push(StreamEvent::Done(Usage::default()));
            events
        },
        ProviderRound::Text(text) => vec![
            StreamEvent::Delta(text),
            StreamEvent::Done(Usage::default()),
        ],
    }
}

fn incident_tools() -> ToolRegistry {
    let mut tools = ToolRegistry::new();
    tools.register(Box::new(IncidentMemorySearchTool));
    tools.register(Box::new(IncidentGrepTool));
    tools
}

fn intervention_stages(events: &[RunnerEvent]) -> Vec<u8> {
    events
        .iter()
        .filter_map(|event| match event {
            RunnerEvent::LoopInterventionFired { stage, .. } => Some(*stage),
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn non_streaming_runner_counts_incident_batch_as_one_model_round() {
    let provider = Arc::new(RoundAwareLoopProvider {
        call_count: std::sync::atomic::AtomicUsize::new(0),
    });
    let tools = incident_tools();
    let events = Arc::new(std::sync::Mutex::new(Vec::new()));
    let event_sink = Arc::clone(&events);
    let on_event: OnEvent = Box::new(move |event| event_sink.lock().unwrap().push(event));

    let result = run_agent_loop(
        provider,
        &tools,
        "Test bot",
        &UserContent::text("Reproduce the tool-loop incident"),
        Some(&on_event),
        None,
    )
    .await
    .unwrap();

    assert!(
        result
            .text
            .starts_with("Recovered after the forced text turn.")
    );
    assert_eq!(result.iterations, 4);
    assert_eq!(result.tool_calls_made, 5);
    assert_eq!(intervention_stages(&events.lock().unwrap()), vec![1, 2]);
}

#[tokio::test]
async fn streaming_runner_counts_incident_batch_as_one_model_round() {
    let provider = Arc::new(RoundAwareLoopProvider {
        call_count: std::sync::atomic::AtomicUsize::new(0),
    });
    let tools = incident_tools();
    let events = Arc::new(std::sync::Mutex::new(Vec::new()));
    let event_sink = Arc::clone(&events);
    let on_event: OnEvent = Box::new(move |event| event_sink.lock().unwrap().push(event));

    let result = run_agent_loop_streaming(
        provider,
        &tools,
        "Test bot",
        &UserContent::text("Reproduce the tool-loop incident"),
        Some(&on_event),
        None,
        None,
        None,
        None,
        None,
    )
    .await
    .unwrap();

    assert!(
        result
            .text
            .starts_with("Recovered after the forced text turn.")
    );
    assert_eq!(result.iterations, 4);
    assert_eq!(result.tool_calls_made, 5);
    assert_eq!(intervention_stages(&events.lock().unwrap()), vec![1, 2]);
}
