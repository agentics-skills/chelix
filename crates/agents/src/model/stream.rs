use std::{pin::Pin, sync::Arc};

use {async_trait::async_trait, tokio_stream::Stream};

use super::{
    CompletionOptions, ReasoningEffort, ToolChoice,
    chat::ChatMessage,
    types::{CompletionResponse, Usage},
};

// ── Stream events ───────────────────────────────────────────────────────────

/// Events emitted during streaming LLM completion.
#[derive(Debug, Clone)]
pub enum StreamEvent {
    /// Append-only provider item update carrying canonical segment ID, item ID, position, seq, and payload.
    ProviderItemUpdate(chelix_common::ProviderItemUpdate),
    /// Provider response/attempt segment opened.
    SegmentStart {
        segment_id: chelix_common::ProviderSegmentId,
    },
    /// Provider response/attempt segment closed.
    SegmentClose {
        segment_id: chelix_common::ProviderSegmentId,
        outcome: chelix_common::ProviderSegmentOutcome,
        usage: Option<Usage>,
    },
    /// Text content delta.
    Delta(String),
    /// Raw provider event payload (for debugging API responses).
    ProviderRaw(serde_json::Value),
    /// A tool call has started (content_block_start with tool_use).
    ToolCallStart {
        /// Tool call ID from the provider.
        id: String,
        /// Tool name being called.
        name: String,
        /// Index of this tool call in the response (0-based).
        index: usize,
    },
    /// Streaming delta for tool call arguments (JSON fragment).
    ToolCallArgumentsDelta {
        /// Index of the tool call this delta belongs to.
        index: usize,
        /// JSON fragment to append to the arguments.
        delta: String,
    },
    /// A tool call's arguments are complete.
    ToolCallComplete {
        /// Index of the completed tool call.
        index: usize,
    },
    /// Stream completed successfully.
    Done(Usage),
    /// An error occurred.
    Error(String),
}

/// LLM provider trait.
#[async_trait]
pub trait LlmProvider: Send + Sync {
    fn name(&self) -> &str;

    /// Model identifier (e.g. "gpt-5.2").
    fn id(&self) -> &str;

    async fn complete(
        &self,
        messages: &[ChatMessage],
        tools: &[serde_json::Value],
    ) -> anyhow::Result<CompletionResponse>;

    async fn complete_with_options(
        &self,
        messages: &[ChatMessage],
        tools: &[serde_json::Value],
        options: &CompletionOptions,
    ) -> anyhow::Result<CompletionResponse> {
        options.reject_forced_tool_choice(self.name())?;
        if options.max_output_tokens.is_some() {
            anyhow::bail!(
                "provider {} does not support a per-request output token limit",
                self.name()
            );
        }
        self.complete(messages, tools).await
    }

    /// Whether this provider supports tool/function calling.
    /// Defaults to false; providers that handle the `tools` parameter
    /// in `complete()` should override this to return true.
    fn supports_tools(&self) -> bool {
        false
    }

    /// Total context window size in tokens for this model.
    fn context_window(&self) -> Option<u32> {
        None
    }

    /// Maximum input tokens accepted by this resolved model.
    fn max_input_tokens(&self) -> Option<u32> {
        None
    }

    /// Maximum output tokens produced by this resolved model.
    fn max_output_tokens(&self) -> Option<u32> {
        None
    }

    /// Whether this provider supports vision (image inputs).
    /// When true, tool results containing images will be sent as multimodal
    /// content blocks instead of stripping the image data.
    fn supports_vision(&self) -> bool {
        false
    }

    /// Configured tool mode for this provider.
    ///
    /// Defaults to native tool calling.
    fn tool_mode(&self) -> chelix_config::ToolMode {
        chelix_config::ToolMode::default()
    }

    /// Stream a completion, yielding delta/done/error events.
    fn stream(
        &self,
        messages: Vec<ChatMessage>,
    ) -> Pin<Box<dyn Stream<Item = StreamEvent> + Send + '_>>;

    /// Stream a completion with tool support.
    ///
    /// Like `stream()`, but accepts tool schemas and can emit `ToolCallStart`,
    /// `ToolCallArgumentsDelta`, and `ToolCallComplete` events in addition to
    /// text deltas.
    ///
    /// Default implementation falls back to `stream()` (ignoring tools).
    /// Providers with native streaming tool support should override this.
    fn stream_with_tools(
        &self,
        messages: Vec<ChatMessage>,
        _tools: Vec<serde_json::Value>,
    ) -> Pin<Box<dyn Stream<Item = StreamEvent> + Send + '_>> {
        self.stream(messages)
    }

    fn stream_with_tools_and_options(
        &self,
        messages: Vec<ChatMessage>,
        tools: Vec<serde_json::Value>,
        tool_choice: Option<ToolChoice>,
    ) -> Pin<Box<dyn Stream<Item = StreamEvent> + Send + '_>> {
        if let Err(error) = reject_unsupported_tool_choice(self.name(), tool_choice.as_ref()) {
            return Box::pin(tokio_stream::once(StreamEvent::Error(error.to_string())));
        }
        self.stream_with_tools(messages, tools)
    }

    /// Configured reasoning effort for this provider instance, if any.
    ///
    /// Providers that support extended thinking or reasoning
    /// use this value when building API requests.
    fn reasoning_effort(&self) -> Option<ReasoningEffort> {
        None
    }

    /// Return a new provider with reasoning effort set, if supported.
    ///
    /// Returns `None` for providers that don't support reasoning effort.
    /// Used to apply per-session reasoning settings without mutating the
    /// shared registry provider.
    fn with_reasoning_effort(
        self: Arc<Self>,
        _effort: ReasoningEffort,
    ) -> Option<Arc<dyn LlmProvider>> {
        None
    }
}

fn reject_unsupported_tool_choice(
    provider_name: &str,
    tool_choice: Option<&ToolChoice>,
) -> anyhow::Result<()> {
    if matches!(tool_choice, Some(ToolChoice::Tool { .. } | ToolChoice::Any)) {
        anyhow::bail!("provider {provider_name} does not support forced tool_choice");
    }
    Ok(())
}
