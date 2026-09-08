use std::{collections::HashMap, pin::Pin};

use {
    anyhow::{Result, bail, ensure},
    chelix_common::{ProviderSegmentMaterializer, ProviderSegmentOutcome},
    futures::StreamExt,
    tokio_stream::Stream,
};

use super::{
    CompletionResponse, StreamEvent, ToolCall, decode_tool_call_arguments_from_str,
    push_capped_provider_raw_event,
};

struct PendingToolCall {
    call: ToolCall,
    arguments: String,
    canonical_arguments: Option<String>,
    complete: bool,
}

/// Collect a provider stream through its successful terminal event.
///
/// Canonical items are materialized with their provider identities and positions.
/// Raw event capture uses the same bound as the agent runner.
pub async fn collect_stream(
    mut stream: Pin<Box<dyn Stream<Item = StreamEvent> + Send + '_>>,
) -> Result<CompletionResponse> {
    let mut response = CompletionResponse::default();
    let mut text = String::new();
    let mut materializer = ProviderSegmentMaterializer::pending();
    let mut tool_calls: Vec<PendingToolCall> = Vec::new();
    let mut indices = HashMap::new();
    let mut failed_outcome = None;

    while let Some(event) = stream.next().await {
        match event {
            StreamEvent::SegmentStart { segment_id } => {
                if materializer.segment.segment_id.is_some() {
                    ensure!(
                        materializer.segment.outcome != ProviderSegmentOutcome::Active,
                        "provider opened a segment before closing the previous segment"
                    );
                    response.segments.push(materializer.segment);
                }
                materializer = ProviderSegmentMaterializer::new(segment_id);
                indices.clear();
            },
            StreamEvent::ProviderItemUpdate(update) => {
                materializer.apply_update(&update)?;
                if let chelix_common::ProviderItemUpdatePayload::FunctionCallDone { arguments } =
                    &update.payload
                {
                    for position in indices.values() {
                        let pending: &mut PendingToolCall = &mut tool_calls[*position];
                        if pending.call.id == update.item_id.as_str() {
                            pending.canonical_arguments = Some(arguments.clone());
                        }
                    }
                }
            },
            StreamEvent::SegmentClose {
                segment_id,
                outcome,
                ..
            } => {
                ensure!(
                    materializer.segment.segment_id.as_ref() == Some(&segment_id),
                    "provider closed a different segment"
                );
                ensure!(
                    outcome != ProviderSegmentOutcome::Active,
                    "provider closed an active segment"
                );
                materializer.close(outcome)?;
                if outcome != ProviderSegmentOutcome::Completed {
                    failed_outcome = Some(outcome);
                }
            },
            StreamEvent::Delta(delta) => text.push_str(&delta),
            StreamEvent::ProviderRaw(raw) => {
                push_capped_provider_raw_event(&mut response.raw_events, raw)
            },
            StreamEvent::ToolCallStart { id, name, index } => {
                ensure!(
                    !indices.contains_key(&index),
                    "provider repeated tool index {index}"
                );
                indices.insert(index, tool_calls.len());
                tool_calls.push(PendingToolCall {
                    call: ToolCall {
                        id,
                        name,
                        arguments: serde_json::Value::Null,
                        argument_diagnostic: None,
                    },
                    arguments: String::new(),
                    canonical_arguments: None,
                    complete: false,
                });
            },
            StreamEvent::ToolCallArgumentsDelta { index, delta } => {
                let position = indices.get(&index).ok_or_else(|| {
                    anyhow::anyhow!("provider sent arguments for unknown tool index {index}")
                })?;
                let pending = &mut tool_calls[*position];
                ensure!(
                    !pending.complete,
                    "provider sent arguments after tool index {index} completed"
                );
                pending.arguments.push_str(&delta);
            },
            StreamEvent::ToolCallComplete { index } => {
                let position = indices.get(&index).ok_or_else(|| {
                    anyhow::anyhow!("provider completed unknown tool index {index}")
                })?;
                tool_calls[*position].complete = true;
            },
            StreamEvent::Error(error) => bail!("{error}"),
            StreamEvent::Done(usage) => {
                if let Some(outcome) = failed_outcome {
                    bail!("provider segment ended with {outcome:?}");
                }
                if materializer.segment.segment_id.is_some() {
                    ensure!(
                        materializer.segment.outcome == ProviderSegmentOutcome::Completed,
                        "provider stream finished with an unclosed segment"
                    );
                    response.segments.push(materializer.segment);
                }
                for pending in tool_calls {
                    ensure!(
                        pending.complete,
                        "provider stream finished with incomplete tool arguments"
                    );
                    let arguments = pending
                        .canonical_arguments
                        .as_deref()
                        .unwrap_or(&pending.arguments);
                    let decoded = decode_tool_call_arguments_from_str(arguments);
                    response.tool_calls.push(ToolCall {
                        arguments: decoded.arguments,
                        argument_diagnostic: decoded.diagnostic,
                        ..pending.call
                    });
                }
                response.text = (!text.is_empty()).then_some(text);
                response.usage = usage;
                return Ok(response);
            },
        }
    }

    if let Some(outcome) = failed_outcome {
        bail!("provider segment ended with {outcome:?}");
    }
    bail!("provider stream closed before Done")
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use {
        super::*,
        crate::model::Usage,
        chelix_common::{
            ProviderItemId, ProviderItemPosition, ProviderItemUpdate, ProviderItemUpdatePayload,
            ProviderSegmentId,
        },
    };

    async fn collect(events: Vec<StreamEvent>) -> Result<CompletionResponse> {
        collect_stream(Box::pin(tokio_stream::iter(events))).await
    }

    struct TextProvider;

    impl crate::model::LlmProvider for TextProvider {
        fn name(&self) -> &str {
            "text-provider"
        }

        fn id(&self) -> &str {
            "text-model"
        }

        fn stream(
            &self,
            _messages: Vec<crate::model::ChatMessage>,
        ) -> Pin<Box<dyn Stream<Item = StreamEvent> + Send + '_>> {
            Box::pin(tokio_stream::iter(vec![
                StreamEvent::Delta("ok".into()),
                StreamEvent::Done(Usage::default()),
            ]))
        }
    }

    #[tokio::test]
    async fn default_provider_rejects_unsupported_request_controls() {
        use crate::model::{CompletionOptions, LlmProvider, ToolChoice};
        let provider = TextProvider;
        for options in [
            CompletionOptions::with_max_output_tokens(100),
            CompletionOptions {
                tool_choice: Some(ToolChoice::Any),
                ..Default::default()
            },
            CompletionOptions {
                tool_choice: Some(ToolChoice::Tool {
                    name: "probe".into(),
                }),
                ..Default::default()
            },
        ] {
            assert!(
                collect_stream(provider.stream_with_tools_and_options(
                    Vec::new(),
                    Vec::new(),
                    options
                ))
                .await
                .is_err()
            );
        }
        let tools = vec![serde_json::json!({"name":"probe"})];
        assert!(
            collect_stream(provider.stream_with_tools(Vec::new(), tools))
                .await
                .is_err()
        );
        assert_eq!(
            collect_stream(provider.stream(Vec::new()))
                .await
                .unwrap()
                .text
                .as_deref(),
            Some("ok")
        );
    }

    #[tokio::test]
    async fn collects_text_tools_and_terminal_usage() {
        let usage = Usage {
            input_tokens: 20,
            output_tokens: 10,
            cache_read_tokens: 3,
            cache_write_tokens: 2,
        };
        let response = collect(vec![
            StreamEvent::Delta("hel".into()),
            StreamEvent::ToolCallStart {
                id: "call_1".into(),
                name: "probe".into(),
                index: 4,
            },
            StreamEvent::ToolCallArgumentsDelta {
                index: 4,
                delta: "{\"offset\":0,\"enabled\":".into(),
            },
            StreamEvent::Delta("lo".into()),
            StreamEvent::ToolCallArgumentsDelta {
                index: 4,
                delta: "false,\"value\":null}".into(),
            },
            StreamEvent::ToolCallComplete { index: 4 },
            StreamEvent::Done(usage.clone()),
        ])
        .await
        .unwrap();
        assert_eq!(response.text.as_deref(), Some("hello"));
        assert_eq!(response.usage, usage);
        assert_eq!(response.tool_calls.len(), 1);
        assert_eq!(response.tool_calls[0].id, "call_1");
        assert_eq!(response.tool_calls[0].name, "probe");
        assert_eq!(
            response.tool_calls[0].arguments,
            serde_json::json!({"offset":0,"enabled":false,"value":null})
        );
    }

    #[tokio::test]
    async fn preserves_canonical_segments_and_raw_events() {
        let mut events = Vec::new();
        for id in ["seg_a", "seg_b"] {
            let segment_id = ProviderSegmentId::new(id);
            events.extend([
                StreamEvent::SegmentStart {
                    segment_id: segment_id.clone(),
                },
                StreamEvent::ProviderItemUpdate(ProviderItemUpdate {
                    segment_id: segment_id.clone(),
                    item_id: ProviderItemId::new("reasoning"),
                    position: ProviderItemPosition::new(2),
                    update_seq: 1,
                    payload: ProviderItemUpdatePayload::ReasoningTextDelta { delta: id.into() },
                }),
                StreamEvent::ProviderRaw(serde_json::json!({"id":id})),
                StreamEvent::SegmentClose {
                    segment_id,
                    outcome: ProviderSegmentOutcome::Completed,
                    usage: None,
                },
            ]);
        }
        events.push(StreamEvent::Done(Usage::default()));
        let response = collect(events).await.unwrap();
        assert_eq!(response.segments.len(), 2);
        assert_eq!(response.raw_events, vec![
            serde_json::json!({"id":"seg_a"}),
            serde_json::json!({"id":"seg_b"})
        ]);
        for (segment, id) in response.segments.iter().zip(["seg_a", "seg_b"]) {
            assert_eq!(segment.segment_id.as_ref().unwrap().as_str(), id);
            assert_eq!(segment.outcome, ProviderSegmentOutcome::Completed);
            assert_eq!(segment.items[0].id.as_str(), "reasoning");
            assert_eq!(segment.items[0].position.as_usize(), 2);
            assert_eq!(
                segment.reasoning_content(),
                Some(chelix_common::ReasoningContent::Text(id.into()))
            );
        }
    }

    #[tokio::test]
    async fn propagates_errors_and_rejects_unfinished_streams() {
        for events in [
            vec![StreamEvent::Delta("partial".into())],
            vec![
                StreamEvent::ToolCallStart {
                    id: "call".into(),
                    name: "probe".into(),
                    index: 0,
                },
                StreamEvent::Done(Usage::default()),
            ],
            vec![
                StreamEvent::ToolCallArgumentsDelta {
                    index: 8,
                    delta: "{}".into(),
                },
                StreamEvent::Done(Usage::default()),
            ],
        ] {
            assert!(collect(events).await.is_err());
        }
        let error = collect(vec![
            StreamEvent::Delta("partial".into()),
            StreamEvent::Error("HTTP 429: rate limited".into()),
            StreamEvent::Done(Usage::default()),
        ])
        .await
        .unwrap_err();
        assert_eq!(error.to_string(), "HTTP 429: rate limited");
        for outcome in [
            ProviderSegmentOutcome::Incomplete,
            ProviderSegmentOutcome::Failed,
            ProviderSegmentOutcome::Cancelled,
            ProviderSegmentOutcome::TransportError,
        ] {
            let id = ProviderSegmentId::new("seg");
            assert!(
                collect(vec![
                    StreamEvent::SegmentStart {
                        segment_id: id.clone()
                    },
                    StreamEvent::Delta("partial".into()),
                    StreamEvent::SegmentClose {
                        segment_id: id,
                        outcome,
                        usage: None
                    },
                    StreamEvent::Done(Usage::default()),
                ])
                .await
                .is_err()
            );
        }
    }
}
