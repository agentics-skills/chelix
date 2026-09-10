use {
    super::helpers::*,
    crate::model::{ChatMessage, CompletionResponse, LlmProvider, StreamEvent},
    chelix_common::hooks::{HookAction, HookEvent, HookHandler, HookPayload, HookRegistry},
    std::{
        pin::Pin,
        sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        },
    },
    tokio_stream::Stream,
};

struct ThresholdProvider {
    complete_calls: AtomicUsize,
}

struct ResumeProvider {
    seen_messages: std::sync::Mutex<Vec<ChatMessage>>,
}

struct PartialRetryProvider {
    stream_calls: AtomicUsize,
    partial: String,
}

struct ReplaceBeforeLlmHook {
    messages: serde_json::Value,
}

struct SurroundSystemBeforeLlmHook {
    prefix: &'static str,
    suffix: &'static str,
}

#[async_trait::async_trait]
impl HookHandler for ReplaceBeforeLlmHook {
    fn name(&self) -> &str {
        "replace-before-llm"
    }

    fn events(&self) -> &[HookEvent] {
        static EVENTS: [HookEvent; 1] = [HookEvent::BeforeLLMCall];
        &EVENTS
    }

    async fn handle(
        &self,
        _event: HookEvent,
        _payload: &HookPayload,
    ) -> chelix_common::error::Result<HookAction> {
        Ok(HookAction::ModifyPayload(serde_json::json!({
            "messages": self.messages,
        })))
    }
}

#[async_trait::async_trait]
impl HookHandler for SurroundSystemBeforeLlmHook {
    fn name(&self) -> &str {
        "surround-system-before-llm"
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
        let Some(mut messages) = messages.as_array().cloned() else {
            return Ok(HookAction::Continue);
        };
        let Some(system) = messages
            .first()
            .and_then(|message| message.get("content"))
            .and_then(serde_json::Value::as_str)
            .map(str::to_string)
        else {
            return Ok(HookAction::Continue);
        };
        let mut wrapped =
            String::with_capacity(self.prefix.len() + system.len() + self.suffix.len());
        wrapped.push_str(self.prefix);
        wrapped.push_str(&system);
        wrapped.push_str(self.suffix);
        messages[0]["content"] = serde_json::Value::String(wrapped);
        Ok(HookAction::ModifyPayload(serde_json::json!({
            "messages": messages,
        })))
    }
}

impl LlmProvider for ThresholdProvider {
    fn name(&self) -> &str {
        "threshold"
    }

    fn id(&self) -> &str {
        "threshold-model"
    }

    fn context_window(&self) -> Option<u32> {
        Some(10)
    }

    fn max_input_tokens(&self) -> Option<u32> {
        Some(9)
    }

    fn max_output_tokens(&self) -> Option<u32> {
        Some(1)
    }

    fn stream_with_tools(
        &self,
        _messages: Vec<ChatMessage>,
        _tools: Vec<serde_json::Value>,
    ) -> Pin<Box<dyn Stream<Item = StreamEvent> + Send + '_>> {
        crate::model::response_stream(async move {
            self.complete_calls.fetch_add(1, Ordering::SeqCst);
            Ok(CompletionResponse {
                text: Some("provider must not be called".to_string()),
                tool_calls: Vec::new(),
                usage: Default::default(),
                ..Default::default()
            })
        })
    }

    fn stream(
        &self,
        messages: Vec<ChatMessage>,
    ) -> Pin<Box<dyn Stream<Item = StreamEvent> + Send + '_>> {
        self.stream_with_tools(messages, Vec::new())
    }
}

impl LlmProvider for PartialRetryProvider {
    fn name(&self) -> &str {
        "partial-retry"
    }

    fn id(&self) -> &str {
        "partial-retry-model"
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

    fn stream_with_tools(
        &self,
        messages: Vec<ChatMessage>,
        _tools: Vec<serde_json::Value>,
    ) -> Pin<Box<dyn Stream<Item = StreamEvent> + Send + '_>> {
        self.stream(messages)
    }

