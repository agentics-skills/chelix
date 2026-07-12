//! Conversation summarization checkpoints.
//!
//! When a session approaches its context window (or the user runs
//! `/compact`), the session model itself summarizes the conversation and a
//! [`PersistedMessage::Checkpoint`] is appended to the session history. The
//! stored history is never mutated, so forks from any point keep working.
//! Context building (`values_to_chat_messages`) starts a fresh context
//! window from the latest checkpoint, injecting the summary as a
//! `<conversation-summary>` user message.
//!
//! The summarization prompt is adapted from the VS Code Copilot Chat
//! reference (`summarizedConversationHistory.tsx`). Unlike the reference,
//! the instructions ride in the trailing user message instead of replacing
//! the system prompt: the request keeps the session's system prompt, tool
//! schemas, and history byte-identical to the previous turn, so the
//! provider's prompt cache prefix stays valid and the history is billed as
//! cached input.

use std::sync::Arc;

use {
    chelix_agents::model::{LlmProvider, values_to_chat_messages},
    chelix_sessions::{PersistedMessage, store::SessionStore},
    tracing::info,
};

use crate::error::{self, Error};

/// Fraction of the token budget at which automatic summarization fires.
///
/// Matches the reference implementation's summarization budget factor
/// (`agentIntent.ts`): the estimated next request is compared against
/// `budget × 0.85` so summarization runs before the provider rejects the
/// request outright.
pub(crate) const AUTO_COMPACT_SAFETY_FACTOR: f64 = 0.85;

/// Summarization instructions.
///
/// Adapted from the `SummaryPrompt` element in the VS Code Copilot Chat
/// reference (`summarizedConversationHistory.tsx`). Sent as part of the
/// trailing user message (not the system prompt) so the request prefix —
/// session system prompt, tool schemas, history — matches the previous turn
/// and hits the provider's prompt cache.
pub(crate) const SUMMARY_INSTRUCTIONS: &str = r#"Your task is to create a comprehensive, detailed summary of the entire conversation that captures all essential information needed to seamlessly continue the work without any loss of context. This summary will be used to compact the conversation while preserving critical technical details, decisions, and progress.

## Recent Context Analysis

Pay special attention to the most recent agent commands and tool executions that led to this summarization being triggered. Include:
- **Last Agent Commands**: What specific actions/tools were just executed
- **Tool Results**: Key outcomes from recent tool calls (truncate if very long, but preserve essential information)
- **Immediate State**: What was the system doing right before summarization
- **Triggering Context**: What caused the token budget to be exceeded

## Analysis Process

Before providing your final summary, wrap your analysis in `<analysis>` tags to organize your thoughts systematically:

1. **Chronological Review**: Go through the conversation chronologically, identifying key phases and transitions
2. **Intent Mapping**: Extract all explicit and implicit user requests, goals, and expectations
3. **Technical Inventory**: Catalog all technical concepts, tools, frameworks, and architectural decisions
4. **Code Archaeology**: Document all files, functions, and code patterns that were discussed or modified
5. **Progress Assessment**: Evaluate what has been completed vs. what remains pending
6. **Context Validation**: Ensure all critical information for continuation is captured
7. **Recent Commands Analysis**: Document the specific agent commands and tool results from the most recent operations

## Summary Structure

Your summary must include these sections in order, following the exact format below:

