use {
    super::helpers::*,
    crate::model::{ChatMessage, CompletionResponse, LlmProvider, StreamEvent},
    anyhow::Result,
    async_trait::async_trait,
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

#[async_trait]
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

    async fn complete(
        &self,
        _messages: &[ChatMessage],
        _tools: &[serde_json::Value],
    ) -> Result<CompletionResponse> {
        self.complete_calls.fetch_add(1, Ordering::SeqCst);
        Ok(CompletionResponse {
            text: Some("provider must not be called".to_string()),
            tool_calls: Vec::new(),
            usage: Default::default(),
        })
    }

    fn stream(
        &self,
        _messages: Vec<ChatMessage>,
    ) -> Pin<Box<dyn Stream<Item = StreamEvent> + Send + '_>> {
        Box::pin(tokio_stream::empty())
    }
}

#[async_trait]
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

    async fn complete(
        &self,
        messages: &[ChatMessage],
        _tools: &[serde_json::Value],
    ) -> Result<CompletionResponse> {
        *self
            .seen_messages
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = messages.to_vec();
        Ok(CompletionResponse {
            text: Some("continued".to_string()),
            tool_calls: Vec::new(),
            usage: Default::default(),
        })
    }

    fn stream(
        &self,
        _messages: Vec<ChatMessage>,
    ) -> Pin<Box<dyn Stream<Item = StreamEvent> + Send + '_>> {
        Box::pin(tokio_stream::empty())
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
        ..Default::default()
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
        AgentLoopLimits {
            automatic_checkpointing: true,
            ..Default::default()
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
        metadata: None,
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
        AgentLoopLimits {
            automatic_checkpointing: false,
            ..Default::default()
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
        AgentLoopLimits {
            automatic_checkpointing: true,
            resume_from_history: true,
            ..Default::default()
        },
    )
    .await
    .expect("checkpoint resume should complete");

    assert_eq!(result.text, "continued");
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
