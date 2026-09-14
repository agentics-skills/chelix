use std::pin::Pin;

use {
    futures::{SinkExt, StreamExt},
    tokio_stream::Stream,
    tokio_tungstenite::tungstenite::{Message, client::IntoClientRequest, http::HeaderValue},
};

use tracing::{debug, trace};

use crate::{
    openai_compat::{
        ResponsesEventResult, ResponsesStreamState, finalize_responses_stream,
        process_responses_event, responses_protocol_error, split_responses_instructions_and_input,
        to_responses_api_tools,
    },
    ws_pool,
};

use chelix_agents::model::{ChatMessage, CompletionOptions, StreamEvent};

use {super::OpenAiProvider, crate::openai::ResponsesWebSocketPolicy};

impl OpenAiProvider {
    pub(super) fn supports_responses_websocket(&self) -> bool {
        matches!(
            self.capabilities.responses_websocket_policy,
            ResponsesWebSocketPolicy::OpenAiPlatform
        )
    }

    pub(super) fn responses_websocket_url(&self) -> crate::error::Result<String> {
        let mut base = self.base_url.trim().trim_end_matches('/').to_string();
        if !base.ends_with("/v1") {
            base.push_str("/v1");
        }
        let url = format!("{base}/responses");
        if let Some(rest) = url.strip_prefix("https://") {
            return Ok(format!("wss://{rest}"));
        }
        if let Some(rest) = url.strip_prefix("http://") {
            return Ok(format!("ws://{rest}"));
        }
        Err(crate::error::Error::message(format!(
            "invalid OpenAI base_url for websocket mode: expected http:// or https://, got {}",
            self.base_url
        )))
    }

    #[allow(clippy::collapsible_if)]
    pub(super) fn stream_with_tools_websocket(
        &self,
        messages: Vec<ChatMessage>,
        tools: Vec<serde_json::Value>,
        fallback_to_sse: bool,
        options: CompletionOptions,
        fallback_to_responses_sse: bool,
    ) -> Pin<Box<dyn Stream<Item = StreamEvent> + Send + '_>> {
        // Synchronous pre-flight: URL, request, auth header, pool key.
        // Fail fast and fall back to SSE before entering the async generator,
        // which avoids cloning messages/tools for the four sync-check paths.
        let (request, pool_key) = match (|| -> crate::error::Result<_> {
            if !self.supports_responses_websocket() {
                return Err(crate::error::Error::message(format!(
                    "websocket mode is not supported for this provider (base_url: {})",
                    self.base_url
                )));
            }
            let ws_url = self.responses_websocket_url()?;
            let pk = ws_pool::PoolKey::new(&ws_url, &self.api_key);
            let mut req = ws_url.as_str().into_client_request()?;
            let auth = self.bearer_auth_header();
            req.headers_mut()
                .insert("Authorization", HeaderValue::from_str(&auth)?);
            req.headers_mut()
                .insert("OpenAI-Beta", HeaderValue::from_static("responses=v1"));
            Ok((req, pk))
        })() {
            Ok(r) => r,
            Err(err) => {
                if fallback_to_sse {
                    debug!(error = %err, "websocket setup failed, falling back to sse");
                    return if fallback_to_responses_sse {
                        self.stream_responses_sse(messages, tools, options)
                    } else {
                        self.stream_with_tools_sse(messages, tools, options)
                    };
                }
                return Box::pin(async_stream::stream! {
                    yield StreamEvent::Error(err.to_string());
                });
            },
        };

