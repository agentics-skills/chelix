//! Tool-call budget enforcement tests.

use std::{
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};

use {anyhow::Result, async_trait::async_trait, tokio_stream::Stream};

use {
    super::{super::ToolCallBudget, helpers::*},
    crate::{
        lazy_tools::wrap_registry_lazy,
        model::{
            AgentToolControls, ChatMessage, CompletionResponse, LlmProvider, StreamEvent, ToolCall,
            Usage, UserContent,
        },
        tool_registry::AgentTool,
    },
};

struct ScriptedToolProvider {
    call_count: AtomicUsize,
    rounds: Vec<ScriptedRound>,
}

#[derive(Clone)]
enum ScriptedRound {
    Tools(Vec<ToolCall>),
    Text(&'static str),
}

impl ScriptedToolProvider {
    fn new(rounds: Vec<ScriptedRound>) -> Self {
        Self {
            call_count: AtomicUsize::new(0),
            rounds,
        }
    }

    fn next_round(&self) -> ScriptedRound {
        let index = self.call_count.fetch_add(1, Ordering::SeqCst);
        self.rounds
            .get(index)
            .cloned()
            .unwrap_or_else(|| panic!("unexpected provider call {index}"))
    }
}

#[async_trait]
impl LlmProvider for ScriptedToolProvider {
    fn name(&self) -> &str {
        "scripted-tool-budget"
    }

    fn id(&self) -> &str {
        "scripted-tool-budget-model"
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
        _messages: &[ChatMessage],
        _tools: &[serde_json::Value],
    ) -> Result<CompletionResponse> {
        Ok(match self.next_round() {
            ScriptedRound::Tools(tool_calls) => CompletionResponse {
                text: None,
                tool_calls,
                usage: Usage::default(),
            },
            ScriptedRound::Text(text) => CompletionResponse {
                text: Some(text.to_string()),
                tool_calls: Vec::new(),
                usage: Usage::default(),
            },
        })
    }

    fn stream(
        &self,
        _messages: Vec<ChatMessage>,
    ) -> Pin<Box<dyn Stream<Item = StreamEvent> + Send + '_>> {
        Box::pin(tokio_stream::iter(stream_events(self.next_round())))
    }

    fn stream_with_tools_and_options(
        &self,
        _messages: Vec<ChatMessage>,
        _tools: Vec<serde_json::Value>,
        _options: AgentToolControls,
    ) -> Pin<Box<dyn Stream<Item = StreamEvent> + Send + '_>> {
        Box::pin(tokio_stream::iter(stream_events(self.next_round())))
    }
}