    fn stream(
        &self,
        _messages: Vec<ChatMessage>,
    ) -> Pin<Box<dyn Stream<Item = StreamEvent> + Send + '_>> {
        let call = self.stream_calls.fetch_add(1, Ordering::SeqCst);
        if call != 0 {
            return Box::pin(tokio_stream::iter(vec![StreamEvent::Error(
                "unexpected second provider call".to_string(),
            )]));
        }
        let partial = self.partial.clone();
        Box::pin(tokio_stream::iter(vec![
            StreamEvent::SegmentStart {
                segment_id: chelix_common::ProviderSegmentId::new("partial-retry-segment"),
            },
            StreamEvent::ProviderItemUpdate(chelix_common::ProviderItemUpdate {
                segment_id: chelix_common::ProviderSegmentId::new("partial-retry-segment"),
                item_id: chelix_common::ProviderItemId::new("partial-retry-message"),
                position: chelix_common::ProviderItemPosition::new(0),
                update_seq: 1,
                payload: chelix_common::ProviderItemUpdatePayload::MessageDelta {
                    delta: partial.clone(),
                },
            }),
            StreamEvent::Delta(partial),
            StreamEvent::Error("http 429 retry-after: 1ms".to_string()),
        ]))
    }
}

impl LlmProvider for ResumeProvider {
    fn name(&self) -> &str {
        "resume"
    }

    fn id(&self) -> &str {
        "resume-model"
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

    fn stream_with_tools(
        &self,
        messages: Vec<ChatMessage>,
        _tools: Vec<serde_json::Value>,
    ) -> Pin<Box<dyn Stream<Item = StreamEvent> + Send + '_>> {
        crate::model::response_stream(async move {
            *self
                .seen_messages
                .lock()
                .unwrap_or_else(|error| error.into_inner()) = messages.to_vec();
            Ok(CompletionResponse {
                text: Some("continued".to_string()),
                tool_calls: Vec::new(),
                usage: Default::default(),
                ..Default::default()
            })
        })
    }

    fn stream(
        &self,
        messages: Vec<ChatMessage>,
    ) -> Pin<Box<dyn Stream<Item = StreamEvent> + Send + '_>> {
        self.stream_with_tools(messages, Vec::new())
    }
}

#[test]
fn context_budget_uses_fixed_eighty_five_percent_threshold() {
    let messages = vec![ChatMessage::user("hello")];
    let metadata = evaluate_context_budget(&messages, &[], 40_000, 27_200, 12_800);

    assert_eq!(metadata.context_window, 40_000);
    assert_eq!(metadata.max_input_tokens, 27_200);
    assert_eq!(metadata.max_output_tokens, 12_800);
    assert_eq!(metadata.compaction_ratio, AUTO_COMPACTION_RATIO);
    assert_eq!(metadata.prompt_tokens, estimate_prompt_tokens(&messages));
    assert_eq!(metadata.tool_schema_tokens, 0);
    assert_eq!(metadata.available_input_tokens, 27_200);
    assert_eq!(metadata.compaction_budget, 23_120);
    assert!(!metadata.compaction_required);
}

#[test]
fn context_budget_reports_real_usage_percent() {
    let messages = vec![ChatMessage::user("a".repeat(400))];
    let metadata = evaluate_context_budget(&messages, &[], 1_200, 1_000, 200);

    assert_eq!(metadata.prompt_tokens, estimate_prompt_tokens(&messages));
    assert_eq!(
        metadata.usage_percent,
        metadata.prompt_tokens * 100 / metadata.compaction_budget
    );
}

