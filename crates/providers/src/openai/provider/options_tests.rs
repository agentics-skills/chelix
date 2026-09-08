#![allow(clippy::unwrap_used, clippy::expect_used)]

use {
    axum::{
        Json, Router,
        body::Body,
        extract::{
            State,
            ws::{Message, WebSocketUpgrade},
        },
        http::{Uri, header::CONTENT_TYPE},
        response::{IntoResponse, Response},
        routing::{get, post},
    },
    chelix_agents::model::{
        ChatMessage, CompletionOptions, LlmProvider, ToolChoice, collect_stream,
    },
    chelix_config::schema::{ProviderStreamTransport, WireApi},
    futures::StreamExt,
    secrecy::Secret,
    tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel},
};

use super::{OpenAiProvider, core::tests::configure_reasoning};

type CapturedRequest = (String, serde_json::Value);

fn completed_response() -> serde_json::Value {
    serde_json::json!({
        "type": "response.completed",
        "response": {"id":"resp_options", "usage": {"input_tokens":12,"output_tokens":7,"input_tokens_details":{"cached_tokens":3}}}
    })
}

async fn capture_sse(
    State(sender): State<UnboundedSender<CapturedRequest>>,
    uri: Uri,
    Json(body): Json<serde_json::Value>,
) -> Response {
    sender.send((uri.path().into(), body)).unwrap();
    let data = if uri.path().ends_with("/responses") {
        format!("data: {}\n\n", completed_response())
    } else {
        let chunk = serde_json::json!({
            "id":"chat_options", "choices":[{"delta":{"content":"ok"},"finish_reason":"stop"}],
            "usage":{"prompt_tokens":12,"completion_tokens":7,"prompt_tokens_details":{"cached_tokens":3}}
        });
        format!("data: {chunk}\n\ndata: [DONE]\n\n")
    };
    Response::builder()
        .header(CONTENT_TYPE, "text/event-stream")
        .body(Body::from(data))
        .unwrap()
}

async fn capture_websocket(
    State(sender): State<UnboundedSender<CapturedRequest>>,
    ws: WebSocketUpgrade,
) -> impl IntoResponse {
    ws.on_upgrade(move |mut socket| async move {
        while let Some(Ok(Message::Text(text))) = socket.next().await {
            let create: serde_json::Value = serde_json::from_str(&text).unwrap();
            assert_eq!(create["type"], "response.create");
            sender
                .send(("websocket".into(), create["response"].clone()))
                .unwrap();
            socket
                .send(Message::Text(completed_response().to_string().into()))
                .await
                .unwrap();
        }
    })
}

async fn capture_server(websocket: bool) -> (String, UnboundedReceiver<CapturedRequest>) {
    let (sender, receiver) = unbounded_channel();
    let responses = if websocket {
        get(capture_websocket).post(capture_sse)
    } else {
        post(capture_sse)
    };
    let app = Router::new()
        .route("/v1/chat/completions", post(capture_sse))
        .route("/v1/responses", responses)
        .with_state(sender);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (format!("http://{address}/v1"), receiver)
}

#[tokio::test]
async fn streaming_options_follow_the_actual_wire_format() {
    for wire_api in [WireApi::ChatCompletions, WireApi::Responses] {
        for (transport, ws_capable, accept_ws) in [
            (ProviderStreamTransport::Sse, true, true),
            (ProviderStreamTransport::Websocket, true, true),
            (ProviderStreamTransport::Auto, true, true),
            (ProviderStreamTransport::Auto, false, true),
            (ProviderStreamTransport::Auto, true, false),
        ] {
            let (base_url, mut captured) = capture_server(accept_ws).await;
            let provider = if ws_capable {
                OpenAiProvider::new(
                    Secret::new("test-key".into()),
                    "test-model".into(),
                    base_url,
                )
            } else {
                OpenAiProvider::new_with_name(
                    Secret::new("test-key".into()),
                    "test-model".into(),
                    base_url,
                    "test-provider".into(),
                )
            };
            let provider = configure_reasoning(
                provider
                    .with_wire_api(wire_api)
                    .with_stream_transport(transport),
                vec!["off".into()],
                "off".into(),
            );
            for tools in [Vec::new(), vec![serde_json::json!({
                "name":"execute_command", "description":"Execute a command",
                "parameters":{"type":"object","properties":{"command":{"type":"string"},"terminalId":{"type":"string"}},"required":["command"]}
            })]] {
                let mut ordinary = None;
                for limit in [None, Some(12_800)] {
                    let options = CompletionOptions {
                        tool_choice: (!tools.is_empty()).then(|| ToolChoice::Tool {
                            name: "execute_command".into(),
                        }),
                        max_output_tokens: limit,
                    };
                    let result = collect_stream(provider.stream_with_tools_and_options(
                        vec![ChatMessage::user("hello")],
                        tools.clone(),
                        options,
                    ))
                    .await
                    .unwrap();
                    assert_eq!(result.usage.input_tokens, 12);
                    assert_eq!(result.usage.output_tokens, 7);
                    assert_eq!(result.usage.cache_read_tokens, 3);
                    let (path, mut body) = captured.recv().await.unwrap();
                    let is_ws =
                        ws_capable && accept_ws && transport != ProviderStreamTransport::Sse;
                    let responses_format = is_ws || wire_api == WireApi::Responses;
                    assert_eq!(
                        path,
                        if is_ws {
                            "websocket"
                        } else if responses_format {
                            "/v1/responses"
                        } else {
                            "/v1/chat/completions"
                        }
                    );
                    assert_eq!(body["stream"], true);
                    assert_eq!(body["model"], "test-model");
                    let (limit_key, other_key) = if responses_format {
                        ("max_output_tokens", "max_completion_tokens")
                    } else {
                        ("max_completion_tokens", "max_output_tokens")
                    };
                    assert!(body.get(other_key).is_none());
                    if let Some(limit) = limit {
                        assert_eq!(body[limit_key], limit);
                        body.as_object_mut().unwrap().remove(limit_key);
                        assert_eq!(Some(body), ordinary);
                        continue;
                    }
                    assert!(body.get(limit_key).is_none());
                    if responses_format {
                        assert_eq!(body["store"], false);
                    } else {
                        assert_eq!(body["stream_options"]["include_usage"], true);
                    }
                    if !tools.is_empty() {
                        let function = if responses_format {
                            &body["tools"][0]
                        } else {
                            &body["tools"][0]["function"]
                        };
                        assert_eq!(function["strict"], false);
                        assert_eq!(
                            function["parameters"]["required"],
                            serde_json::json!(["command"])
                        );
                        assert_eq!(
                            function["parameters"]["properties"]["terminalId"]["type"],
                            "string"
                        );
                        assert_eq!(
                            body["tool_choice"],
                            if responses_format {
                                serde_json::json!({"type":"function","name":"execute_command"})
                            } else {
                                serde_json::json!({"type":"function","function":{"name":"execute_command"}})
                            }
                        );
                    }
                    ordinary = Some(body);
                }
            }
        }
    }
}
