//! Basic runner tests: parsing, shell commands, sanitization, tool results, vision.

use std::sync::Arc;

use {
    super::helpers::*,
    crate::{
        model::{
            AgentToolControls, ChatMessage, CompletionOptions, CompletionResponse, LlmProvider,
            StreamEvent, ToolCall, Usage,
        },
        tool_parsing::new_synthetic_tool_call_id,
    },
    anyhow::Result,
    async_trait::async_trait,
    chelix_common::hooks::{HookAction, HookEvent, HookHandler, HookPayload, HookRegistry},
    std::pin::Pin,
    tokio_stream::Stream,
};

// ── parse_tool_call_from_text tests (delegates to tool_parsing) ──

#[test]
fn test_parse_tool_call_basic() {
    let text =
        "```tool_call\n{\"tool\": \"execute_command\", \"arguments\": {\"command\": \"ls\"}}\n```";
    let (tc, remaining) = parse_tool_call_from_text(text).unwrap();
    assert_eq!(tc.name, "execute_command");
    assert_eq!(tc.arguments["command"], "ls");
    assert!(tc.id.len() <= 40);
    assert!(remaining.is_none() || remaining.as_deref() == Some(""));
}

#[test]
fn test_parse_tool_call_with_surrounding_text() {
    let text = "I'll run ls for you.\n```tool_call\n{\"tool\": \"execute_command\", \"arguments\": {\"command\": \"ls\"}}\n```\nHere you go.";
    let (tc, remaining) = parse_tool_call_from_text(text).unwrap();
    assert_eq!(tc.name, "execute_command");
    let remaining = remaining.unwrap();
    assert!(remaining.contains("I'll run ls"));
    assert!(remaining.contains("Here you go"));
}

#[test]
fn test_parse_tool_call_no_block() {
    let text = "I would run ls but I can't.";
    assert!(parse_tool_call_from_text(text).is_none());
}

#[test]
fn test_parse_tool_call_invalid_json() {
    let text = "```tool_call\nnot json\n```";
    assert!(parse_tool_call_from_text(text).is_none());
}

#[test]
fn test_parse_tool_call_function_block() {
    let text = "<function=process>\n<parameter=action>\nstart\n</parameter>\n<parameter=command>\npwd\n</parameter>\n</function>";
    let (tc, remaining) = parse_tool_call_from_text(text).unwrap();
    assert_eq!(tc.name, "process");
    assert_eq!(tc.arguments["action"], "start");
    assert_eq!(tc.arguments["command"], "pwd");
    assert!(tc.id.len() <= 40);
    assert!(remaining.is_none() || remaining.as_deref() == Some(""));
}

#[test]
fn test_new_synthetic_tool_call_id_is_openai_compatible() {
    let id = new_synthetic_tool_call_id("forced");
    assert!(id.starts_with("forced_"));
    assert!(id.len() <= 40);

    let long_prefix_id = new_synthetic_tool_call_id(
        "prefix_that_is_intentionally_way_too_long_for_openai_tool_call_ids",
    );
    assert!(long_prefix_id.len() <= 40);
}

#[test]
fn test_parse_tool_call_function_block_with_wrapper_and_text() {
    let text = "I'll do it.\n<tool_call>\n<function=process>\n<parameter=action>start</parameter>\n<parameter=command>pwd</parameter>\n</function>\n</tool_call>\nDone.";
    let (tc, remaining) = parse_tool_call_from_text(text).unwrap();
    assert_eq!(tc.name, "process");
    assert_eq!(tc.arguments["action"], "start");
    assert_eq!(tc.arguments["command"], "pwd");
    let remaining = remaining.unwrap();
    assert!(remaining.contains("I'll do it."));
    assert!(remaining.contains("Done."));
    assert!(!remaining.contains("<tool_call>"));
    assert!(!remaining.contains("</tool_call>"));
}

// ── Tests ────────────────────────────────────────────────────────

#[tokio::test]
async fn test_simple_text_response() {
    let provider = Arc::new(MockProvider {
        response_text: "Hello!".into(),
    });
    let tools = ToolRegistry::new();
    let uc = UserContent::text("Hi");
    let result = run_agent_loop(provider, &tools, "You are a test bot.", &uc, None, None)
        .await
        .unwrap();
    assert_eq!(result.output.text, "Hello!");
    assert_eq!(result.iterations, 1);
    assert_eq!(result.tool_calls_made, 0);
}

struct NoToolsRoutingProvider {
    complete_calls: std::sync::atomic::AtomicUsize,
    complete_with_options_calls: std::sync::atomic::AtomicUsize,
}

#[async_trait]
impl LlmProvider for NoToolsRoutingProvider {
    fn name(&self) -> &str {
        "no-tools-routing"
    }

    fn id(&self) -> &str {
        "no-tools-routing-model"
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
        tools: &[serde_json::Value],
    ) -> Result<CompletionResponse> {
        self.complete_calls
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        assert!(tools.is_empty());
        Ok(CompletionResponse {
            text: Some("no tools".into()),
            tool_calls: vec![],
            usage: Usage::default(),
        })
    }

    async fn complete_with_options(
        &self,
        _messages: &[ChatMessage],
        _tools: &[serde_json::Value],
        _options: &CompletionOptions,
    ) -> Result<CompletionResponse> {
        self.complete_with_options_calls
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        anyhow::bail!("with-tools path must not be used for empty schemas")
    }

    fn stream(
        &self,
        _messages: Vec<ChatMessage>,
    ) -> Pin<Box<dyn Stream<Item = StreamEvent> + Send + '_>> {
        Box::pin(tokio_stream::empty())
    }
}

#[tokio::test]
async fn test_non_streaming_runner_does_not_use_tools_path_for_empty_schema_list() {
    let provider = Arc::new(NoToolsRoutingProvider {
        complete_calls: std::sync::atomic::AtomicUsize::new(0),
        complete_with_options_calls: std::sync::atomic::AtomicUsize::new(0),
    });
    let tools = ToolRegistry::new();
    let uc = UserContent::text("Hi");

    let result = run_agent_loop(
        provider.clone(),
        &tools,
        "You are a test bot.",
        &uc,
        None,
        None,
    )
    .await
    .unwrap();

    assert_eq!(result.output.text, "no tools");
    assert_eq!(
        provider
            .complete_calls
            .load(std::sync::atomic::Ordering::SeqCst),
        1
    );
    assert_eq!(
        provider
            .complete_with_options_calls
            .load(std::sync::atomic::Ordering::SeqCst),
        0
    );
}

struct NoToolsStreamingRoutingProvider {
    stream_calls: std::sync::atomic::AtomicUsize,
    stream_with_options_calls: std::sync::atomic::AtomicUsize,
}

#[async_trait]
impl LlmProvider for NoToolsStreamingRoutingProvider {
    fn name(&self) -> &str {
        "no-tools-streaming-routing"
    }

    fn id(&self) -> &str {
        "no-tools-streaming-routing-model"
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
        Ok(CompletionResponse {
            text: Some("unused".into()),
            tool_calls: vec![],
            usage: Usage::default(),
        })
    }