#[test]
fn context_budget_subtracts_tool_schemas_once() {
    let messages = vec![ChatMessage::user("hello")];
    let schemas = vec![serde_json::json!({
        "name": "large_tool",
        "description": "x".repeat(400),
    })];

    let without_tools = evaluate_context_budget(&messages, &[], 1_200, 1_000, 200);
    let with_tools = evaluate_context_budget(&messages, &schemas, 1_200, 1_000, 200);

    assert_eq!(with_tools.prompt_tokens, without_tools.prompt_tokens);
    assert_eq!(
        with_tools.tool_schema_tokens,
        estimate_tool_schema_tokens(&schemas)
    );
    assert_eq!(
        with_tools.available_input_tokens,
        1_000usize.saturating_sub(with_tools.tool_schema_tokens)
    );
    assert!(with_tools.compaction_budget < without_tools.compaction_budget);
}

#[test]
fn context_budget_saturates_when_tool_schemas_exceed_input_limit() {
    let messages = vec![ChatMessage::user("hello")];
    let schemas = vec![serde_json::json!({"description": "x".repeat(400)})];

    let metadata = evaluate_context_budget(&messages, &schemas, 100, 1, 99);

    assert!(metadata.tool_schema_tokens > metadata.max_input_tokens as usize);
    assert_eq!(metadata.available_input_tokens, 0);
    assert_eq!(metadata.compaction_budget, 0);
    assert!(metadata.compaction_required);
}

#[test]
fn context_budget_triggers_at_threshold() {
    let messages = vec![ChatMessage::user("a".repeat(400))];
    let prompt_tokens = estimate_prompt_tokens(&messages);
    let max_input_tokens = u32::try_from(prompt_tokens * 100 / AUTO_COMPACTION_RATIO)
        .expect("test input limit should fit u32");
    let metadata = evaluate_context_budget(
        &messages,
        &[],
        max_input_tokens + 100,
        max_input_tokens,
        100,
    );

    assert!(metadata.prompt_tokens >= metadata.compaction_budget);
    assert!(metadata.compaction_required);
}

#[test]
fn context_budget_never_mutates_prompt_messages() {
    let messages = vec![
        ChatMessage::user("question"),
        ChatMessage::tool("call-1", "full tool result".repeat(100)),
    ];
    let before: Vec<serde_json::Value> =
        messages.iter().map(ChatMessage::to_openai_value).collect();

    let _ = evaluate_context_budget(&messages, &[], 200, 100, 100);

    let after: Vec<serde_json::Value> = messages.iter().map(ChatMessage::to_openai_value).collect();
    assert_eq!(after, before);
}

#[test]
fn checkpoint_resume_bypasses_only_the_first_automatic_checkpoint_gate() {
    let limits = AgentLoopLimits {
        automatic_checkpointing: true,
        resume_after_checkpoint: true,
        ..test_agent_loop_limits()
    };
    let metadata = chelix_sessions::message::ContextBudgetMetadata {
        context_window: 100,
        prompt_tokens: 89,
        available_input_tokens: 100,
        compaction_required: true,
        ..Default::default()
    };

    assert!(!super::super::should_trigger_automatic_checkpoint(
        &limits, 1, &metadata
    ));
    assert!(super::super::should_trigger_automatic_checkpoint(
        &limits, 2, &metadata
    ));

    let at_hard_limit = chelix_sessions::message::ContextBudgetMetadata {
        prompt_tokens: 100,
        ..metadata
    };
    assert!(super::super::should_trigger_automatic_checkpoint(
        &limits,
        1,
        &at_hard_limit
    ));
}

fn expected_compacted_floor(
    actual_system: &str,
    alternate_system: &str,
    continuation: &[ChatMessage],
) -> CompactedPromptTokenFloor {
    let fixed_tokens = estimate_message_tokens(&ChatMessage::user(
        "<conversation-summary>\nx\n</conversation-summary>",
    ))
    .saturating_add(estimate_prompt_tokens(continuation));
    CompactedPromptTokenFloor {
        expected_system: estimate_message_tokens(&ChatMessage::system(actual_system))
            .saturating_add(fixed_tokens),
        alternate_system: estimate_message_tokens(&ChatMessage::system(alternate_system))
            .saturating_add(fixed_tokens),
    }
}

