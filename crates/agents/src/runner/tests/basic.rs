//! Basic runner tests: parsing, shell commands, sanitization, tool results, vision.

use std::sync::Arc;

use {
    super::helpers::*,
    crate::{
        model::{
            AgentToolControls, ChatMessage, CompletionResponse, LlmProvider, StreamEvent, ToolCall,
            Usage,
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

#[test]
fn test_explicit_shell_command_requires_sh_prefix() {
    let uc = UserContent::text("pwd");
    assert!(explicit_shell_command_from_user_content(&uc).is_none());
}

#[test]
fn test_explicit_shell_command_extracts_command() {
    let uc = UserContent::text("/sh pwd");
    assert_eq!(
        explicit_shell_command_from_user_content(&uc).as_deref(),
        Some("pwd")
    );
}

#[test]
fn test_explicit_shell_command_supports_telegram_style_bot_mention() {
    let uc = UserContent::text("/sh@ChelixBot uname -a");
    assert_eq!(
        explicit_shell_command_from_user_content(&uc).as_deref(),
        Some("uname -a")
    );
}

#[test]
fn test_resolve_agent_max_iterations_falls_back_for_zero() {
    assert_eq!(
        resolve_agent_max_iterations(0),
        DEFAULT_AGENT_MAX_ITERATIONS
    );
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
    assert_eq!(result.text, "Hello!");
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
        _options: &AgentToolControls,
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

    assert_eq!(result.text, "no tools");
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

    assert_eq!(result.text, "no tools");
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

#[tokio::test]
async fn test_non_streaming_runner_uses_max_iteration_override() {
    let provider = Arc::new(ToolCallingProvider {
        call_count: std::sync::atomic::AtomicUsize::new(0),
    });
    let mut tools = ToolRegistry::new();
    tools.register(Box::new(EchoTool));

    let result = run_agent_loop_with_context_and_limits(
        provider,
        &tools,
        "You are a test bot.",
        &UserContent::text("Hi"),
        None,
        None,
        None,
        None,
        None,
        AgentLoopLimits {
            max_iterations: Some(1),
            ..Default::default()
        },
    )
    .await;

    let error = result.unwrap_err().to_string();
    assert!(
        error.contains("agent loop exceeded max iterations (1)"),
        "unexpected error: {error}"
    );
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

    assert_eq!(result.text, "Hello!");
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

    assert_eq!(result.text, "ok");
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

    assert_eq!(result.text, "ok");
    let messages = recorded_messages.lock().unwrap();
    assert!(matches!(
        messages.first(),
        Some(ChatMessage::System { content }) if content == "hook-injected system"
    ));
}

#[test]
fn test_before_llm_call_modify_payload_keeps_original_when_invalid() {
    let mut messages = vec![ChatMessage::system("original system")];

    apply_before_llm_call_modify_payload(
        &mut messages,
        serde_json::json!({"messages": [{"role": "invalid", "content": "ignored"}]}),
    );

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

    assert_eq!(result.text, "cached reply");
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

    assert_eq!(result.text, "cached reply");
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

    assert_eq!(result.text, "cached reply");
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
                    metadata: None,
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

    assert_eq!(result.text, "Done with cache.");
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

    assert_eq!(result.text, "Done!");
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
                    metadata: None,
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

    let uc = UserContent::text("Run echo hello");
    let result = run_agent_loop(
        provider,
        &tools,
        "You are a test bot.",
        &uc,
        Some(&on_event),
        None,
    )
    .await
    .unwrap();

    assert!(result.text.contains("hello"), "got: {}", result.text);
    assert_eq!(result.iterations, 2);
    assert_eq!(result.tool_calls_made, 1);

    let evts = events.lock().unwrap();
    let has = |name: &str| {
        evts.iter().any(|e| {
            matches!(
                (e, name),
                (RunnerEvent::Thinking, "thinking")
                    | (RunnerEvent::ToolCallStart { .. }, "tool_call_start")
                    | (RunnerEvent::ToolCallEnd { .. }, "tool_call_end")
            )
        })
    };
    assert!(has("tool_call_start"));
    assert!(has("tool_call_end"));
    assert!(has("thinking"));

    let tool_end = evts
        .iter()
        .find(|e| matches!(e, RunnerEvent::ToolCallEnd { .. }));
    if let Some(RunnerEvent::ToolCallEnd {
        success,
        name,
        context_budget,
        ..
    }) = tool_end
    {
        assert!(success, "execute_command tool should succeed");
        assert_eq!(name, "execute_command");
        assert_eq!(context_budget.context_window, 200_000);
        assert_eq!(context_budget.compaction_ratio, 85);
        assert_eq!(context_budget.compaction_budget, 170_000);
        assert!(context_budget.current_tokens > 0);
        assert!(!context_budget.compaction_required);
    }
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
                    metadata: None,
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

    let events: Arc<std::sync::Mutex<Vec<RunnerEvent>>> =
        Arc::new(std::sync::Mutex::new(Vec::new()));
    let events_clone = Arc::clone(&events);
    let on_event: OnEvent = Box::new(move |event| {
        events_clone.lock().unwrap().push(event);
    });

    let result = run_agent_loop_with_context(
        provider,
        &tools,
        "You are a test bot.",
        &UserContent::text("Run through hook"),
        Some(&on_event),
        None,
        None,
        Some(Arc::new(hooks)),
        None,
    )
    .await
    .unwrap();

    assert_eq!(result.text, "Hook rewrite was rejected.");
    assert_eq!(result.tool_calls_made, 1);

    let evts = events.lock().unwrap();
    let start_index = evts
        .iter()
        .position(
            |event| matches!(event, RunnerEvent::ToolCallStart { name, .. } if name == "execute_command"),
        )
        .expect("hook-modified call should emit ToolCallStart before hook dispatch");
    let end_index = evts
        .iter()
        .position(|event| {
            matches!(
                event,
                RunnerEvent::ToolCallEnd {
                    name,
                    success: false,
                    error: Some(error),
                    ..
                } if name == "execute_command" && error.contains("Missing required field(s): `command`")
            )
        })
        .expect("hook-modified validation failure should close the started tool span");
    assert!(
        start_index < end_index,
        "ToolCallEnd should follow ToolCallStart"
    );
    assert!(
        !evts
            .iter()
            .any(|event| matches!(event, RunnerEvent::ToolCallRejected { .. })),
        "post-start validation failures should not emit ToolCallRejected"
    );
}

/// Test that non-native providers can still execute tools via text parsing.
#[tokio::test]
async fn test_text_based_tool_calling() {
    let provider = Arc::new(TextToolCallingProvider {
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

    let uc = UserContent::text("Run echo hello");
    let result = run_agent_loop(
        provider,
        &tools,
        "You are a test bot.",
        &uc,
        Some(&on_event),
        None,
    )
    .await
    .unwrap();

    assert!(result.text.contains("hello"), "got: {}", result.text);
    assert_eq!(result.iterations, 2, "should take 2 iterations");
    assert_eq!(result.tool_calls_made, 1, "should execute 1 tool call");

    let evts = events.lock().unwrap();
    assert!(
        evts.iter()
            .any(|e| matches!(e, RunnerEvent::ToolCallStart { .. }))
    );
    assert!(
        evts.iter()
            .any(|e| matches!(e, RunnerEvent::ToolCallEnd { success: true, .. }))
    );
}

/// Native-tool provider that returns plain text (no structured tool call)
/// on the first turn for a command-like prompt.
struct DirectCommandNoToolProvider {
    call_count: std::sync::atomic::AtomicUsize,
}

#[async_trait]
impl LlmProvider for DirectCommandNoToolProvider {
    fn name(&self) -> &str {
        "mock-direct-command"
    }

    fn id(&self) -> &str {
        "mock-direct-command"
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
                text: Some("I'll summarize the command output for you.".into()),
                tool_calls: vec![],
                usage: Usage {
                    input_tokens: 10,
                    output_tokens: 10,
                    ..Default::default()
                },
            })
        } else {
            let assistant_tool_text = messages
                .iter()
                .find_map(|m| {
                    if let ChatMessage::Assistant {
                        content,
                        tool_calls,
                        ..
                    } = m
                    {
                        if tool_calls.is_empty() {
                            return None;
                        }
                        return content.as_deref();
                    }
                    None
                })
                .unwrap_or("");
            assert!(
                !assistant_tool_text.is_empty(),
                "forced command should preserve assistant reasoning text"
            );
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
                !tool_content.is_empty(),
                "forced command should append a tool result message"
            );
            Ok(CompletionResponse {
                text: Some("done".into()),
                tool_calls: vec![],
                usage: Usage {
                    input_tokens: 10,
                    output_tokens: 5,
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
async fn test_explicit_sh_command_forces_execute_command_non_streaming() {
    let provider = Arc::new(DirectCommandNoToolProvider {
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

    let uc = UserContent::text("/sh pwd");
    let result = run_agent_loop(
        provider,
        &tools,
        "You are a test bot.",
        &uc,
        Some(&on_event),
        None,
    )
    .await
    .unwrap();

    assert_eq!(result.iterations, 2);
    assert_eq!(result.tool_calls_made, 1);
    assert_eq!(result.text, "done");

    let evts = events.lock().unwrap();
    let tool_start = evts.iter().find_map(|e| {
        if let RunnerEvent::ToolCallStart {
            name, arguments, ..
        } = e
        {
            Some((name.clone(), arguments.clone()))
        } else {
            None
        }
    });
    assert!(tool_start.is_some(), "should emit ToolCallStart");
    let (name, args) = tool_start.unwrap();
    assert_eq!(name, "execute_command");
    assert_eq!(args["command"], "pwd");
}

#[tokio::test]
async fn test_unprefixed_command_like_text_does_not_force_execute_command_non_streaming() {
    let provider = Arc::new(MockProvider {
        response_text: "plain response".to_string(),
    });
    let mut tools = ToolRegistry::new();
    tools.register(Box::new(TestExecuteCommandTool));

    let events: Arc<std::sync::Mutex<Vec<RunnerEvent>>> =
        Arc::new(std::sync::Mutex::new(Vec::new()));
    let events_clone = Arc::clone(&events);
    let on_event: OnEvent = Box::new(move |event| {
        events_clone.lock().unwrap().push(event);
    });

    let uc = UserContent::text("pwd");
    let result = run_agent_loop(
        provider,
        &tools,
        "You are a test bot.",
        &uc,
        Some(&on_event),
        None,
    )
    .await
    .unwrap();

    assert_eq!(result.iterations, 1);
    assert_eq!(result.tool_calls_made, 0);
    assert_eq!(result.text, "plain response");

    let evts = events.lock().unwrap();
    assert!(
        !evts
            .iter()
            .any(|e| matches!(e, RunnerEvent::ToolCallStart { .. })),
        "should not emit ToolCallStart for unprefixed command-like text"
    );
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
            assert!(
                tool_content.contains("\"action\":\"start\""),
                "tool result should include action=start, got: {tool_content}"
            );
            assert!(
                tool_content.contains("\"command\":\"pwd\""),
                "tool result should include command=pwd, got: {tool_content}"
            );
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

    let events: Arc<std::sync::Mutex<Vec<RunnerEvent>>> =
        Arc::new(std::sync::Mutex::new(Vec::new()));
    let events_clone = Arc::clone(&events);
    let on_event: OnEvent = Box::new(move |event| {
        events_clone.lock().unwrap().push(event);
    });

    let uc = UserContent::text("execute pwd");
    let result = run_agent_loop(
        provider,
        &tools,
        "You are a test bot.",
        &uc,
        Some(&on_event),
        None,
    )
    .await
    .unwrap();

    assert!(result.text.contains("pwd"), "got: {}", result.text);
    assert_eq!(result.iterations, 2, "should take 2 iterations");
    assert_eq!(result.tool_calls_made, 1, "should execute 1 tool call");

    let evts = events.lock().unwrap();
    let tool_start = evts.iter().find_map(|e| {
        if let RunnerEvent::ToolCallStart {
            arguments, name, ..
        } = e
        {
            Some((name.clone(), arguments.clone()))
        } else {
            None
        }
    });
    assert!(tool_start.is_some(), "should emit ToolCallStart");
    let (name, args) = tool_start.unwrap();
    assert_eq!(name, "process");
    assert_eq!(args["action"], "start");
    assert_eq!(args["command"], "pwd");
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
    assert_eq!(result.text, "Screenshot processed successfully");
    assert_eq!(result.tool_calls_made, 1);
}

#[tokio::test]
async fn test_tool_call_end_event_separates_context_from_raw_result() {
    let provider = Arc::new(VisionEnabledProvider {
        call_count: std::sync::atomic::AtomicUsize::new(0),
    });
    let mut tools = ToolRegistry::new();
    tools.register(Box::new(ScreenshotTool));
    let events: Arc<std::sync::Mutex<Vec<RunnerEvent>>> =
        Arc::new(std::sync::Mutex::new(Vec::new()));
    let events_clone = Arc::clone(&events);
    let on_event: OnEvent = Box::new(move |event| {
        events_clone.lock().unwrap().push(event);
    });
    let uc = UserContent::text("Take a screenshot");
    let result = run_agent_loop(
        provider,
        &tools,
        "You are a test bot.",
        &uc,
        Some(&on_event),
        None,
    )
    .await
    .unwrap();
    assert_eq!(result.tool_calls_made, 1);
    let evts = events.lock().unwrap();
    let tool_end = evts
        .iter()
        .find(|e| matches!(e, RunnerEvent::ToolCallEnd { success: true, .. }));
    if let Some(RunnerEvent::ToolCallEnd {
        success,
        result: Some(context_result),
        raw_result: Some(raw_result),
        ..
    }) = tool_end
    {
        assert!(success);
        assert!(!context_result.contains("data:image/png;base64,"));
        assert!(context_result.contains("[screenshot captured and displayed in UI]"));
        let result_str = raw_result.to_string();
        assert!(
            result_str.contains("screenshot"),
            "raw result should contain screenshot field"
        );
        assert!(
            result_str.contains("data:image/png;base64,"),
            "raw result should contain image data URI"
        );
    } else {
        panic!("expected ToolCallEnd event with canonical and raw results");
    }
}