    fn stream(
        &self,
        _messages: Vec<ChatMessage>,
    ) -> Pin<Box<dyn Stream<Item = StreamEvent> + Send + '_>> {
        self.stream_calls
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Box::pin(tokio_stream::iter(vec![
            StreamEvent::Delta("no tools".into()),
            StreamEvent::Done(Usage::default()),
        ]))
    }

    fn stream_with_tools_and_options(
        &self,
        _messages: Vec<ChatMessage>,
        _tools: Vec<serde_json::Value>,
        _options: AgentToolControls,
    ) -> Pin<Box<dyn Stream<Item = StreamEvent> + Send + '_>> {
        self.stream_with_options_calls
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Box::pin(tokio_stream::iter(vec![StreamEvent::Error(
            "with-tools path must not be used for empty schemas".into(),
        )]))
    }
}

#[tokio::test]
async fn test_streaming_runner_does_not_use_tools_path_for_empty_schema_list() {
    let provider = Arc::new(NoToolsStreamingRoutingProvider {
        stream_calls: std::sync::atomic::AtomicUsize::new(0),
        stream_with_options_calls: std::sync::atomic::AtomicUsize::new(0),
    });
    let tools = ToolRegistry::new();
    let uc = UserContent::text("Hi");

    let result = run_agent_loop_streaming(
        provider.clone(),
        &tools,
        "You are a test bot.",
        &uc,
        None,
        None,
        None,
        None,
        None,
        None,
    )
    .await
    .unwrap();

    assert_eq!(result.output.text, "no tools");
    assert_eq!(
        provider
            .stream_calls
            .load(std::sync::atomic::Ordering::SeqCst),
        1
    );
    assert_eq!(
        provider
            .stream_with_options_calls
            .load(std::sync::atomic::Ordering::SeqCst),
        0
    );
}

#[derive(Clone, Copy)]
enum TerminalResponsesOutput {
    VisibleReasoning,
    OpaqueReasoning,
}

struct IterationOwnedResponsesProvider {
    stream_calls: std::sync::atomic::AtomicUsize,
    terminal_output: TerminalResponsesOutput,
}

#[async_trait]
impl LlmProvider for IterationOwnedResponsesProvider {
    fn name(&self) -> &str {
        "iteration-owned-responses"
    }

    fn id(&self) -> &str {
        "iteration-owned-responses-model"
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
        anyhow::bail!("streaming runner must not call complete")
    }

    fn stream(
        &self,
        _messages: Vec<ChatMessage>,
    ) -> Pin<Box<dyn Stream<Item = StreamEvent> + Send + '_>> {
        Box::pin(tokio_stream::iter(self.terminal_events()))
    }

    fn stream_with_tools_and_options(
        &self,
        _messages: Vec<ChatMessage>,
        _tools: Vec<serde_json::Value>,
        _options: AgentToolControls,
    ) -> Pin<Box<dyn Stream<Item = StreamEvent> + Send + '_>> {
        let call = self
            .stream_calls
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        if call == 0 {
            return Box::pin(tokio_stream::iter(vec![
                StreamEvent::Delta("Answer owned by the tool iteration.".into()),
                StreamEvent::ToolCallStart {
                    id: "call_iteration_owned".into(),
                    name: "echo_tool".into(),
                    index: 0,
                },
                StreamEvent::ToolCallArgumentsDelta {
                    index: 0,
                    delta: r#"{"text":"hello"}"#.into(),
                },
                StreamEvent::ToolCallComplete { index: 0 },
                StreamEvent::Done(Usage::default()),
            ]));
        }
        Box::pin(tokio_stream::iter(self.terminal_events()))
    }
}

impl IterationOwnedResponsesProvider {
    fn terminal_events(&self) -> Vec<StreamEvent> {
        let mut events = match self.terminal_output {
            TerminalResponsesOutput::VisibleReasoning => {
                vec![StreamEvent::ResponsesReasoningDelta {
                    item_id: "rs_terminal".into(),
                    output_index: 0,
                    summary_index: 0,
                    delta: "Terminal iteration reasoning.".into(),
                }]
            },
            TerminalResponsesOutput::OpaqueReasoning => {
                vec![StreamEvent::ResponsesReasoningItem(
                    chelix_common::ResponsesReasoningItem {
                        id: "rs_terminal".into(),
                        encrypted_content: "opaque-terminal".into(),
                    },
                )]
            },
        };
        events.push(StreamEvent::Done(Usage::default()));
        events
    }
}

async fn run_iteration_owned_responses_provider(
    terminal_output: TerminalResponsesOutput,
) -> AgentRunResult {
    let provider = Arc::new(IterationOwnedResponsesProvider {
        stream_calls: std::sync::atomic::AtomicUsize::new(0),
        terminal_output,
    });
    let mut tools = ToolRegistry::new();
    tools.register(Box::new(EchoTool));

    run_agent_loop_streaming(
        provider,
        &tools,
        "You are a test bot.",
        &UserContent::text("Use the tool."),
        None,
        None,
        None,
        None,
        None,
        None,
    )
    .await
    .unwrap()
}

#[tokio::test]
async fn test_reasoning_only_terminal_iteration_is_a_new_segment() {
    let result =
        run_iteration_owned_responses_provider(TerminalResponsesOutput::VisibleReasoning).await;

    assert_eq!(result.output.text, "");
    assert_eq!(
        result.output.reasoning,
        Some(chelix_common::ReasoningContent::Parts(vec![
            "Terminal iteration reasoning.".to_string(),
        ]))
    );
    assert!(result.output.responses_reasoning.is_empty());
    assert_eq!(
        result.final_text_source,
        super::super::FinalTextSource::NewSegment
    );
    assert_eq!(result.iterations, 2);
}

#[tokio::test]
async fn test_opaque_only_terminal_iteration_is_a_new_segment() {
    let result =
        run_iteration_owned_responses_provider(TerminalResponsesOutput::OpaqueReasoning).await;

    assert_eq!(result.output.text, "");
    assert!(result.output.reasoning.is_none());
    assert_eq!(result.output.responses_reasoning.len(), 1);
    assert_eq!(result.output.responses_reasoning[0].id, "rs_terminal");
    assert_eq!(
        result.output.responses_reasoning[0].encrypted_content,
        "opaque-terminal"
    );
    assert_eq!(
        result.final_text_source,
        super::super::FinalTextSource::NewSegment
    );
    assert_eq!(result.iterations, 2);
}

struct ToolCallContextStreamingProvider {
    stream_calls: std::sync::atomic::AtomicUsize,
}

#[async_trait]
impl LlmProvider for ToolCallContextStreamingProvider {
    fn name(&self) -> &str {
        "tool-call-context-streaming"
    }

    fn id(&self) -> &str {
        "tool-call-context-streaming-model"
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
        anyhow::bail!("streaming runner must not call complete")
    }