#[test]
fn compacted_prompt_floor_returns_exact_system_variants_without_mutation() {
    let reminder_segment = "\n\n<REMINDER>\ntask\n</REMINDER>";
    let system_with_reminder = format!("system{reminder_segment}");
    let continuation = vec![ChatMessage::user("continuation")];
    let request = ContextCompactionRequest {
        metadata: Default::default(),
        summary_messages: vec![
            ChatMessage::system(system_with_reminder.clone()),
            ChatMessage::user("<conversation-summary>\nsummary\n</conversation-summary>"),
        ],
        continuation_messages: continuation.clone(),
        tool_schemas: Vec::new(),
        provider_calls_started: 0,
        completed_iterations: 0,
        tool_calls_made: 0,
        usage: Default::default(),
        raw_llm_responses: Vec::new(),
    };
    let original_summary = request
        .summary_messages
        .iter()
        .map(ChatMessage::to_openai_value)
        .collect::<Vec<_>>();

    let floor = request
        .compacted_prompt_token_floor(reminder_segment)
        .expect("exact post-checkpoint prefix should produce a token floor");

    assert_eq!(
        floor,
        expected_compacted_floor(&system_with_reminder, "system", &continuation)
    );
    assert_eq!(
        request
            .summary_messages
            .iter()
            .map(ChatMessage::to_openai_value)
            .collect::<Vec<_>>(),
        original_summary
    );
}

#[test]
fn compacted_prompt_floor_rejects_system_without_exact_segment() {
    let request = ContextCompactionRequest {
        metadata: Default::default(),
        summary_messages: vec![
            ChatMessage::system("hook-modified system"),
            ChatMessage::user("<conversation-summary>\nsummary\n</conversation-summary>"),
        ],
        continuation_messages: vec![ChatMessage::user("continuation")],
        tool_schemas: Vec::new(),
        provider_calls_started: 0,
        completed_iterations: 0,
        tool_calls_made: 0,
        usage: Default::default(),
        raw_llm_responses: Vec::new(),
    };

    assert_eq!(
        request.compacted_prompt_token_floor("\n\n<REMINDER>\ntask\n</REMINDER>"),
        None
    );
}

#[test]
fn compacted_prompt_floor_accepts_post_hook_text_around_segment() {
    let reminder_segment = "\n\n<REMINDER>\ntask\n</REMINDER>";
    let actual_system = format!("safety prefix\nsystem{reminder_segment}\nsafety suffix");
    let alternate_system = "safety prefix\nsystem\nsafety suffix";
    let continuation = vec![ChatMessage::user("continuation")];
    let request = ContextCompactionRequest {
        metadata: Default::default(),
        summary_messages: vec![
            ChatMessage::system(actual_system.clone()),
            ChatMessage::user("<conversation-summary>\nsummary\n</conversation-summary>"),
        ],
        continuation_messages: continuation.clone(),
        tool_schemas: Vec::new(),
        provider_calls_started: 0,
        completed_iterations: 0,
        tool_calls_made: 0,
        usage: Default::default(),
        raw_llm_responses: Vec::new(),
    };
    let original_summary = request
        .summary_messages
        .iter()
        .map(ChatMessage::to_openai_value)
        .collect::<Vec<_>>();

    let floor = request
        .compacted_prompt_token_floor(reminder_segment)
        .expect("post-hook text should preserve one exact reminder segment");

    assert_eq!(
        floor,
        expected_compacted_floor(&actual_system, alternate_system, &continuation)
    );
    assert_eq!(
        request
            .summary_messages
            .iter()
            .map(ChatMessage::to_openai_value)
            .collect::<Vec<_>>(),
        original_summary
    );
}

