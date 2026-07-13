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

    fn context_window(&self) -> u32 {
        10
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
    let metadata = evaluate_context_budget(&messages, &[], 100_000);

    assert_eq!(metadata.context_window, 100_000);
    assert_eq!(metadata.compaction_ratio, AUTO_COMPACTION_RATIO);
    assert_eq!(metadata.compaction_budget, 85_000);
    assert!(!metadata.compaction_required);
}

#[test]
fn context_budget_reports_real_usage_percent() {
    let messages = vec![ChatMessage::user("a".repeat(400))];
    let metadata = evaluate_context_budget(&messages, &[], 1_000);

    assert_eq!(
        metadata.current_tokens,
        estimate_prompt_tokens(&messages, &[])
    );
    assert_eq!(
        metadata.usage_percent,
        metadata.current_tokens * 100 / 1_000
    );
}

#[test]
fn context_budget_includes_tool_schemas() {
    let messages = vec![ChatMessage::user("hello")];
    let schemas = vec![serde_json::json!({
        "name": "large_tool",
        "description": "x".repeat(400),
    })];

    let without_tools = evaluate_context_budget(&messages, &[], 1_000);
    let with_tools = evaluate_context_budget(&messages, &schemas, 1_000);

    assert!(with_tools.current_tokens > without_tools.current_tokens);
}

#[test]
fn context_budget_triggers_at_threshold() {
    let messages = vec![ChatMessage::user("a".repeat(400))];
    let current_tokens = estimate_prompt_tokens(&messages, &[]);
    let context_window = u32::try_from(current_tokens * 100 / AUTO_COMPACTION_RATIO)
        .expect("test context window should fit u32");
    let metadata = evaluate_context_budget(&messages, &[], context_window);

    assert!(metadata.current_tokens >= metadata.compaction_budget);
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

    let _ = evaluate_context_budget(&messages, &[], 100);

    let after: Vec<serde_json::Value> = messages.iter().map(ChatMessage::to_openai_value).collect();
    assert_eq!(after, before);
}

#[test]
fn zero_context_window_reports_without_triggering() {
    let messages = vec![ChatMessage::user("hello")];
    let metadata = evaluate_context_budget(&messages, &[], 0);

    assert_eq!(metadata.context_window, 0);
    assert_eq!(metadata.compaction_budget, 0);
    assert_eq!(metadata.usage_percent, 0);
    assert!(!metadata.compaction_required);
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