    fn stream(
        &self,
        _messages: Vec<ChatMessage>,
    ) -> Pin<Box<dyn Stream<Item = StreamEvent> + Send + '_>> {
        Box::pin(tokio_stream::iter(vec![
            StreamEvent::Delta("The streaming tool context test completed successfully.".into()),
            StreamEvent::Done(Usage::default()),
        ]))
    }

    fn stream_with_tools_and_options(
        &self,
        _messages: Vec<ChatMessage>,
        _tools: Vec<serde_json::Value>,
        _options: AgentToolControls,
    ) -> Pin<Box<dyn Stream<Item = StreamEvent> + Send + '_>> {
        let call = self
            .stream_calls
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        if call == 0 {
            Box::pin(tokio_stream::iter(vec![
                StreamEvent::ToolCallStart {
                    id: "call_runtime_context".into(),
                    name: "capture_context".into(),
                    index: 0,
                },
                StreamEvent::ToolCallArgumentsDelta {
                    index: 0,
                    delta: "{}".into(),
                },
                StreamEvent::ToolCallComplete { index: 0 },
                StreamEvent::Done(Usage::default()),
            ]))
        } else {
            self.stream(Vec::new())
        }
    }
}

struct CaptureToolCallContext {
    arguments: Arc<std::sync::Mutex<Option<serde_json::Value>>>,
}

#[async_trait]
impl crate::tool_registry::AgentTool for CaptureToolCallContext {
    fn name(&self) -> &str {
        "capture_context"
    }

    fn description(&self) -> &str {
        "Capture the runner-injected tool call context"
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "additionalProperties": false,
            "properties": {}
        })
    }

    async fn execute(&self, params: serde_json::Value) -> Result<serde_json::Value> {
        *self.arguments.lock().unwrap() = Some(params);
        Ok(serde_json::json!({ "captured": true }))
    }
}

#[tokio::test]
async fn test_streaming_runner_injects_tool_call_id_only_into_execution_context() {
    let provider = Arc::new(ToolCallContextStreamingProvider {
        stream_calls: std::sync::atomic::AtomicUsize::new(0),
    });
    let captured = Arc::new(std::sync::Mutex::new(None));
    let mut tools = ToolRegistry::new();
    tools.register(Box::new(CaptureToolCallContext {
        arguments: Arc::clone(&captured),
    }));
    let lifecycle_events = Arc::new(std::sync::Mutex::new(Vec::new()));
    let on_tool_lifecycle = recording_tool_lifecycle(&lifecycle_events);
    let mut hooks = HookRegistry::new();
    hooks.register(Arc::new(RewriteToolArgsHook {
        replacement: serde_json::json!({}),
    }));

    let result = run_agent_loop_streaming_with_tool_lifecycle(
        provider,
        &tools,
        "You are a test bot.",
        &UserContent::text("capture context"),
        None,
        Some(&on_tool_lifecycle),
        None,
        Some(serde_json::json!({
            "_session_key": "session-runtime-context",
            "_run_id": "run-runtime-context"
        })),
        Some(Arc::new(hooks)),
        None,
        None,
    )
    .await
    .unwrap();

    assert_eq!(
        result.output.text,
        "The streaming tool context test completed successfully."
    );
    let captured = captured.lock().unwrap();
    let arguments = captured.as_ref().expect("tool must receive arguments");
    assert_eq!(arguments["_session_key"], "session-runtime-context");
    assert_eq!(arguments["_run_id"], "run-runtime-context");
    assert_eq!(arguments["_tool_call_id"], "call_runtime_context");
    let lifecycle_events = lifecycle_events.lock().unwrap();
    let invocation_events = lifecycle_events
        .iter()
        .filter(|event| event.lifecycle.tool_call_id == "call_runtime_context")
        .collect::<Vec<_>>();
    assert!(matches!(
        invocation_events[0].lifecycle.update,
        chelix_common::tool_lifecycle::ToolLifecycleUpdate::Created {
            provider_index: Some(0)
        }
    ));
    assert_eq!(invocation_events[0].lifecycle.sequence, 0);
    assert!(matches!(
        &invocation_events[1].lifecycle.update,
        chelix_common::tool_lifecycle::ToolLifecycleUpdate::InputStreaming {
            arguments_delta,
        } if arguments_delta == "{}"
    ));
    assert_eq!(invocation_events[1].lifecycle.sequence, 1);
    let input_ready = match &invocation_events[2].lifecycle.update {
        chelix_common::tool_lifecycle::ToolLifecycleUpdate::InputReady { arguments } => arguments,
        other => panic!("expected input-ready lifecycle event, got {other:?}"),
    };
    assert_eq!(invocation_events[2].lifecycle.sequence, 2);
    assert!(input_ready.get("_tool_call_id").is_none());
}

#[tokio::test]
async fn test_waiting_for_execution_receipt_blocks_tool_dispatch() {
    let provider = Arc::new(ToolCallContextStreamingProvider {
        stream_calls: std::sync::atomic::AtomicUsize::new(0),
    });
    let captured = Arc::new(std::sync::Mutex::new(None));
    let mut tools = ToolRegistry::new();
    tools.register(Box::new(CaptureToolCallContext {
        arguments: Arc::clone(&captured),
    }));
    let waiting_seen = Arc::new(tokio::sync::Notify::new());
    let release_waiting = Arc::new(tokio::sync::Notify::new());
    let on_tool_lifecycle: OnToolLifecycle = {
        let waiting_seen = Arc::clone(&waiting_seen);
        let release_waiting = Arc::clone(&release_waiting);
        Arc::new(move |event| {
            let waiting_seen = Arc::clone(&waiting_seen);
            let release_waiting = Arc::clone(&release_waiting);
            Box::pin(async move {
                if event.lifecycle.stage()
                    == chelix_common::tool_lifecycle::ToolLifecycleStage::WaitingForExecution
                {
                    waiting_seen.notify_one();
                    release_waiting.notified().await;
                }
                Ok(())
            })
        })
    };

    let user_content = UserContent::text("capture context");
    let run = run_agent_loop_streaming_with_tool_lifecycle(
        provider,
        &tools,
        "You are a test bot.",
        &user_content,
        None,
        Some(&on_tool_lifecycle),
        None,
        None,
        None,
        None,
        None,
    );
    tokio::pin!(run);
    tokio::select! {
        () = waiting_seen.notified() => {},
        _ = &mut run => panic!("runner crossed the waiting boundary early"),
    }
    assert!(
        captured.lock().unwrap().is_none(),
        "tool execution must not start before the lifecycle receipt"
    );

    release_waiting.notify_waiters();
    let result = run.await.unwrap();
    assert_eq!(
        result.output.text,
        "The streaming tool context test completed successfully."
    );
    assert!(captured.lock().unwrap().is_some());
}

