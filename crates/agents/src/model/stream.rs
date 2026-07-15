use std::{pin::Pin, sync::Arc, time::Duration};

use {async_trait::async_trait, futures::StreamExt, tokio_stream::Stream};

use super::{
    AgentToolControls, CompletionOptions, ReasoningEffort, ToolChoice,
    chat::ChatMessage,
    types::{CompletionResponse, Usage},
};

// ── Stream events ───────────────────────────────────────────────────────────

/// Events emitted during streaming LLM completion.
#[derive(Debug, Clone)]
pub enum StreamEvent {
    /// Text content delta.
    Delta(String),
    /// Raw provider event payload (for debugging API responses).
    ProviderRaw(serde_json::Value),
    /// Reasoning/planning text delta (not user-visible final answer text).
    ReasoningDelta(String),
    /// A tool call has started (content_block_start with tool_use).
    ToolCallStart {
        /// Tool call ID from the provider.
        id: String,
        /// Tool name being called.
        name: String,
        /// Index of this tool call in the response (0-based).
        index: usize,
        /// Provider-specific metadata (e.g. Gemini `thought_signature`).
        metadata: Option<serde_json::Map<String, serde_json::Value>>,
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

/// LLM provider trait (Anthropic, OpenAI, Google, etc.).
#[async_trait]
pub trait LlmProvider: Send + Sync {
    fn name(&self) -> &str;

    /// Model identifier (e.g. "claude-sonnet-4-20250514", "gpt-4o").
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

    /// Configured tool mode for this provider, if any.
    ///
    /// Returns `None` when the provider has no explicit tool mode override
    /// (the caller should fall back to `Auto` behavior based on `supports_tools()`).
    fn tool_mode(&self) -> Option<chelix_config::ToolMode> {
        None
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
        options: AgentToolControls,
    ) -> Pin<Box<dyn Stream<Item = StreamEvent> + Send + '_>> {
        if let Err(error) = reject_unsupported_tool_choice(self.name(), &options) {
            return Box::pin(tokio_stream::once(StreamEvent::Error(error.to_string())));
        }
        self.stream_with_tools(messages, tools)
    }

    /// Configured reasoning effort for this provider instance, if any.
    ///
    /// Providers that support extended thinking (Anthropic, OpenAI o-series)
    /// use this value when building API requests.
    fn reasoning_effort(&self) -> Option<ReasoningEffort> {
        None
    }

    /// Return a new provider with reasoning effort set, if supported.
    ///
    /// Returns `None` for providers that don't support reasoning effort.
    /// Used by sub-agent spawning to apply per-agent reasoning settings
    /// without mutating the shared registry provider.
    fn with_reasoning_effort(
        self: Arc<Self>,
        _effort: ReasoningEffort,
    ) -> Option<Arc<dyn LlmProvider>> {
        None
    }

    /// Send the cheapest request available that proves the model can answer.
    ///
    /// The default implementation streams a tiny prompt and returns as soon as
    /// the first text delta or terminal event arrives. Providers can override
    /// this to use provider-specific low-cost probe requests.
    async fn probe(&self) -> anyhow::Result<()> {
        let timeout = self.probe_timeout();
        let probe = vec![ChatMessage::user("ping")];
        let mut stream = self.stream(probe);

        let result = tokio::time::timeout(timeout, async {
            while let Some(event) = stream.next().await {
                match event {
                    StreamEvent::Delta(_) | StreamEvent::Done(_) => return Ok(()),
                    StreamEvent::Error(err) => return Err(anyhow::anyhow!(err)),
                    _ => continue,
                }
            }
            Err(anyhow::anyhow!("stream ended without producing any output"))
        })
        .await;

        drop(stream);

        match result {
            Ok(inner) => inner,
            Err(_) => Err(anyhow::anyhow!(
                "Connection timed out after {} seconds",
                timeout.as_secs()
            )),
        }
    }

    /// Timeout for the completion-based `probe()` fallback.
    ///
    /// Providers with slow model loading (e.g. local LLM servers) should
    /// override this with a longer duration. The default is 30 seconds.
    fn probe_timeout(&self) -> Duration {
        Duration::from_secs(30)
    }

    /// Check whether the provider is reachable and knows about this model.
    ///
    /// Unlike [`probe()`](Self::probe), this does **not** require the model to
    /// generate output. It uses lightweight endpoints such as `GET /v1/models`
    /// or `POST /api/show` to verify model availability without triggering
    /// model loading.
    ///
    /// The default implementation falls back to [`probe()`](Self::probe).
    /// Providers should override this with a catalog/listing check whenever
    /// the server supports one.
    async fn check_availability(&self) -> anyhow::Result<()> {
        self.probe().await
    }
}

fn reject_unsupported_tool_choice(
    provider_name: &str,
    options: &AgentToolControls,
) -> anyhow::Result<()> {
    if matches!(
        options.tool_choice,
        Some(ToolChoice::Tool { .. } | ToolChoice::Any)
    ) {
        anyhow::bail!("provider {provider_name} does not support forced tool_choice");
    }
    Ok(())
}
