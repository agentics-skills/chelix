//! Contract tests for the [`LlmProvider`] trait.
//!
//! These functions validate that any `LlmProvider` implementation satisfies
//! the streaming semantics required by the chat runtime.
//! Run against `MockLlmProvider` in provider tests.

#![allow(clippy::unwrap_used)]

use std::pin::Pin;

use {
    chelix_agents::model::{ChatMessage, LlmProvider, StreamEvent, Usage, collect_stream},
    chelix_common::{
        ProviderItemId, ProviderItemPosition, ProviderItemUpdate, ProviderItemUpdatePayload,
        ProviderSegmentId,
    },
    tokio_stream::{Stream, StreamExt},
};

// ── Mock provider ───────────────────────────────────────────────────────────

/// A mock LLM provider that returns canned responses for contract testing.
pub struct MockLlmProvider {
    /// If set, `stream()` returns this error.
    pub stream_error: Option<MockError>,
}

/// Simulated error types for the mock provider.
pub enum MockError {
    /// Simulates a 429 rate-limit error (retryable).
    RateLimit,
    /// Simulates a 401 auth error (fatal).
    AuthFailed,
}

impl MockLlmProvider {
    pub fn ok() -> Self {
        Self { stream_error: None }
    }

    pub fn with_error(error: MockError) -> Self {
        Self {
            stream_error: Some(error),
        }
    }
}

impl LlmProvider for MockLlmProvider {
    fn name(&self) -> &str {
        "mock"
    }

    fn id(&self) -> &str {
        "mock-model"
    }

    fn stream(
        &self,
        _messages: Vec<ChatMessage>,
    ) -> Pin<Box<dyn Stream<Item = StreamEvent> + Send + '_>> {
        if let Some(error) = &self.stream_error {
            let message = match error {
                MockError::RateLimit => "429 Too Many Requests: rate limited",
                MockError::AuthFailed => "401 Unauthorized: invalid API key",
            };
            return Box::pin(tokio_stream::once(StreamEvent::Error(message.to_string())));
        }
        let events = vec![
            StreamEvent::Delta("Hello ".into()),
            StreamEvent::Delta("world".into()),
            StreamEvent::Done(Usage {
                input_tokens: 10,
                output_tokens: 5,
                cache_read_tokens: 0,
                cache_write_tokens: 0,
            }),
        ];
        Box::pin(tokio_stream::iter(events))
    }
}

// ── Contract tests ──────────────────────────────────────────────────────────

/// A collected stream must return a response with text and usage data.
pub async fn collected_stream_returns_complete_response(
    provider: &dyn LlmProvider,
) -> anyhow::Result<()> {
    let messages = vec![ChatMessage::user("Say hello.")];
    let response = collect_stream(provider.stream(messages)).await?;

    assert!(
        response.text.is_some(),
        "completion response must contain text"
    );
    assert!(
        response.usage.input_tokens > 0 || response.usage.output_tokens > 0,
        "completion response must include usage data"
    );
    Ok(())
}

/// A streaming completion must eventually emit a Done event.
pub async fn stream_emits_done_signal(provider: &dyn LlmProvider) -> anyhow::Result<()> {
    let messages = vec![ChatMessage::user("Say hello.")];
    let mut stream = provider.stream(messages);

    let mut saw_done = false;
    while let Some(event) = stream.next().await {
        if matches!(event, StreamEvent::Done(_)) {
            saw_done = true;
            break;
        }
    }

    assert!(saw_done, "stream must emit a Done event");
    Ok(())
}