#[tokio::test]
async fn test_non_streaming_runner_dispatches_before_agent_start_hook() {
    let provider = Arc::new(MockProvider {
        response_text: "Hello!".into(),
    });
    let tools = ToolRegistry::new();
    let payloads = Arc::new(std::sync::Mutex::new(Vec::new()));
    let mut hooks = HookRegistry::new();
    hooks.register(Arc::new(AgentStartRecordingHook {
        payloads: Arc::clone(&payloads),
    }));

    let result = run_agent_loop_with_context(
        provider,
        &tools,
        "You are a test bot.",
        &UserContent::text("Hi"),
        None,
        None,
        Some(serde_json::json!({"_session_key": "session-123"})),
        Some(Arc::new(hooks)),
        None,
    )
    .await
    .unwrap();

    assert_eq!(result.output.text, "Hello!");
    let payloads = payloads.lock().unwrap();
    assert_eq!(payloads.len(), 1);
    assert!(matches!(
        &payloads[0],
        HookPayload::BeforeAgentStart { session_key, model }
            if session_key == "session-123" && model == "mock-model"
    ));
}

struct InjectBeforeLlmSystemHook;

#[async_trait]
impl HookHandler for InjectBeforeLlmSystemHook {
    fn name(&self) -> &str {
        "inject-before-llm-system-hook"
    }

    fn events(&self) -> &[HookEvent] {
        static EVENTS: [HookEvent; 1] = [HookEvent::BeforeLLMCall];
        &EVENTS
    }

    async fn handle(
        &self,
        _event: HookEvent,
        payload: &HookPayload,
    ) -> chelix_common::error::Result<HookAction> {
        let HookPayload::BeforeLLMCall { messages, .. } = payload else {
            return Ok(HookAction::Continue);
        };
        let mut messages = messages.as_array().cloned().unwrap_or_default();
        messages.insert(
            0,
            serde_json::json!({"role": "system", "content": "hook-injected system"}),
        );
        Ok(HookAction::ModifyPayload(
            serde_json::json!({"messages": messages}),
        ))
    }
}

struct RecordingMessagesProvider {
    messages: Arc<std::sync::Mutex<Vec<ChatMessage>>>,
}

#[async_trait]
impl LlmProvider for RecordingMessagesProvider {
    fn name(&self) -> &str {
        "recording-messages"
    }

    fn id(&self) -> &str {
        "recording-messages-model"
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

    async fn complete(
        &self,
        messages: &[ChatMessage],
        _tools: &[serde_json::Value],
    ) -> Result<CompletionResponse> {
        *self.messages.lock().unwrap() = messages.to_vec();
        Ok(CompletionResponse {
            text: Some("ok".into()),
            tool_calls: vec![],
            usage: Usage::default(),
        })
    }

    fn stream(
        &self,
        messages: Vec<ChatMessage>,
    ) -> Pin<Box<dyn Stream<Item = StreamEvent> + Send + '_>> {
        *self.messages.lock().unwrap() = messages;
        Box::pin(tokio_stream::iter(vec![
            StreamEvent::Delta("ok".into()),
            StreamEvent::Done(Usage::default()),
        ]))
    }

    fn stream_with_tools(
        &self,
        messages: Vec<ChatMessage>,
        _tools: Vec<serde_json::Value>,
    ) -> Pin<Box<dyn Stream<Item = StreamEvent> + Send + '_>> {
        self.stream(messages)
    }
}

#[tokio::test]
async fn test_before_llm_call_modify_payload_updates_non_streaming_messages() {
    let recorded_messages = Arc::new(std::sync::Mutex::new(Vec::new()));
    let provider = Arc::new(RecordingMessagesProvider {
        messages: Arc::clone(&recorded_messages),
    });
    let tools = ToolRegistry::new();
    let mut hooks = HookRegistry::new();
    hooks.register(Arc::new(InjectBeforeLlmSystemHook));

    let result = run_agent_loop_with_context(
        provider,
        &tools,
        "original system",
        &UserContent::text("hello"),
        None,
        None,
        None,
        Some(Arc::new(hooks)),
        None,
    )
    .await
    .unwrap();

    assert_eq!(result.output.text, "ok");
    let messages = recorded_messages.lock().unwrap();
    assert!(matches!(
        messages.first(),
        Some(ChatMessage::System { content }) if content == "hook-injected system"
    ));
}

#[tokio::test]
async fn test_before_llm_call_modify_payload_updates_streaming_messages() {
    let recorded_messages = Arc::new(std::sync::Mutex::new(Vec::new()));
    let provider = Arc::new(RecordingMessagesProvider {
        messages: Arc::clone(&recorded_messages),
    });
    let tools = ToolRegistry::new();
    let mut hooks = HookRegistry::new();
    hooks.register(Arc::new(InjectBeforeLlmSystemHook));

    let result = run_agent_loop_streaming(
        provider,
        &tools,
        "original system",
        &UserContent::text("hello"),
        None,
        None,
        None,
        Some(Arc::new(hooks)),
        None,
        None,
    )
    .await
    .unwrap();

    assert_eq!(result.output.text, "ok");
    let messages = recorded_messages.lock().unwrap();
    assert!(matches!(
        messages.first(),
        Some(ChatMessage::System { content }) if content == "hook-injected system"
    ));
}

#[test]
fn test_before_llm_call_modify_payload_rejects_invalid_messages() {
    let mut messages = vec![ChatMessage::system("original system")];

    let error = apply_before_llm_call_modify_payload(
        &mut messages,
        serde_json::json!({"messages": [{"role": "invalid", "content": "ignored"}]}),
    )
    .expect_err("invalid hook messages must fail");

    assert!(error.to_string().contains("no valid messages"));
    assert_eq!(messages.len(), 1);
    assert!(matches!(
        messages.first(),
        Some(ChatMessage::System { content }) if content == "original system"
    ));
}

struct StreamingUsageProvider;

#[async_trait]
impl LlmProvider for StreamingUsageProvider {
    fn name(&self) -> &str {
        "streaming-usage"
    }

    fn id(&self) -> &str {
        "streaming-usage-model"
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

    async fn complete(
        &self,
        _messages: &[ChatMessage],
        _tools: &[serde_json::Value],
    ) -> Result<CompletionResponse> {
        Ok(CompletionResponse {
            text: Some("unused".into()),
            tool_calls: vec![],
            usage: Usage::default(),
        })
    }

    fn stream(
        &self,
        _messages: Vec<ChatMessage>,
    ) -> Pin<Box<dyn Stream<Item = StreamEvent> + Send + '_>> {
        Box::pin(tokio_stream::iter(vec![
            StreamEvent::Delta("cached reply".into()),
            StreamEvent::Done(Usage {
                input_tokens: 13_047,
                output_tokens: 17,
                cache_read_tokens: 12_800,
                cache_write_tokens: 64,
            }),
        ]))
    }

    fn stream_with_tools(
        &self,
        messages: Vec<ChatMessage>,
        _tools: Vec<serde_json::Value>,
    ) -> Pin<Box<dyn Stream<Item = StreamEvent> + Send + '_>> {
        self.stream(messages)
    }
}

struct StreamingChunksProvider;

#[async_trait]
impl LlmProvider for StreamingChunksProvider {
    fn name(&self) -> &str {
        "streaming-chunks"
    }