        Box::pin(async_stream::stream! {
            // Try the pool first; fall back to a fresh connection.
            let (mut ws_stream, created_at) = if let Some(pooled) = ws_pool::shared_ws_pool().checkout(&pool_key).await {
                pooled
            } else {
                match tokio_tungstenite::connect_async(request).await {
                    Ok((ws, _)) => (ws, std::time::Instant::now()),
                    Err(err) => {
                        if fallback_to_sse {
                            debug!(error = %err, "websocket connect failed, falling back to sse");
                            let mut sse = if fallback_to_responses_sse {
                                self.stream_responses_sse(messages, tools, options)
                            } else {
                                self.stream_with_tools_sse(messages, tools, options)
                            };
                            while let Some(event) = sse.next().await {
                                yield event;
                            }
                        } else {
                            yield StreamEvent::Error(err.to_string());
                        }
                        return;
                    }
                }
            };

            let (instructions, input) = split_responses_instructions_and_input(messages);
            let mut response_payload = serde_json::json!({
                "model": self.model,
                "stream": true,
                "store": false,
                "input": input,
            });
            if let Some(instructions) = instructions {
                response_payload["instructions"] = serde_json::Value::String(instructions);
            }
            if !tools.is_empty() {
                match to_responses_api_tools(&tools) {
                    Ok(prepared) => response_payload["tools"] = serde_json::Value::Array(prepared),
                    Err(error) => {
                        yield StreamEvent::Error(error.to_string());
                        return;
                    },
                }
            }
            if let Err(error) = super::core::apply_openai_responses_tool_choice(
                &mut response_payload,
                options.tool_choice.as_ref(),
            ) {
                yield StreamEvent::Error(error.to_string());
                return;
            }

            if let Some(max_output_tokens) = options.max_output_tokens {
                response_payload["max_output_tokens"] = serde_json::json!(max_output_tokens);
            }

            if let Err(error) = self.apply_reasoning_responses(&mut response_payload) {
                yield StreamEvent::Error(error.to_string());
                return;
            }

            let create_event = serde_json::json!({
                "type": "response.create",
                "response": response_payload,
            });

            debug!(
                model = %self.model,
                tools_count = tools.len(),
                reasoning_effort = ?self.selected_reasoning_effort(),
                "openai stream_with_tools request (websocket)"
            );
            trace!(event = %create_event, "openai websocket create event");

            if let Err(err) = ws_stream
                .send(Message::Text(create_event.to_string().into()))
                .await
            {
                yield StreamEvent::Error(format!("websocket send failed: {err}"));
                return;
            }

            let mut state = ResponsesStreamState::default();
            let mut clean_completion = false;

            while let Some(frame) = ws_stream.next().await {
                let text = match frame {
                    Ok(Message::Text(t)) => t.to_string(),
                    Ok(Message::Binary(b)) => match String::from_utf8(b.into()) {
                        Ok(text) => text,
                        Err(err) => {
                            for event in state.close_on_transport_error() {
                                yield event;
                            }
                            yield StreamEvent::Error(format!("websocket frame is not valid UTF-8: {err}"));
                            return;
                        },
                    },
                    Ok(Message::Ping(p)) => {
                        if let Err(err) = ws_stream.send(Message::Pong(p)).await {
                            for event in state.close_on_transport_error() {
                                yield event;
                            }
                            yield StreamEvent::Error(err.to_string());
                            return;
                        }
                        continue;
                    },
                    Ok(Message::Close(_)) => break,
                    Ok(_) => continue,
                    Err(err) => {
                        // The response was cut off. Close the segment so it is
                        // not replayed later as still in progress.
                        for event in state.close_on_transport_error() {
                            yield event;
                        }
                        yield StreamEvent::Error(err.to_string());
                        return;
                    },
                };

                let result = match serde_json::from_str::<serde_json::Value>(&text) {
                    Ok(event) => {
                        trace!(event = %event, "openai websocket event");
                        process_responses_event(event, &mut state)
                    },
                    Err(error) => responses_protocol_error(
                        &mut state,
                        Vec::new(),
                        format!("provider sent a non-JSON Responses websocket frame: {error}"),
                    ),
                };

                match result {
                    ResponsesEventResult::Completed(events) => {
                        for event in events {
                            yield event;
                        }
                        clean_completion = true;
                        break;
                    },
                    ResponsesEventResult::Failed(events) => {
                        for event in events {
                            yield event;
                        }
                        return;
                    },
                    ResponsesEventResult::Events(events) => {
                        for event in events {
                            yield event;
                        }
                    },
                }
            }

            // Return healthy connections to the pool; drop on error / close.
            if clean_completion {
                ws_pool::shared_ws_pool()
                    .return_conn(pool_key, ws_stream, created_at)
                    .await;
            }

            for event in finalize_responses_stream(&mut state) {
                yield event;
            }
        })
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use {
        axum::{
            Router,
            extract::ws::{Message, WebSocketUpgrade},
            response::IntoResponse,
            routing::get,
        },
        chelix_agents::model::{ChatMessage, LlmProvider, StreamEvent},
        chelix_config::schema::{ProviderStreamTransport, WireApi},
        futures::StreamExt,
        secrecy::Secret,
    };

    use super::super::{OpenAiProvider, core::tests::configure_reasoning};

    /// Stream one Responses request over a real websocket whose server answers
    /// the `response.create` event with `reply`.
    async fn websocket_events(reply: Message) -> Vec<StreamEvent> {
        let handler = move |ws: WebSocketUpgrade| {
            let reply = reply.clone();
            async move {
                ws.on_upgrade(move |mut socket| async move {
                    while let Some(Ok(Message::Text(text))) = socket.next().await {
                        let create: serde_json::Value = serde_json::from_str(&text).unwrap();
                        assert_eq!(create["type"], "response.create");
                        socket.send(reply.clone()).await.unwrap();
                    }
                })
                .into_response()
            }
        };
        let app = Router::new().route("/v1/responses", get(handler));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let provider = configure_reasoning(
            OpenAiProvider::new(
                Secret::new("test-key".into()),
                "test-model".into(),
                format!("http://{address}/v1"),
            )
            .with_wire_api(WireApi::Responses)
            .with_stream_transport(ProviderStreamTransport::Websocket),
            vec!["off".into()],
            "off".into(),
        );
        provider
            .stream(vec![ChatMessage::user("hello")])
            .collect()
            .await
    }

    #[tokio::test]
    async fn websocket_invalid_utf8_binary_frame_fails_the_stream() {
        let events = websocket_events(Message::Binary(vec![0xff, 0xfe].into())).await;

        assert!(matches!(
            events.as_slice(),
            [StreamEvent::Error(message)] if message.contains("not valid UTF-8")
        ));
    }

    #[tokio::test]
    async fn websocket_non_json_frame_fails_without_closing_an_unopened_segment() {
        let events = websocket_events(Message::Text("not json".into())).await;

        assert!(matches!(
            events.as_slice(),
            [StreamEvent::Error(message)] if message.contains("non-JSON Responses websocket frame")
        ));
    }
}