<analysis>
[Chronological Review: Walk through conversation phases: initial request → exploration → implementation → debugging → current state]
[Intent Mapping: List each explicit user request with message context]
[Technical Inventory: Catalog all technologies, patterns, and decisions mentioned]
[Code Archaeology: Document every file, function, and code change discussed]
[Progress Assessment: What's done vs. pending with specific status]
[Context Validation: Verify all continuation context is captured]
[Recent Commands Analysis: Last agent commands executed, tool results (truncated if long), immediate pre-summarization state]
</analysis>

<summary>
1. Conversation Overview:
- Primary Objectives: [All explicit user requests and overarching goals with exact quotes]
- Session Context: [High-level narrative of conversation flow and key phases]
- User Intent Evolution: [How user's needs or direction changed throughout conversation]

2. Technical Foundation:
- [Core Technology 1]: [Version/details and purpose]
- [Framework/Library 2]: [Configuration and usage context]
- [Architectural Pattern 3]: [Implementation approach and reasoning]
- [Environment Detail 4]: [Setup specifics and constraints]

3. Codebase Status:
- [File Name 1]:
- Purpose: [Why this file is important to the project]
- Current State: [Summary of recent changes or modifications]
- Key Code Segments: [Important functions/classes with brief explanations]
- Dependencies: [How this relates to other components]
- [File Name 2]:
- Purpose: [Role in the project]
- Current State: [Modification status]
- Key Code Segments: [Critical code blocks]
- [Additional files as needed]

4. Problem Resolution:
- Issues Encountered: [Technical problems, bugs, or challenges faced]
- Solutions Implemented: [How problems were resolved and reasoning]
- Debugging Context: [Ongoing troubleshooting efforts or known issues]
- Lessons Learned: [Important insights or patterns discovered]

5. Progress Tracking:
- Completed Tasks: [What has been successfully implemented with status indicators]
- Partially Complete Work: [Tasks in progress with current completion status]
- Validated Outcomes: [Features or code confirmed working through testing]

6. Active Work State:
- Current Focus: [Precisely what was being worked on in most recent messages]
- Recent Context: [Detailed description of last few conversation exchanges]
- Working Code: [Code snippets being modified or discussed recently]
- Immediate Context: [Specific problem or feature being addressed before summary]

7. Recent Operations:
- Last Agent Commands: [Specific tools/actions executed just before summarization with exact command names]
- Tool Results Summary: [Key outcomes from recent tool executions - truncate long results but keep essential info]
- Pre-Summary State: [What the agent was actively doing when token budget was exceeded]
- Operation Context: [Why these specific commands were executed and their relationship to user goals]

8. Continuation Plan:
- [Pending Task 1]: [Details and specific next steps with verbatim quotes]
- [Pending Task 2]: [Requirements and continuation context]
- [Priority Information]: [Which tasks are most urgent or logically sequential]
- [Next Action]: [Immediate next step with direct quotes from recent messages]
</summary>

## Quality Guidelines

- **Precision**: Include exact filenames, function names, variable names, and technical terms
- **Completeness**: Capture all context needed to continue without re-reading the full conversation
- **Clarity**: Write for someone who needs to pick up exactly where the conversation left off
- **Verbatim Accuracy**: Use direct quotes for task specifications and recent work context
- **Technical Depth**: Include enough detail for complex technical decisions and code patterns
- **Logical Flow**: Present information in a way that builds understanding progressively

This summary should serve as a comprehensive handoff document that enables seamless continuation of all active work streams while preserving the full technical and contextual richness of the original conversation."#;

/// Trailing request appended after [`SUMMARY_INSTRUCTIONS`] in the
/// summarization user message.
///
/// Adapted from the `UserMessage` element in
/// `ConversationHistorySummarizationPrompt` in the reference.
pub(crate) const SUMMARY_REQUEST: &str = r#"Summarize the conversation history so far, paying special attention to the most recent agent commands and tool results that triggered this summarization. Structure your summary using the enhanced format provided above.

IMPORTANT: Do NOT call any tools. Your only task is to generate a text summary of the conversation. Do not attempt to execute any actions or make any tool calls.

Focus particularly on:
- The specific agent commands/tools that were just executed
- The results returned from these recent tool calls (truncate if very long but preserve key information)
- What the agent was actively working on when the token budget was exceeded
- How these recent operations connect to the overall user goals

Include all important tool calls and their results as part of the appropriate sections, with special emphasis on the most recent operations."#;

/// Result of a successful summarization checkpoint.
#[derive(Debug, Clone)]
pub(crate) struct CheckpointOutcome {
    /// The persisted checkpoint message exactly as appended to the store.
    pub message: serde_json::Value,
    /// Index of the checkpoint in the session history (0-based).
    pub index: usize,
    /// Model that produced the summary (the session model).
    pub model: String,
    /// Input tokens spent on the summarization call.
    pub input_tokens: u32,
    /// Output tokens produced by the summarization call.
    pub output_tokens: u32,
    /// Number of history messages covered by this checkpoint.
    pub messages_summarized: u32,
}

impl CheckpointOutcome {
    /// Broadcast payload fields describing this checkpoint.
    #[must_use]
    pub fn broadcast_metadata(&self) -> serde_json::Value {
        serde_json::json!({
            "messageIndex": self.index,
            "checkpoint": self.message,
        })
    }
}

/// Compute the token count at which automatic summarization fires.
///
/// `threshold_tokens = 0` (the default) uses the model's context window as
/// the budget. The budget is scaled by [`AUTO_COMPACT_SAFETY_FACTOR`] and
/// floored at 1 so a zero-context window still yields a valid threshold.
#[must_use]
pub(crate) fn auto_compact_threshold(context_window_tokens: u64, threshold_tokens: u32) -> u64 {
    let budget = if threshold_tokens > 0 {
        u64::from(threshold_tokens)
    } else {
        context_window_tokens
    };
    (((budget as f64) * AUTO_COMPACT_SAFETY_FACTOR).floor() as u64).max(1)
}

/// Summarize the session and append a checkpoint message.
///
/// The request is built to share the provider prompt-cache prefix with the
/// previous regular turn: the caller passes the session's own system prompt
/// and native tool schemas, the exact stored history is converted with the
/// same [`values_to_chat_messages`] used for regular runs (so a previous
/// checkpoint already scopes the context), and the summarization
/// instructions ride in a single trailing user message. The resulting
/// summary is appended as a [`PersistedMessage::Checkpoint`] — nothing in
/// the existing history is modified.
pub(crate) async fn summarize_session(
    store: &Arc<SessionStore>,
    session_key: &str,
    provider: &dyn LlmProvider,
    system_prompt: &str,
    tools: &[serde_json::Value],
) -> error::Result<CheckpointOutcome> {
    let history = store
        .read(session_key)
        .await
        .map_err(|source| Error::external("failed to read session history", source))?;

    if history.is_empty() {
        return Err(Error::message("nothing to compact"));
    }
    if history
        .last()
        .and_then(|m| m.get("role"))
        .and_then(serde_json::Value::as_str)
        == Some("checkpoint")
    {
        return Err(Error::message(
            "nothing to compact: session already ends with a checkpoint",
        ));
    }

    // Exact session history, unmutated. A previous checkpoint (if any) is
    // rendered as its <conversation-summary> user message by the shared
    // conversion, so iterative re-summarization builds on the prior summary.
    let mut messages = vec![chelix_agents::ChatMessage::system(system_prompt)];
    messages.extend(values_to_chat_messages(&history));
    messages.push(chelix_agents::ChatMessage::user(format!(
        "{SUMMARY_INSTRUCTIONS}\n\n{SUMMARY_REQUEST}"
    )));

    let response = provider
        .complete(&messages, tools)
        .await
        .map_err(|e| Error::message(format!("summarization request failed: {e}")))?;

    let summary = response
        .text
        .as_deref()
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .ok_or_else(|| Error::message("summarization returned an empty response"))?;

    let checkpoint = PersistedMessage::checkpoint(
        summary,
        provider.id(),
        provider.name(),
        response.usage.input_tokens,
        response.usage.output_tokens,
        u32::try_from(history.len()).unwrap_or(u32::MAX),
    );
    let message = checkpoint.to_value();

    store
        .append(session_key, &message)
        .await
        .map_err(|source| Error::external("failed to append checkpoint", source))?;

    info!(
        session = %session_key,
        model = provider.id(),
        input_tokens = response.usage.input_tokens,
        output_tokens = response.usage.output_tokens,
        messages_summarized = history.len(),
        "compaction checkpoint appended"
    );

    Ok(CheckpointOutcome {
        message,
        index: history.len(),
        model: provider.id().to_string(),
        input_tokens: response.usage.input_tokens,
        output_tokens: response.usage.output_tokens,
        messages_summarized: u32::try_from(history.len()).unwrap_or(u32::MAX),
    })
}

#[allow(clippy::unwrap_used, clippy::expect_used)]
#[cfg(test)]
mod tests {
    use std::pin::Pin;

    use {
        chelix_agents::model::{ChatMessage, CompletionResponse, LlmProvider, StreamEvent, Usage},
        tokio_stream::Stream,
    };

    use super::*;

    struct MockProvider {
        response_text: Option<String>,
        seen_messages: std::sync::Mutex<Vec<ChatMessage>>,
        seen_tools: std::sync::Mutex<Vec<serde_json::Value>>,
    }

    impl MockProvider {
        fn new(response_text: Option<&str>) -> Self {
            Self {
                response_text: response_text.map(str::to_string),
                seen_messages: std::sync::Mutex::new(Vec::new()),
                seen_tools: std::sync::Mutex::new(Vec::new()),
            }
        }
    }

    #[async_trait::async_trait]
    impl LlmProvider for MockProvider {
        fn name(&self) -> &str {
            "mock"
        }

        fn id(&self) -> &str {
            "mock-model"
        }

        async fn complete(
            &self,
            messages: &[ChatMessage],
            tools: &[serde_json::Value],
        ) -> anyhow::Result<CompletionResponse> {
            *self.seen_messages.lock().unwrap_or_else(|e| e.into_inner()) = messages.to_vec();
            *self.seen_tools.lock().unwrap_or_else(|e| e.into_inner()) = tools.to_vec();
            Ok(CompletionResponse {
                text: self.response_text.clone(),
                tool_calls: Vec::new(),
                usage: Usage {
                    input_tokens: 100,
                    output_tokens: 50,
                    ..Usage::default()
                },
            })
        }

        fn stream(
            &self,
            _messages: Vec<ChatMessage>,
        ) -> Pin<Box<dyn Stream<Item = StreamEvent> + Send + '_>> {
            Box::pin(tokio_stream::empty())
        }
    }

    fn test_store() -> (tempfile::TempDir, Arc<SessionStore>) {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = Arc::new(SessionStore::new(dir.path().to_path_buf()));
        (dir, store)
    }

    async fn seed_history(store: &Arc<SessionStore>, key: &str) {
        store
            .append(key, &PersistedMessage::user("hello").to_value())
            .await
            .unwrap();
        store
            .append(
                key,
                &PersistedMessage::assistant("hi there", "mock-model", "mock", 10, 5, None)
                    .to_value(),
            )
            .await
            .unwrap();
    }

    // ── auto_compact_threshold ───────────────────────────────────────

    #[test]
    fn threshold_uses_context_window_when_zero() {
        assert_eq!(auto_compact_threshold(200_000, 0), 170_000);
    }

    #[test]
    fn threshold_uses_configured_tokens_when_set() {
        assert_eq!(auto_compact_threshold(200_000, 100_000), 85_000);
    }

    #[test]
    fn threshold_floors_at_one() {
        assert_eq!(auto_compact_threshold(0, 0), 1);
    }

    // ── summarize_session ────────────────────────────────────────────

    #[tokio::test]
    async fn summarize_appends_checkpoint_without_mutating_history() {
        let (_dir, store) = test_store();
        seed_history(&store, "s1").await;
        let before = store.read("s1").await.unwrap();

        let provider = MockProvider::new(Some("<summary>the summary</summary>"));
        let outcome = summarize_session(&store, "s1", &provider, "session system prompt", &[])
            .await
            .unwrap();

        let after = store.read("s1").await.unwrap();
        assert_eq!(after.len(), before.len() + 1);
        // Prior history is byte-identical — never mutated.
        assert_eq!(&after[..before.len()], &before[..]);

        let checkpoint = &after[before.len()];
        assert_eq!(checkpoint["role"], "checkpoint");
        assert_eq!(checkpoint["summary"], "<summary>the summary</summary>");
        assert_eq!(checkpoint["model"], "mock-model");
        assert_eq!(checkpoint["inputTokens"], 100);
        assert_eq!(checkpoint["outputTokens"], 50);
        assert_eq!(checkpoint["messagesSummarized"], 2);

        assert_eq!(outcome.index, before.len());
        assert_eq!(outcome.model, "mock-model");
        assert_eq!(outcome.messages_summarized, 2);
    }

    #[tokio::test]
    async fn summarize_request_shares_session_prompt_prefix() {
        let (_dir, store) = test_store();
        seed_history(&store, "s5").await;

        let provider = MockProvider::new(Some("summary"));
        let tools = vec![serde_json::json!({"name": "read_file"})];
        summarize_session(&store, "s5", &provider, "session system prompt", &tools)
            .await
            .unwrap();

        // Prefix matches a regular turn: session system prompt first, exact
        // history next, summarization instructions only in the trailing user
        // message — so the provider prompt cache stays valid.
        let seen = provider
            .seen_messages
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        assert!(
            matches!(&seen[0], ChatMessage::System { content } if content == "session system prompt")
        );
        assert_eq!(seen.len(), 4); // system + user + assistant + summary request
        match seen.last().unwrap() {
            ChatMessage::User { content, .. } => {
                let text = format!("{content:?}");
                assert!(text.contains("Summarize the conversation history"));
                assert!(text.contains("Do NOT call any tools"));
            },
            other => panic!("expected trailing user message, got {other:?}"),
        }
        // Session tool schemas are forwarded unchanged.
        let seen_tools = provider
            .seen_tools
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        assert_eq!(seen_tools, tools);
    }

    #[tokio::test]
    async fn summarize_empty_session_errors() {
        let (_dir, store) = test_store();
        let provider = MockProvider::new(Some("summary"));
        let err = summarize_session(&store, "empty", &provider, "sys", &[])
            .await
            .unwrap_err();
        assert!(err.to_string().contains("nothing to compact"));
    }

    #[tokio::test]
    async fn summarize_rejects_double_checkpoint() {
        let (_dir, store) = test_store();
        seed_history(&store, "s2").await;
        let provider = MockProvider::new(Some("summary"));
        summarize_session(&store, "s2", &provider, "sys", &[])
            .await
            .unwrap();
        let err = summarize_session(&store, "s2", &provider, "sys", &[])
            .await
            .unwrap_err();
        assert!(err.to_string().contains("already ends with a checkpoint"));
    }

    #[tokio::test]
    async fn summarize_empty_llm_response_errors() {
        let (_dir, store) = test_store();
        seed_history(&store, "s3").await;
        let provider = MockProvider::new(Some("   "));
        let err = summarize_session(&store, "s3", &provider, "sys", &[])
            .await
            .unwrap_err();
        assert!(err.to_string().contains("empty response"));
    }

    #[tokio::test]
    async fn context_restarts_from_latest_checkpoint() {
        let (_dir, store) = test_store();
        seed_history(&store, "s4").await;
        let provider = MockProvider::new(Some("compacted context"));
        summarize_session(&store, "s4", &provider, "sys", &[])
            .await
            .unwrap();
        store
            .append("s4", &PersistedMessage::user("next question").to_value())
            .await
            .unwrap();

        let history = store.read("s4").await.unwrap();
        let context = values_to_chat_messages(&history);
        // Checkpoint summary + tail only; pre-checkpoint messages excluded.
        assert_eq!(context.len(), 2);
        match &context[0] {
            ChatMessage::User { content, .. } => {
                let text = format!("{content:?}");
                assert!(text.contains("<conversation-summary>"));
                assert!(text.contains("compacted context"));
            },
            other => panic!("expected user summary message, got {other:?}"),
        }
    }
}