    fn id(&self) -> &str {
        "streaming-chunks-model"
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

    async fn complete(
        &self,
        _messages: &[ChatMessage],
        _tools: &[serde_json::Value],
    ) -> Result<CompletionResponse> {
        Ok(CompletionResponse {
            text: Some("unused".into()),
            tool_calls: vec![],
            usage: Usage::default(),
        })
    }

    fn stream(
        &self,
        _messages: Vec<ChatMessage>,
    ) -> Pin<Box<dyn Stream<Item = StreamEvent> + Send + '_>> {
        Box::pin(tokio_stream::iter(vec![
            StreamEvent::Delta("cached ".into()),
            StreamEvent::Delta("reply".into()),
            StreamEvent::Done(Usage::default()),
        ]))
    }

    fn stream_with_tools(
        &self,
        messages: Vec<ChatMessage>,
        _tools: Vec<serde_json::Value>,
    ) -> Pin<Box<dyn Stream<Item = StreamEvent> + Send + '_>> {
        self.stream(messages)
    }
}

#[tokio::test]
async fn test_streaming_runner_emits_final_text_chunks_live() {
    let provider = Arc::new(StreamingChunksProvider);
    let tools = ToolRegistry::new();
    let events: Arc<std::sync::Mutex<Vec<RunnerEvent>>> =
        Arc::new(std::sync::Mutex::new(Vec::new()));
    let events_clone = Arc::clone(&events);
    let on_event: OnEvent = Box::new(move |event| {
        events_clone.lock().unwrap().push(event);
    });

    let result = run_agent_loop_streaming(
        provider,
        &tools,
        "You are a test bot.",
        &UserContent::text("another"),
        Some(&on_event),
        None,
        None,
        None,
        None,
        None,
    )
    .await
    .unwrap();

    assert_eq!(result.output.text, "cached reply");
    let final_chunks: Vec<String> = events
        .lock()
        .unwrap()
        .iter()
        .filter_map(|event| match event {
            RunnerEvent::FinalText(text) => Some(text.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(final_chunks, vec![
        "cached ".to_string(),
        "reply".to_string()
    ]);
}

#[tokio::test]
async fn test_streaming_runner_preserves_cache_usage() {
    let provider = Arc::new(StreamingUsageProvider);
    let tools = ToolRegistry::new();
    let uc = UserContent::text("another");

    let result = run_agent_loop_streaming(
        provider,
        &tools,
        "You are a test bot.",
        &uc,
        None,
        None,
        None,
        None,
        None,
        None,
    )
    .await
    .unwrap();

    assert_eq!(result.output.text, "cached reply");
    assert_eq!(result.iterations, 1);
    assert_eq!(result.tool_calls_made, 0);
    assert_eq!(result.usage.input_tokens, 13_047);
    assert_eq!(result.usage.output_tokens, 17);
    assert_eq!(result.usage.cache_read_tokens, 12_800);
    assert_eq!(result.usage.cache_write_tokens, 64);
    assert_eq!(result.request_usage.input_tokens, 13_047);
    assert_eq!(result.request_usage.output_tokens, 17);
    assert_eq!(result.request_usage.cache_read_tokens, 12_800);
    assert_eq!(result.request_usage.cache_write_tokens, 64);
}

#[tokio::test]
async fn test_streaming_runner_dispatches_before_agent_start_hook() {
    let provider = Arc::new(StreamingUsageProvider);
    let tools = ToolRegistry::new();
    let payloads = Arc::new(std::sync::Mutex::new(Vec::new()));
    let mut hooks = HookRegistry::new();
    hooks.register(Arc::new(AgentStartRecordingHook {
        payloads: Arc::clone(&payloads),
    }));

    let result = run_agent_loop_streaming(
        provider,
        &tools,
        "You are a test bot.",
        &UserContent::text("Hi"),
        None,
        None,
        Some(serde_json::json!({"_session_key": "stream-session-123"})),
        Some(Arc::new(hooks)),
        None,
        None,
    )
    .await
    .unwrap();

    assert_eq!(result.output.text, "cached reply");
    let payloads = payloads.lock().unwrap();
    assert_eq!(payloads.len(), 1);
    assert!(matches!(
        &payloads[0],
        HookPayload::BeforeAgentStart { session_key, model }
            if session_key == "stream-session-123" && model == "streaming-usage-model"
    ));
}

struct NonStreamingUsageProvider {
    call_count: std::sync::atomic::AtomicUsize,
}

#[async_trait]
impl LlmProvider for NonStreamingUsageProvider {
    fn name(&self) -> &str {
        "non-streaming-usage"
    }

    fn id(&self) -> &str {
        "non-streaming-usage-model"
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
        let count = self
            .call_count
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);

        if count == 0 {
            Ok(CompletionResponse {
                text: None,
                tool_calls: vec![ToolCall {
                    id: "call_usage_1".into(),
                    name: "echo_tool".into(),
                    arguments: serde_json::json!({"text": "hi"}),
                    argument_diagnostic: None,
                }],
                usage: Usage {
                    input_tokens: 100,
                    output_tokens: 10,
                    cache_read_tokens: 80,
                    cache_write_tokens: 8,
                },
            })
        } else {
            Ok(CompletionResponse {
                text: Some("Done with cache.".into()),
                tool_calls: vec![],
                usage: Usage {
                    input_tokens: 40,
                    output_tokens: 5,
                    cache_read_tokens: 32,
                    cache_write_tokens: 3,
                },
            })
        }
    }

    fn stream(
        &self,
        _messages: Vec<ChatMessage>,
    ) -> Pin<Box<dyn Stream<Item = StreamEvent> + Send + '_>> {
        Box::pin(tokio_stream::empty())
    }
}

#[tokio::test]
async fn test_non_streaming_runner_preserves_total_and_request_cache_usage() {
    let provider = Arc::new(NonStreamingUsageProvider {
        call_count: std::sync::atomic::AtomicUsize::new(0),
    });
    let mut tools = ToolRegistry::new();
    tools.register(Box::new(EchoTool));

    let uc = UserContent::text("Use the tool");
    let result = run_agent_loop(provider, &tools, "You are a test bot.", &uc, None, None)
        .await
        .unwrap();

    assert_eq!(result.output.text, "Done with cache.");
    assert_eq!(result.iterations, 2);
    assert_eq!(result.tool_calls_made, 1);
    assert_eq!(result.usage.input_tokens, 140);
    assert_eq!(result.usage.output_tokens, 15);
    assert_eq!(result.usage.cache_read_tokens, 112);
    assert_eq!(result.usage.cache_write_tokens, 11);
    assert_eq!(result.request_usage.input_tokens, 40);
    assert_eq!(result.request_usage.output_tokens, 5);
    assert_eq!(result.request_usage.cache_read_tokens, 32);
    assert_eq!(result.request_usage.cache_write_tokens, 3);
}

#[tokio::test]
async fn test_tool_call_loop() {
    let provider = Arc::new(ToolCallingProvider {
        call_count: std::sync::atomic::AtomicUsize::new(0),
    });
    let mut tools = ToolRegistry::new();
    tools.register(Box::new(EchoTool));

    let uc = UserContent::text("Use the tool");
    let result = run_agent_loop(provider, &tools, "You are a test bot.", &uc, None, None)
        .await
        .unwrap();

    assert_eq!(result.output.text, "Done!");
    assert_eq!(result.iterations, 2);
    assert_eq!(result.tool_calls_made, 1);
}

/// Mock provider that calls the "execute_command" tool (native) and verifies result fed back.
struct CommandSimulatingProvider {
    call_count: std::sync::atomic::AtomicUsize,
}

#[async_trait]
impl LlmProvider for CommandSimulatingProvider {
    fn name(&self) -> &str {
        "mock"
    }

    fn id(&self) -> &str {
        "mock-model"
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
        _tools: &[serde_json::Value],
    ) -> Result<CompletionResponse> {
        let count = self
            .call_count
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        if count == 0 {
            Ok(CompletionResponse {
                text: None,
                tool_calls: vec![ToolCall {
                    id: "call_execute_command_1".into(),
                    name: "execute_command".into(),
                    arguments: serde_json::json!({"command": "echo hello"}),
                    argument_diagnostic: None,
                }],
                usage: Usage {
                    input_tokens: 10,
                    output_tokens: 5,
                    ..Default::default()
                },
            })
        } else {
            let tool_content = messages
                .iter()
                .find_map(|m| {
                    if let ChatMessage::Tool { content, .. } = m {
                        Some(content.as_str())
                    } else {
                        None
                    }
                })
                .unwrap_or("");
            let parsed: serde_json::Value = serde_json::from_str(tool_content).unwrap();
            let stdout = parsed["stdout"].as_str().unwrap_or("");
            assert!(stdout.contains("hello"));
            assert_eq!(parsed["exit_code"].as_i64().unwrap(), 0);
            Ok(CompletionResponse {
                text: Some(format!("The output was: {}", stdout.trim())),
                tool_calls: vec![],
                usage: Usage {
                    input_tokens: 20,
                    output_tokens: 10,
                    ..Default::default()
                },
            })
        }
    }

    fn stream(
        &self,
        _messages: Vec<ChatMessage>,
    ) -> Pin<Box<dyn Stream<Item = StreamEvent> + Send + '_>> {
        Box::pin(tokio_stream::empty())
    }
}