/// A streaming completion may surface reasoning separately from visible text.
///
/// Providers that support explicit reasoning channels should emit at least one
/// `ReasoningDelta`, then at least one visible `Delta`, and still finish with
/// `Done`.
pub async fn stream_surfaces_reasoning_separately(
    provider: &dyn LlmProvider,
) -> anyhow::Result<()> {
    let messages = vec![ChatMessage::user("Think, then answer.")];
    let mut stream = provider.stream(messages);

    let mut first_reasoning_index: Option<usize> = None;
    let mut first_visible_index: Option<usize> = None;
    let mut saw_done = false;
    let mut event_index = 0usize;

    while let Some(event) = stream.next().await {
        match event {
            StreamEvent::ProviderItemUpdate(ref update) => {
                if matches!(
                    &update.payload,
                    ProviderItemUpdatePayload::ReasoningDelta { .. }
                        | ProviderItemUpdatePayload::ReasoningPartDone { .. }
                        | ProviderItemUpdatePayload::ReasoningText { .. }
                        | ProviderItemUpdatePayload::ReasoningTextDelta { .. }
                ) {
                    first_reasoning_index.get_or_insert(event_index);
                }
            },
            StreamEvent::Delta(_) => {
                first_visible_index.get_or_insert(event_index);
            },
            StreamEvent::Done(_) => {
                saw_done = true;
                break;
            },
            _ => {},
        }
        event_index += 1;
    }

    let first_reasoning_index = first_reasoning_index
        .ok_or_else(|| anyhow::anyhow!("stream should emit at least one ReasoningDelta"))?;
    let first_visible_index = first_visible_index
        .ok_or_else(|| anyhow::anyhow!("stream should emit at least one visible Delta"))?;
    assert!(
        first_reasoning_index < first_visible_index,
        "reasoning should arrive before visible text"
    );
    assert!(saw_done, "stream must emit a Done event");
    Ok(())
}

/// A 429 rate-limit error message must contain "429" (retryable indicator).
pub async fn error_classification_maps_429_to_retryable(provider: &dyn LlmProvider) {
    let messages = vec![ChatMessage::user("test")];
    let result = collect_stream(provider.stream(messages)).await;
    assert!(result.is_err(), "rate-limited provider must return error");
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("429"),
        "rate-limit error must contain '429', got: {err}"
    );
}

/// A 401 auth error message must contain "401" (fatal indicator).
pub async fn error_classification_maps_401_to_fatal(provider: &dyn LlmProvider) {
    let messages = vec![ChatMessage::user("test")];
    let result = collect_stream(provider.stream(messages)).await;
    assert!(result.is_err(), "auth-failed provider must return error");
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("401"),
        "auth error must contain '401', got: {err}"
    );
}

#[cfg(test)]
mod tests {
    use {super::*, std::pin::Pin};

    struct ReasoningProvider;

    impl LlmProvider for ReasoningProvider {
        fn name(&self) -> &str {
            "reasoning-mock"
        }

        fn id(&self) -> &str {
            "reasoning-model"
        }

        fn stream(
            &self,
            _messages: Vec<ChatMessage>,
        ) -> Pin<Box<dyn Stream<Item = StreamEvent> + Send + '_>> {
            Box::pin(tokio_stream::iter(vec![
                StreamEvent::ProviderItemUpdate(ProviderItemUpdate {
                    segment_id: ProviderSegmentId::new("seg_mock"),
                    item_id: ProviderItemId::new("rs_0"),
                    position: ProviderItemPosition::new(0),
                    update_seq: 1,
                    payload: ProviderItemUpdatePayload::ReasoningTextDelta {
                        delta: "step 1".into(),
                    },
                }),
                StreamEvent::ProviderItemUpdate(ProviderItemUpdate {
                    segment_id: ProviderSegmentId::new("seg_mock"),
                    item_id: ProviderItemId::new("rs_0"),
                    position: ProviderItemPosition::new(0),
                    update_seq: 2,
                    payload: ProviderItemUpdatePayload::ReasoningTextDelta {
                        delta: " step 2".into(),
                    },
                }),
                StreamEvent::Delta("answer".into()),
                StreamEvent::Done(Usage::default()),
            ]))
        }
    }

    #[tokio::test]
    async fn contract_collected_stream_returns_complete_response() {
        let provider = MockLlmProvider::ok();
        collected_stream_returns_complete_response(&provider)
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn contract_stream_emits_done_signal() {
        let provider = MockLlmProvider::ok();
        stream_emits_done_signal(&provider).await.unwrap();
    }

    #[tokio::test]
    async fn contract_stream_surfaces_reasoning_separately() {
        let provider = ReasoningProvider;
        stream_surfaces_reasoning_separately(&provider)
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn contract_error_classification_429() {
        let provider = MockLlmProvider::with_error(MockError::RateLimit);
        error_classification_maps_429_to_retryable(&provider).await;
    }

    #[tokio::test]
    async fn contract_error_classification_401() {
        let provider = MockLlmProvider::with_error(MockError::AuthFailed);
        error_classification_maps_401_to_fatal(&provider).await;
    }
}
