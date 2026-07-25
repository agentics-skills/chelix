use std::sync::Arc;

use {
    axum::extract::ws::{Message, WebSocket},
    base64::Engine as _,
    chelix_protocol::{
        ToolsServiceTerminalClientMessage, ToolsServiceTerminalControlAction,
        ToolsServiceTerminalInfo,
    },
    futures::{SinkExt, StreamExt},
};

use crate::terminal::TerminalManager;

const MAX_INPUT_BYTES: usize = 8 * 1024;

pub(crate) async fn handle(
    socket: WebSocket,
    manager: Arc<TerminalManager>,
    terminal: ToolsServiceTerminalInfo,
) {
    let (mut ws_tx, mut ws_rx) = socket.split();
    let mut subscription = match manager
        .subscribe_terminal(&terminal.session_key, &terminal.id)
        .await
    {
        Ok(subscription) => subscription,
        Err(error) => {
            let _ = send_status(&mut ws_tx, error, "error").await;
            return;
        },
    };

    if !send_json(
        &mut ws_tx,
        serde_json::json!({
            "type": "ready",
            "available": true,
            "mode": "tools_service",
            "terminal": terminal,
        }),
    )
    .await
    {
        return;
    }
    if !subscription.initial_output.is_empty()
        && !send_output(&mut ws_tx, &subscription.initial_output).await
    {
        return;
    }
    loop {
        tokio::select! {
            output = subscription.next_output() => {
                match output {
                    Ok(Some(data)) => {
                        if !send_output(&mut ws_tx, &data).await {
                            break;
                        }
                    },
                    Ok(None) => {
                        let _ = send_status(&mut ws_tx, "terminal stream closed", "error").await;
                        break;
                    },
                    Err(error) => {
                        let _ = send_status(&mut ws_tx, error, "error").await;
                        break;
                    },
                }
            },
            message = ws_rx.next() => {
                let Some(Ok(message)) = message else {
                    break;
                };
                match message {
                    Message::Text(text) => {
                        let parsed = serde_json::from_str::<ToolsServiceTerminalClientMessage>(&text);
                        let result = match parsed {
                            Ok(ToolsServiceTerminalClientMessage::Input { data }) => {
                                if data.len() > MAX_INPUT_BYTES {
                                    Err(anyhow::anyhow!(
                                        "input chunk too large (max {MAX_INPUT_BYTES} bytes)"
                                    ))
                                } else {
                                    manager
                                        .write_terminal(
                                            &terminal.session_key,
                                            &terminal.id,
                                            data.as_bytes(),
                                        )
                                        .await
                                }
                            },
                            Ok(ToolsServiceTerminalClientMessage::Resize { cols, rows }) => {
                                manager
                                    .resize_terminal(&terminal.session_key, &terminal.id, cols, rows)
                                    .await
                            },
                            Ok(ToolsServiceTerminalClientMessage::Control { action }) => {
                                match action {
                                    ToolsServiceTerminalControlAction::CtrlC => {
                                        manager
                                            .send_terminal_keys(
                                                &terminal.session_key,
                                                &terminal.id,
                                                "C-c",
                                            )
                                            .await
                                    },
                                    ToolsServiceTerminalControlAction::Clear => {
                                        manager
                                            .send_terminal_keys(
                                                &terminal.session_key,
                                                &terminal.id,
                                                "C-l",
                                            )
                                            .await
                                    },
                                }
                            },
                            Ok(ToolsServiceTerminalClientMessage::Ping) => {
                                if !send_json(&mut ws_tx, serde_json::json!({ "type": "pong" })).await {
                                    break;
                                }
                                Ok(())
                            },
                            Err(error) => Err(anyhow::anyhow!("invalid terminal message: {error}")),
                        };
                        if let Err(error) = result
                            && !send_status(&mut ws_tx, error, "error").await
                        {
                            break;
                        }
                    },
                    Message::Ping(payload) => {
                        if ws_tx.send(Message::Pong(payload)).await.is_err() {
                            break;
                        }
                    },
                    Message::Close(_) => break,
                    Message::Binary(_) | Message::Pong(_) => {},
                }
            },
        }
    }
}

async fn send_json(
    sink: &mut futures::stream::SplitSink<WebSocket, Message>,
    payload: serde_json::Value,
) -> bool {
    match serde_json::to_string(&payload) {
        Ok(text) => sink.send(Message::Text(text.into())).await.is_ok(),
        Err(error) => {
            tracing::error!(%error, "serializing managed terminal websocket message failed");
            false
        },
    }
}

async fn send_status(
    sink: &mut futures::stream::SplitSink<WebSocket, Message>,
    text: impl std::fmt::Display,
    level: &str,
) -> bool {
    send_json(
        sink,
        serde_json::json!({
            "type": "status",
            "text": text.to_string(),
            "level": level,
        }),
    )
    .await
}

async fn send_output(
    sink: &mut futures::stream::SplitSink<WebSocket, Message>,
    data: &[u8],
) -> bool {
    send_json(
        sink,
        serde_json::json!({
            "type": "output",
            "encoding": "base64",
            "data": base64::engine::general_purpose::STANDARD.encode(data),
        }),
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn terminal_control_messages_round_trip() {
        let message = ToolsServiceTerminalClientMessage::Control {
            action: ToolsServiceTerminalControlAction::CtrlC,
        };
        let json = serde_json::to_string(&message)
            .unwrap_or_else(|error| panic!("serialize failed: {error}"));
        let decoded = serde_json::from_str::<ToolsServiceTerminalClientMessage>(&json)
            .unwrap_or_else(|error| panic!("decode failed: {error}"));
        assert_eq!(decoded, message);
    }
}