#[tokio::test]
async fn test_execute_command_tool_end_to_end() {
    let provider = Arc::new(CommandSimulatingProvider {
        call_count: std::sync::atomic::AtomicUsize::new(0),
    });
    let mut tools = ToolRegistry::new();
    tools.register(Box::new(TestExecuteCommandTool));

    let events: Arc<std::sync::Mutex<Vec<RunnerEvent>>> =
        Arc::new(std::sync::Mutex::new(Vec::new()));
    let events_clone = Arc::clone(&events);
    let on_event: OnEvent = Box::new(move |event| {
        events_clone.lock().unwrap().push(event);
    });
    let lifecycle_events = Arc::new(std::sync::Mutex::new(Vec::new()));
    let on_tool_lifecycle = recording_tool_lifecycle(&lifecycle_events);

    let uc = UserContent::text("Run echo hello");
    let result = run_agent_loop_with_tool_lifecycle(
        provider,
        &tools,
        "You are a test bot.",
        &uc,
        Some(&on_event),
        Some(&on_tool_lifecycle),
        None,
    )
    .await
    .unwrap();

    assert!(
        result.output.text.contains("hello"),
        "got: {}",
        result.output.text
    );
    assert_eq!(result.iterations, 2);
    assert_eq!(result.tool_calls_made, 1);

    let events = events.lock().unwrap();
    assert!(
        events
            .iter()
            .any(|event| matches!(event, RunnerEvent::Thinking))
    );

    let lifecycle_events = lifecycle_events.lock().unwrap();
    assert!(lifecycle_events.iter().any(|event| {
        event.lifecycle.stage() == chelix_common::tool_lifecycle::ToolLifecycleStage::Executing
    }));
    let completed = lifecycle_events
        .iter()
        .find(|event| {
            matches!(
                event.lifecycle.update,
                chelix_common::tool_lifecycle::ToolLifecycleUpdate::Completed { success: true, .. }
            )
        })
        .expect("execute_command lifecycle must complete successfully");
    assert_eq!(completed.lifecycle.tool_name, "execute_command");
    let context_budget = completed
        .lifecycle
        .context_budget
        .as_ref()
        .expect("completed lifecycle event must carry context budget");
    assert_eq!(context_budget.context_window, TEST_CONTEXT_WINDOW);
    assert_eq!(context_budget.max_input_tokens, TEST_MAX_INPUT_TOKENS);
    assert_eq!(context_budget.max_output_tokens, TEST_MAX_OUTPUT_TOKENS);
    assert_eq!(context_budget.compaction_ratio, 85);
    assert_eq!(
        context_budget.available_input_tokens,
        TEST_MAX_INPUT_TOKENS as usize - context_budget.tool_schema_tokens
    );
    assert_eq!(
        context_budget.compaction_budget,
        context_budget.available_input_tokens * 85 / 100
    );
    assert!(context_budget.prompt_tokens > 0);
    assert!(!context_budget.compaction_required);
}