fn stream_events(round: ScriptedRound) -> Vec<StreamEvent> {
    match round {
        ScriptedRound::Tools(tool_calls) => {
            let mut events = Vec::with_capacity(tool_calls.len() * 3 + 1);
            for (index, tool_call) in tool_calls.into_iter().enumerate() {
                events.push(StreamEvent::ToolCallStart {
                    id: tool_call.id,
                    name: tool_call.name,
                    index,
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
        ScriptedRound::Text(text) => vec![
            StreamEvent::Delta(text.to_string()),
            StreamEvent::Done(Usage::default()),
        ],
    }
}

struct CountingTool {
    name: &'static str,
    executions: Arc<AtomicUsize>,
}

#[async_trait]
impl AgentTool for CountingTool {
    fn name(&self) -> &str {
        self.name
    }

    fn description(&self) -> &str {
        "Count executions"
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "additionalProperties": false,
            "properties": {},
        })
    }

    async fn execute(&self, _params: serde_json::Value) -> Result<serde_json::Value> {
        self.executions.fetch_add(1, Ordering::SeqCst);
        Ok(serde_json::json!({"ok": true}))
    }
}

fn call(id: &str, name: &str, arguments: serde_json::Value) -> ToolCall {
    ToolCall {
        id: id.to_string(),
        name: name.to_string(),
        arguments,
        argument_diagnostic: None,
    }
}

fn threshold_limits(max_tools_threshold: usize) -> AgentLoopLimits {
    AgentLoopLimits {
        max_tools_threshold,
        ..test_agent_loop_limits()
    }
}

fn assert_threshold_error(error: AgentRunError, threshold: usize, used: usize, requested: usize) {
    assert!(matches!(
        error,
        AgentRunError::MaxToolsThresholdReached {
            threshold: actual_threshold,
            used: actual_used,
            requested: actual_requested,
        } if actual_threshold == threshold
            && actual_used == used
            && actual_requested == requested
    ));
}

#[test]
fn tool_call_budget_rejects_oversized_batch_without_changing_used() {
    let mut budget = ToolCallBudget::new(3);

    budget.reserve_batch(2).unwrap();
    let error = budget.reserve_batch(2).unwrap_err();

    assert_threshold_error(error, 3, 2, 2);
    assert_eq!(budget.used(), 2);
    budget.reserve_batch(1).unwrap();
    assert_eq!(budget.used(), 3);
}

#[tokio::test]
async fn non_streaming_rejects_oversized_parallel_batch_before_any_sibling_runs() {
    let executions = Arc::new(AtomicUsize::new(0));
    let provider = Arc::new(ScriptedToolProvider::new(vec![ScriptedRound::Tools(vec![
        call("c1", "count_a", serde_json::json!({})),
        call("c2", "count_b", serde_json::json!({})),
        call("c3", "unknown_tool", serde_json::json!({})),
    ])]));
    let mut tools = ToolRegistry::new();
    tools.register(Box::new(CountingTool {
        name: "count_a",
        executions: Arc::clone(&executions),
    }));
    tools.register(Box::new(CountingTool {
        name: "count_b",
        executions: Arc::clone(&executions),
    }));
    let events = Arc::new(std::sync::Mutex::new(Vec::new()));
    let event_sink = Arc::clone(&events);
    let on_event: OnEvent = Box::new(move |event| event_sink.lock().unwrap().push(event));

    let error = run_agent_loop_with_context_and_limits(
        provider,
        &tools,
        "Test bot",
        &UserContent::text("Run the batch"),
        Some(&on_event),
        None,
        None,
        None,
        None,
        threshold_limits(2),
    )
    .await
    .unwrap_err();

    assert_threshold_error(error, 2, 0, 3);
    assert_eq!(executions.load(Ordering::SeqCst), 0);
    assert!(!events.lock().unwrap().iter().any(|event| matches!(
        event,
        RunnerEvent::ToolCallStart { .. }
            | RunnerEvent::ToolCallEnd { .. }
            | RunnerEvent::ToolCallRejected { .. }
    )));
}

#[tokio::test]
async fn streaming_rejects_oversized_parallel_batch_before_any_sibling_runs() {
    let executions = Arc::new(AtomicUsize::new(0));
    let provider = Arc::new(ScriptedToolProvider::new(vec![ScriptedRound::Tools(vec![
        call("c1", "count_a", serde_json::json!({})),
        call("c2", "count_b", serde_json::json!({})),
        call("c3", "unknown_tool", serde_json::json!({})),
    ])]));
    let mut tools = ToolRegistry::new();
    tools.register(Box::new(CountingTool {
        name: "count_a",
        executions: Arc::clone(&executions),
    }));
    tools.register(Box::new(CountingTool {
        name: "count_b",
        executions: Arc::clone(&executions),
    }));
    let events = Arc::new(std::sync::Mutex::new(Vec::new()));
    let event_sink = Arc::clone(&events);
    let on_event: OnEvent = Box::new(move |event| event_sink.lock().unwrap().push(event));

    let error = run_agent_loop_streaming_with_limits(
        provider,
        &tools,
        "Test bot",
        &UserContent::text("Run the batch"),
        Some(&on_event),
        None,
        None,
        None,
        None,
        None,
        threshold_limits(2),
    )
    .await
    .unwrap_err();

    assert_threshold_error(error, 2, 0, 3);
    assert_eq!(executions.load(Ordering::SeqCst), 0);
    assert!(!events.lock().unwrap().iter().any(|event| matches!(
        event,
        RunnerEvent::ToolCallStart { .. }
            | RunnerEvent::ToolCallEnd { .. }
            | RunnerEvent::ToolCallRejected { .. }
    )));
}

#[tokio::test]
async fn full_budget_still_allows_a_final_text_only_model_round() {
    let provider = Arc::new(ScriptedToolProvider::new(vec![
        ScriptedRound::Tools(vec![call("c1", "unknown_tool", serde_json::json!({}))]),
        ScriptedRound::Text("Final answer after the full tool budget."),
    ]));

    let result = run_agent_loop_with_context_and_limits(
        provider,
        &ToolRegistry::new(),
        "Test bot",
        &UserContent::text("Finish after the tool call"),
        None,
        None,
        None,
        None,
        None,
        threshold_limits(1),
    )
    .await
    .unwrap();

    assert_eq!(result.text, "Final answer after the full tool budget.");
    assert_eq!(result.tool_calls_made, 1);
    assert_eq!(result.iterations, 2);
}

#[tokio::test]
async fn rejected_recognized_calls_consume_the_budget() {
    let provider = Arc::new(ScriptedToolProvider::new(vec![
        ScriptedRound::Tools(vec![call(
            "c1",
            "echo_tool",
            serde_json::json!({"text": 7}),
        )]),
        ScriptedRound::Tools(vec![call("c2", "echo_tool", serde_json::json!({}))]),
    ]));
    let mut tools = ToolRegistry::new();
    tools.register(Box::new(EchoTool));

    let error = run_agent_loop_with_context_and_limits(
        provider,
        &tools,
        "Test bot",
        &UserContent::text("Retry rejected calls"),
        None,
        None,
        None,
        None,
        None,
        threshold_limits(1),
    )
    .await
    .unwrap_err();

    assert_threshold_error(error, 1, 1, 1);
}

#[tokio::test]
async fn lazy_get_tool_and_revealed_target_each_consume_one_unit() {
    let executions = Arc::new(AtomicUsize::new(0));
    let mut registry = ToolRegistry::new();
    registry.register(Box::new(CountingTool {
        name: "count_target",
        executions: Arc::clone(&executions),
    }));
    let lazy_registry = wrap_registry_lazy(registry).unwrap();
    let provider = Arc::new(ScriptedToolProvider::new(vec![
        ScriptedRound::Tools(vec![call(
            "get_1",
            "get_tool",
            serde_json::json!({"name": "count_target"}),
        )]),
        ScriptedRound::Tools(vec![call(
            "target_1",
            "count_target",
            serde_json::json!({}),
        )]),
        ScriptedRound::Text("Lazy flow complete."),
    ]));

    let result = run_agent_loop_with_context_and_limits(
        provider,
        &lazy_registry,
        "Test bot",
        &UserContent::text("Reveal and run the target"),
        None,
        None,
        None,
        None,
        None,
        threshold_limits(2),
    )
    .await
    .unwrap();

    assert_eq!(result.text, "Lazy flow complete.");
    assert_eq!(result.tool_calls_made, 2);
    assert_eq!(executions.load(Ordering::SeqCst), 1);
}