#[test]
fn compacted_prompt_floor_rejects_overlapping_reminder_segments() {
    let repeated = "\n\n<REMINDER>\nx\n</REMINDER>";
    let reminder_segment = format!("{repeated}{repeated}");
    let request = ContextCompactionRequest {
        metadata: Default::default(),
        summary_messages: vec![
            ChatMessage::system(format!("system{reminder_segment}{repeated}")),
            ChatMessage::user("<conversation-summary>\nsummary\n</conversation-summary>"),
        ],
        continuation_messages: vec![ChatMessage::user("continuation")],
        tool_schemas: Vec::new(),
        provider_calls_started: 0,
        completed_iterations: 0,
        tool_calls_made: 0,
        usage: Default::default(),
        raw_llm_responses: Vec::new(),
    };

    assert_eq!(
        request.compacted_prompt_token_floor(&reminder_segment),
        None
    );
}

#[tokio::test]
async fn checkpoint_resume_token_floor_uses_post_hook_system_text() {
    let provider = Arc::new(ResumeProvider {
        seen_messages: std::sync::Mutex::new(Vec::new()),
    });
    let reminder_segment = "\n\n<REMINDER>\ntask\n</REMINDER>";
    let system_prompt = format!("system{reminder_segment}");
    let post_hook_system = format!("safety prefix\n{system_prompt}\nsafety suffix");
    let alternate_system = "safety prefix\nsystem\nsafety suffix";
    let mut hooks = HookRegistry::new();
    hooks.register(Arc::new(SurroundSystemBeforeLlmHook {
        prefix: "safety prefix\n",
        suffix: "\nsafety suffix",
    }));
    let result = run_agent_loop_with_context_and_limits(
        provider.clone(),
        &ToolRegistry::new(),
        &system_prompt,
        &UserContent::text("ignored while resuming"),
        None,
        Some(vec![
            ChatMessage::user("<conversation-summary>\nsummary\n</conversation-summary>"),
            ChatMessage::tool("call-1", "large persisted result".repeat(400_000)),
        ]),
        None,
        None,
        Some(Arc::new(hooks)),
        None,
        AgentLoopLimits {
            automatic_checkpointing: true,
            resume_from_history: true,
            resume_after_checkpoint: true,
            ..test_agent_loop_limits()
        },
    )
    .await;

    let Err(AgentRunError::ContextCompactionRequired(request)) = result else {
        panic!("post-hook prompt should require compaction");
    };
    assert!(matches!(
        request.summary_messages.first(),
        Some(ChatMessage::System { content }) if content == &post_hook_system
    ));
    let original_summary = request
        .summary_messages
        .iter()
        .map(ChatMessage::to_openai_value)
        .collect::<Vec<_>>();
    let floor = request
        .compacted_prompt_token_floor(reminder_segment)
        .expect("post-hook request should preserve one exact reminder segment");
    assert_eq!(
        floor,
        expected_compacted_floor(
            &post_hook_system,
            alternate_system,
            &request.continuation_messages,
        )
    );
    assert_eq!(
        request
            .summary_messages
            .iter()
            .map(ChatMessage::to_openai_value)
            .collect::<Vec<_>>(),
        original_summary
    );
    assert!(
        provider
            .seen_messages
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .is_empty()
    );
}

#[tokio::test]
async fn transport_retry_reports_started_provider_call_before_compaction() {
    let partial = "partial provider output".repeat(20_000);
    let provider = Arc::new(PartialRetryProvider {
        stream_calls: AtomicUsize::new(0),
        partial: partial.clone(),
    });
    let result = run_agent_loop_with_context_and_limits(
        provider.clone(),
        &ToolRegistry::new(),
        "system",
        &UserContent::text("ignored while resuming"),
        None,
        Some(vec![ChatMessage::user(
            "<conversation-summary>\nsummary\n</conversation-summary>",
        )]),
        None,
        None,
        None,
        None,
        AgentLoopLimits {
            automatic_checkpointing: true,
            resume_from_history: true,
            resume_after_checkpoint: true,
            ..test_agent_loop_limits()
        },
    )
    .await;

    let Err(AgentRunError::ContextCompactionRequired(request)) = result else {
        panic!("partial retry context should require compaction");
    };
    assert_eq!(provider.stream_calls.load(Ordering::SeqCst), 1);
    assert_eq!(request.completed_iterations, 0);
    assert_eq!(request.provider_calls_started, 1);
    assert!(request.metadata.prompt_tokens >= request.metadata.available_input_tokens);
    assert!(matches!(
        request.continuation_messages.as_slice(),
        [ChatMessage::Assistant {
            content: Some(content),
            ..
        }] if content == &partial
    ));
}