#[tokio::test(start_paused = true)]
async fn test_backend_execution_progress_uses_paused_tokio_time() {
    let provider = Arc::new(ToolCallingProvider {
        call_count: std::sync::atomic::AtomicUsize::new(0),
    });
    let mut tools = ToolRegistry::new();
    tools.register(Box::new(SlowTool {
        tool_name: "echo_tool".to_owned(),
        delay_ms: 3_500,
    }));
    let lifecycle_events = Arc::new(std::sync::Mutex::new(Vec::new()));
    let progress_seen = Arc::new(tokio::sync::Notify::new());
    let on_tool_lifecycle: OnToolLifecycle = {
        let lifecycle_events = Arc::clone(&lifecycle_events);
        let progress_seen = Arc::clone(&progress_seen);
        Arc::new(move |event| {
            lifecycle_events.lock().unwrap().push(event.clone());
            let progress_seen = Arc::clone(&progress_seen);
            Box::pin(async move {
                if event.lifecycle.stage()
                    == chelix_common::tool_lifecycle::ToolLifecycleStage::ExecutionProgress
                {
                    progress_seen.notify_one();
                }
                Ok(())
            })
        })
    };

    let user_content = UserContent::text("run slowly");
    let run = run_agent_loop_with_tool_lifecycle(
        provider,
        &tools,
        "You are a test bot.",
        &user_content,
        None,
        Some(&on_tool_lifecycle),
        None,
    );
    tokio::pin!(run);
    tokio::select! {
        () = progress_seen.notified() => {},
        _ = &mut run => panic!("slow tool completed before initial progress"),
    }

    for expected_second in 1..=3_u64 {
        tokio::time::advance(std::time::Duration::from_secs(1)).await;
        tokio::select! {
            () = progress_seen.notified() => {},
            _ = &mut run => panic!("slow tool completed before progress second {expected_second}"),
        }
    }
    tokio::time::advance(std::time::Duration::from_millis(500)).await;
    let result = run.await.unwrap();
    assert_eq!(result.output.text, "Done!");

    let lifecycle_events = lifecycle_events.lock().unwrap();
    let progress = lifecycle_events
        .iter()
        .filter_map(|event| match &event.lifecycle.update {
            chelix_common::tool_lifecycle::ToolLifecycleUpdate::ExecutionProgress {
                elapsed_ms,
                message,
                ..
            } => Some((*elapsed_ms, message.as_str())),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(progress, vec![
        (0, "wait for result [0] sec."),
        (1_000, "wait for result [1] sec."),
        (2_000, "wait for result [2] sec."),
        (3_000, "wait for result [3] sec."),
    ]);
}

struct HookModifiedCommandProvider {
    call_count: std::sync::atomic::AtomicUsize,
}

#[async_trait]
impl LlmProvider for HookModifiedCommandProvider {
    fn name(&self) -> &str {
        "hook-modified-command"
    }

    fn id(&self) -> &str {
        "hook-modified-command-model"
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
        _tools: &[serde_json::Value],
    ) -> Result<CompletionResponse> {
        let count = self
            .call_count
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        if count == 0 {
            Ok(CompletionResponse {
                text: None,
                tool_calls: vec![ToolCall {
                    id: "call_execute_command_hook_1".into(),
                    name: "execute_command".into(),
                    arguments: serde_json::json!({"command": "echo should-not-run"}),
                    argument_diagnostic: None,
                }],
                usage: Usage::default(),
            })
        } else {
            let tool_content = messages
                .iter()
                .find_map(|m| {
                    if let ChatMessage::Tool { content, .. } = m {
                        Some(content.as_str())
                    } else {
                        None
                    }
                })
                .unwrap_or("");
            assert!(
                tool_content.contains("Missing required field(s): `command`"),
                "tool result should contain validation error, got: {tool_content}"
            );
            assert!(
                !tool_content.contains("should-not-run"),
                "invalid hook args must be rejected before execute_command runs"
            );
            Ok(CompletionResponse {
                text: Some("Hook rewrite was rejected.".into()),
                tool_calls: vec![],
                usage: Usage::default(),
            })
        }
    }

    fn stream(
        &self,
        _messages: Vec<ChatMessage>,
    ) -> Pin<Box<dyn Stream<Item = StreamEvent> + Send + '_>> {
        Box::pin(tokio_stream::empty())
    }
}

#[tokio::test]
async fn test_hook_modified_tool_args_are_revalidated_before_execute() {
    let provider = Arc::new(HookModifiedCommandProvider {
        call_count: std::sync::atomic::AtomicUsize::new(0),
    });
    let mut tools = ToolRegistry::new();
    tools.register(Box::new(TestExecuteCommandTool));

    let mut hooks = HookRegistry::new();
    hooks.register(Arc::new(RewriteToolArgsHook {
        replacement: serde_json::json!({"timeout": 1}),
    }));

    let lifecycle_events = Arc::new(std::sync::Mutex::new(Vec::new()));
    let on_tool_lifecycle = recording_tool_lifecycle(&lifecycle_events);

    let result = run_agent_loop_with_context_and_tool_lifecycle(
        provider,
        &tools,
        "You are a test bot.",
        &UserContent::text("Run through hook"),
        None,
        Some(&on_tool_lifecycle),
        None,
        None,
        Some(Arc::new(hooks)),
        None,
    )
    .await
    .unwrap();

    assert_eq!(result.output.text, "Hook rewrite was rejected.");
    assert_eq!(result.tool_calls_made, 1);

    let lifecycle_events = lifecycle_events.lock().unwrap();
    let waiting_index = lifecycle_events
        .iter()
        .position(|event| {
            event.lifecycle.tool_name == "execute_command"
                && event.lifecycle.stage()
                    == chelix_common::tool_lifecycle::ToolLifecycleStage::WaitingForExecution
        })
        .expect("hook-modified call must wait for execution before hook dispatch");
    let result_ready_index = lifecycle_events
        .iter()
        .position(|event| {
            matches!(
                &event.lifecycle.update,
                chelix_common::tool_lifecycle::ToolLifecycleUpdate::ResultReady {
                    success: false,
                    error: Some(error),
                    ..
                } if error.contains("Missing required field(s): `command`")
            )
        })
        .expect("hook-modified validation failure must produce a terminal result");
    let completed_index = lifecycle_events
        .iter()
        .position(|event| {
            matches!(
                &event.lifecycle.update,
                chelix_common::tool_lifecycle::ToolLifecycleUpdate::Completed {
                    success: false,
                    error: Some(error),
                    ..
                } if error.contains("Missing required field(s): `command`")
            )
        })
        .expect("hook-modified validation failure must complete the invocation");
    assert!(waiting_index < result_ready_index);
    assert!(result_ready_index < completed_index);
    assert!(!lifecycle_events.iter().any(|event| {
        matches!(
            event.lifecycle.stage(),
            chelix_common::tool_lifecycle::ToolLifecycleStage::Executing
                | chelix_common::tool_lifecycle::ToolLifecycleStage::Rejected
        )
    }));
}

/// Test that non-native providers can still execute tools via text parsing.
#[tokio::test]
async fn test_text_based_tool_calling() {
    let provider = Arc::new(TextToolCallingProvider {
        call_count: std::sync::atomic::AtomicUsize::new(0),
    });
    let mut tools = ToolRegistry::new();
    tools.register(Box::new(TestExecuteCommandTool));

    let lifecycle_events = Arc::new(std::sync::Mutex::new(Vec::new()));
    let on_tool_lifecycle = recording_tool_lifecycle(&lifecycle_events);

    let uc = UserContent::text("Run echo hello");
    let result = run_agent_loop_with_tool_lifecycle(
        provider,
        &tools,
        "You are a test bot.",
        &uc,
        None,
        Some(&on_tool_lifecycle),
        None,
    )
    .await
    .unwrap();

    assert!(
        result.output.text.contains("hello"),
        "got: {}",
        result.output.text
    );
    assert_eq!(result.iterations, 2, "should take 2 iterations");
    assert_eq!(result.tool_calls_made, 1, "should execute 1 tool call");

    let lifecycle_events = lifecycle_events.lock().unwrap();
    assert!(lifecycle_events.iter().any(|event| {
        event.lifecycle.stage() == chelix_common::tool_lifecycle::ToolLifecycleStage::Executing
    }));
    assert!(lifecycle_events.iter().any(|event| {
        matches!(
            event.lifecycle.update,
            chelix_common::tool_lifecycle::ToolLifecycleUpdate::Completed { success: true, .. }
        )
    }));
}

/// Native-tool provider that emits XML-like function text instead of
/// structured tool calls.
struct NativeTextFunctionProvider {
    call_count: std::sync::atomic::AtomicUsize,
}

#[async_trait]
impl LlmProvider for NativeTextFunctionProvider {
    fn name(&self) -> &str {
        "mock-native-function"
    }

    fn id(&self) -> &str {
        "mock-native-function"
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
        _tools: &[serde_json::Value],
    ) -> Result<CompletionResponse> {
        let count = self
            .call_count
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        if count == 0 {
            Ok(CompletionResponse {
                text: Some(
                    "<function=process>\n<parameter=action>\nstart\n</parameter>\n<parameter=command>\npwd\n</parameter>\n</function>\n</tool_call>"
                        .into(),
                ),
                tool_calls: vec![],
                usage: Usage {
                    input_tokens: 10,
                    output_tokens: 20,
                    ..Default::default()
                },
            })
        } else {
            let tool_content = messages
                .iter()
                .find_map(|m| {
                    if let ChatMessage::Tool { content, .. } = m {
                        Some(content.as_str())
                    } else {
                        None
                    }
                })
                .unwrap_or("");
            let tool_result: serde_json::Value = serde_json::from_str(tool_content)
                .unwrap_or_else(|error| panic!("tool result should be JSON: {error}"));
            assert_eq!(tool_result["received"]["action"], "start");
            assert_eq!(tool_result["received"]["command"], "pwd");
            Ok(CompletionResponse {
                text: Some("Process started for pwd".into()),
                tool_calls: vec![],
                usage: Usage {
                    input_tokens: 30,
                    output_tokens: 10,
                    ..Default::default()
                },
            })
        }
    }

    fn stream(
        &self,
        _messages: Vec<ChatMessage>,
    ) -> Pin<Box<dyn Stream<Item = StreamEvent> + Send + '_>> {
        Box::pin(tokio_stream::empty())
    }
}

#[tokio::test]
async fn test_native_text_function_tool_calling_non_streaming() {
    let provider = Arc::new(NativeTextFunctionProvider {
        call_count: std::sync::atomic::AtomicUsize::new(0),
    });
    let mut tools = ToolRegistry::new();
    tools.register(Box::new(TestProcessTool));

    let lifecycle_events = Arc::new(std::sync::Mutex::new(Vec::new()));
    let on_tool_lifecycle = recording_tool_lifecycle(&lifecycle_events);

    let uc = UserContent::text("execute pwd");
    let result = run_agent_loop_with_tool_lifecycle(
        provider,
        &tools,
        "You are a test bot.",
        &uc,
        None,
        Some(&on_tool_lifecycle),
        None,
    )
    .await
    .unwrap();

    assert!(
        result.output.text.contains("pwd"),
        "got: {}",
        result.output.text
    );
    assert_eq!(result.iterations, 2, "should take 2 iterations");
    assert_eq!(result.tool_calls_made, 1, "should execute 1 tool call");

    let lifecycle_events = lifecycle_events.lock().unwrap();
    let input_ready = lifecycle_events.iter().find_map(|event| {
        if let chelix_common::tool_lifecycle::ToolLifecycleUpdate::InputReady { arguments } =
            &event.lifecycle.update
        {
            Some((&event.lifecycle.tool_name, arguments))
        } else {
            None
        }
    });
    let (name, arguments) = input_ready.expect("input-ready lifecycle event must be emitted");
    assert_eq!(name, "process");
    assert_eq!(arguments["action"], "start");
    assert_eq!(arguments["command"], "pwd");
}

// ── sanitize_tool_result tests ──────────────────────────────────

#[test]
fn test_sanitize_short_input_unchanged() {
    let input = "hello world";
    assert_eq!(sanitize_tool_result(input), "hello world");
}

#[test]
fn test_sanitize_strips_base64_data_uri() {
    let payload = "A".repeat(300);
    let input = format!("before data:image/png;base64,{payload} after");
    let result = sanitize_tool_result(&input);
    assert!(!result.contains(&payload));
    assert!(result.contains("[screenshot captured and displayed in UI]"));
    assert!(result.contains("before"));
    assert!(result.contains("after"));
}

#[test]
fn test_sanitize_preserves_short_base64() {
    let payload = "QUFB";
    let input = format!("data:text/plain;base64,{payload}");
    let result = sanitize_tool_result(&input);
    assert!(result.contains(payload));
}

#[test]
fn test_sanitize_strips_long_hex() {
    let hex = "a1b2c3d4".repeat(50);
    let input = format!("prefix {hex} suffix");
    let result = sanitize_tool_result(&input);
    assert!(!result.contains(&hex));
    assert!(result.contains("[hex data removed"));
    assert!(result.contains("prefix"));
    assert!(result.contains("suffix"));
}

#[test]
fn test_sanitize_preserves_short_hex() {
    let hex = "deadbeef";
    let input = format!("code: {hex}");
    let result = sanitize_tool_result(&input);
    assert!(result.contains(hex));
}

// ── Vision and image edge cases ─────────────────────────────────

#[tokio::test]
async fn test_vision_provider_tool_result_sanitized() {
    let provider = Arc::new(VisionEnabledProvider {
        call_count: std::sync::atomic::AtomicUsize::new(0),
    });
    let mut tools = ToolRegistry::new();
    tools.register(Box::new(ScreenshotTool));
    let uc = UserContent::text("Take a screenshot");
    let result = run_agent_loop(provider, &tools, "You are a test bot.", &uc, None, None)
        .await
        .unwrap();
    assert_eq!(result.output.text, "Screenshot processed successfully");
    assert_eq!(result.tool_calls_made, 1);
}

#[tokio::test]
async fn completed_lifecycle_separates_context_from_raw_result() {
    let provider = Arc::new(VisionEnabledProvider {
        call_count: std::sync::atomic::AtomicUsize::new(0),
    });
    let mut tools = ToolRegistry::new();
    tools.register(Box::new(ScreenshotTool));
    let lifecycle_events = Arc::new(std::sync::Mutex::new(Vec::new()));
    let on_tool_lifecycle = recording_tool_lifecycle(&lifecycle_events);
    let uc = UserContent::text("Take a screenshot");
    let result = run_agent_loop_with_tool_lifecycle(
        provider,
        &tools,
        "You are a test bot.",
        &uc,
        None,
        Some(&on_tool_lifecycle),
        None,
    )
    .await
    .unwrap();
    assert_eq!(result.tool_calls_made, 1);

    let lifecycle_events = lifecycle_events.lock().unwrap();
    let completed = lifecycle_events
        .iter()
        .find(|event| {
            matches!(
                event.lifecycle.update,
                chelix_common::tool_lifecycle::ToolLifecycleUpdate::Completed { success: true, .. }
            )
        })
        .expect("completed lifecycle event must be emitted");
    let chelix_common::tool_lifecycle::ToolLifecycleUpdate::Completed {
        result: Some(context_result),
        ..
    } = &completed.lifecycle.update
    else {
        panic!("completed lifecycle must carry the canonical context result");
    };
    let raw_result = completed
        .raw_result
        .as_ref()
        .expect("completed lifecycle must carry the raw result envelope");
    assert!(!context_result.contains("data:image/png;base64,"));
    assert!(context_result.contains("[screenshot captured and displayed in UI]"));
    let result_str = raw_result.to_string();
    assert!(result_str.contains("screenshot"));
    assert!(result_str.contains("data:image/png;base64,"));
}