#[tokio::test]
async fn checkpoint_resume_uses_post_hook_payload_for_hard_limit_gate() {
    let provider = Arc::new(ResumeProvider {
        seen_messages: std::sync::Mutex::new(Vec::new()),
    });
    let replacement = serde_json::json!([
        {"role": "system", "content": "system with reminder"},
        {"role": "user", "content": "<conversation-summary>\nshort\n</conversation-summary>"},
        {"role": "tool", "tool_call_id": "call-1", "content": "shortened result"}
    ]);
    let mut hooks = HookRegistry::new();
    hooks.register(Arc::new(ReplaceBeforeLlmHook {
        messages: replacement,
    }));
    let result = run_agent_loop_with_context_and_limits(
        provider.clone(),
        &ToolRegistry::new(),
        "system with reminder",
        &UserContent::text("ignored while resuming"),
        None,
        Some(vec![
            ChatMessage::user("<conversation-summary>\nsummary\n</conversation-summary>"),
            ChatMessage::tool("call-1", "large persisted result".repeat(20_000)),
        ]),
        None,
        None,
        Some(Arc::new(hooks)),
        None,
        AgentLoopLimits {
            automatic_checkpointing: true,
            resume_from_history: true,
            resume_after_checkpoint: true,
            ..test_agent_loop_limits()
        },
    )
    .await;

    assert!(result.is_ok());
    let seen = provider
        .seen_messages
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    assert_eq!(seen.len(), 3);
    assert_eq!(seen[2].to_openai_value()["content"], "shortened result");
}

#[test]
fn zero_input_capacity_requires_compaction() {
    let messages = vec![ChatMessage::user("hello")];
    let metadata = evaluate_context_budget(&messages, &[], 0, 0, 0);

    assert_eq!(metadata.context_window, 0);
    assert_eq!(metadata.available_input_tokens, 0);
    assert_eq!(metadata.compaction_budget, 0);
    assert_eq!(metadata.usage_percent, 0);
    assert!(metadata.compaction_required);
}

#[tokio::test]
async fn automatic_checkpoint_trigger_stops_before_provider_call() {
    let provider = Arc::new(ThresholdProvider {
        complete_calls: AtomicUsize::new(0),
    });
    let result = run_agent_loop_with_context_and_limits(
        provider.clone(),
        &ToolRegistry::new(),
        "system prompt",
        &UserContent::text("user prompt that exceeds the tiny context window"),
        None,
        None,
        None,
        None,
        None,
        None,
        AgentLoopLimits {
            automatic_checkpointing: true,
            ..test_agent_loop_limits()
        },
    )
    .await;

    let Err(AgentRunError::ContextCompactionRequired(request)) = result else {
        panic!("expected automatic checkpoint request");
    };
    assert_eq!(provider.complete_calls.load(Ordering::SeqCst), 0);
    assert!(request.metadata.compaction_required);
    assert_eq!(request.metadata.compaction_ratio, 85);
    assert_eq!(request.completed_iterations, 0);
    assert!(matches!(
        request.summary_messages.first(),
        Some(ChatMessage::System { .. })
    ));
    assert!(matches!(
        request.summary_messages.last(),
        Some(ChatMessage::User { .. })
    ));
    assert!(request.continuation_messages.is_empty());
}

#[test]
fn compaction_split_preserves_current_user_and_first_tool_round() {
    let messages = vec![
        ChatMessage::system("system"),
        ChatMessage::user("old request"),
        ChatMessage::assistant("old answer"),
        ChatMessage::user("current request"),
        ChatMessage::assistant_with_tools(None, vec![tool_call("call-1")]),
        ChatMessage::tool("call-1", "result"),
    ];

    let (summary, continuation) = super::super::split_context_for_compaction(messages, 3);

    assert_eq!(summary.len(), 3);
    assert!(matches!(
        continuation.first(),
        Some(ChatMessage::User { .. })
    ));
    assert!(matches!(
        continuation.get(1),
        Some(ChatMessage::Assistant { tool_calls, .. }) if !tool_calls.is_empty()
    ));
    assert!(matches!(
        continuation.get(2),
        Some(ChatMessage::Tool { .. })
    ));
}

#[test]
fn compaction_split_preserves_only_latest_tool_round_after_multiple_rounds() {
    let messages = vec![
        ChatMessage::system("system"),
        ChatMessage::user("current request"),
        ChatMessage::assistant_with_tools(None, vec![tool_call("call-1")]),
        ChatMessage::tool("call-1", "first result"),
        ChatMessage::assistant_with_tools(None, vec![tool_call("call-2")]),
        ChatMessage::tool("call-2", "second result"),
    ];

    let (summary, continuation) = super::super::split_context_for_compaction(messages, 4);

    assert_eq!(summary.len(), 4);
    assert!(matches!(
        continuation.first(),
        Some(ChatMessage::Assistant { tool_calls, .. }) if tool_calls[0].id == "call-2"
    ));
    assert!(matches!(
        continuation.get(1),
        Some(ChatMessage::Tool { .. })
    ));
}

fn tool_call(id: &str) -> crate::model::ToolCall {
    crate::model::ToolCall {
        id: id.to_string(),
        name: "read".to_string(),
        arguments: serde_json::json!({}),
        argument_diagnostic: None,
    }
}

#[tokio::test]
async fn isolated_runner_does_not_trigger_session_checkpointing() {
    let provider = Arc::new(ThresholdProvider {
        complete_calls: AtomicUsize::new(0),
    });
    let result = run_agent_loop_with_context_and_limits(
        provider.clone(),
        &ToolRegistry::new(),
        "system prompt",
        &UserContent::text("isolated sub-agent prompt"),
        None,
        None,
        None,
        None,
        None,
        None,
        AgentLoopLimits {
            automatic_checkpointing: false,
            ..test_agent_loop_limits()
        },
    )
    .await;

    assert!(result.is_ok());
    assert_eq!(provider.complete_calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn checkpoint_resume_does_not_repeat_original_user_message() {
    let provider = Arc::new(ResumeProvider {
        seen_messages: std::sync::Mutex::new(Vec::new()),
    });
    let checkpoint_history = vec![ChatMessage::user(
        "<conversation-summary>checkpoint state</conversation-summary>",
    )];
    let result = run_agent_loop_with_context_and_limits(
        provider.clone(),
        &ToolRegistry::new(),
        "system prompt",
        &UserContent::text("original user message"),
        None,
        Some(checkpoint_history),
        None,
        None,
        None,
        None,
        AgentLoopLimits {
            automatic_checkpointing: true,
            resume_from_history: true,
            ..test_agent_loop_limits()
        },
    )
    .await
    .expect("checkpoint resume should complete");

    assert_eq!(result.output.text, "continued");
    let seen = provider
        .seen_messages
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    assert_eq!(seen.len(), 2);
    assert!(matches!(&seen[0], ChatMessage::System { content } if content == "system prompt"));
    match &seen[1] {
        ChatMessage::User { content, .. } => {
            let text = format!("{content:?}");
            assert!(text.contains("checkpoint state"));
            assert!(!text.contains("original user message"));
        },
        other => panic!("expected checkpoint summary, got {other:?}"),
    }
}
