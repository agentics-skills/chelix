use std::io::{Read, Write};

use {
    anyhow::{Context, Result},
    axum::extract::ws::{Message, WebSocket},
    base64::Engine as _,
    chelix_protocol::{
        ToolsServiceTerminalClientMessage, ToolsServiceTerminalControlAction,
        ToolsServiceTerminalInfo,
    },
    futures::{SinkExt, StreamExt},
    portable_pty::{Child, MasterPty, PtySize, native_pty_system},
};

use crate::tmux::TmuxRuntime;

const DEFAULT_COLS: u16 = 220;
const DEFAULT_ROWS: u16 = 56;
const MAX_INPUT_BYTES: usize = 8 * 1024;

enum TerminalOutputEvent {
    Output(Vec<u8>),
    Error(String),
    Closed,
}

struct TerminalRuntime {
    master: Box<dyn MasterPty + Send>,
    writer: Box<dyn Write + Send>,
    child: Box<dyn Child + Send + Sync>,
    output_rx: tokio::sync::mpsc::UnboundedReceiver<TerminalOutputEvent>,
}

pub(crate) async fn handle(
    socket: WebSocket,
    runtime: std::sync::Arc<TmuxRuntime>,
    terminal: ToolsServiceTerminalInfo,
) {
    let (mut ws_tx, mut ws_rx) = socket.split();
    let mut cols = DEFAULT_COLS;
    let mut rows = DEFAULT_ROWS;
    let mut attached = match spawn_runtime(&runtime, &terminal, cols, rows) {
        Ok(attached) => attached,
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
        stop_runtime(&mut attached);
        return;
    }

    loop {
        tokio::select! {
            output = attached.output_rx.recv() => {
                match output {
                    Some(TerminalOutputEvent::Output(data)) => {
                        if !send_output(&mut ws_tx, &data).await {
                            break;
                        }
                    },
                    Some(TerminalOutputEvent::Error(error)) => {
                        if !send_status(&mut ws_tx, error, "error").await {
                            break;
                        }
                    },
                    Some(TerminalOutputEvent::Closed) | None => {
                        let _ = send_status(&mut ws_tx, "terminal attachment closed", "error").await;
                        break;
                    },
                }
            },
            message = ws_rx.next() => {
                let Some(message) = message else {
                    break;
                };
                let Ok(message) = message else {
                    break;
                };
                match message {
                    Message::Text(text) => {
                        let parsed = serde_json::from_str::<ToolsServiceTerminalClientMessage>(&text);
                        match parsed {
                            Ok(ToolsServiceTerminalClientMessage::Input { data }) => {
                                if data.len() > MAX_INPUT_BYTES {
                                    if !send_status(
                                        &mut ws_tx,
                                        format!("input chunk too large (max {MAX_INPUT_BYTES} bytes)"),
                                        "error",
                                    )
                                    .await
                                    {
                                        break;
                                    }
                                    continue;
                                }
                                if let Err(error) = write_input(&mut attached, data.as_bytes())
                                    && !send_status(&mut ws_tx, error, "error").await
                                {
                                    break;
                                }
                            },
                            Ok(ToolsServiceTerminalClientMessage::Resize {
                                cols: next_cols,
                                rows: next_rows,
                            }) => {
                                if next_cols < 2 || next_rows < 1 {
                                    continue;
                                }
                                match resize(&attached, next_cols, next_rows) {
                                    Ok(()) => {
                                        cols = next_cols;
                                        rows = next_rows;
                                    },
                                    Err(error) => {
                                        if !send_status(&mut ws_tx, error, "error").await {
                                            break;
                                        }
                                    },
                                }
                            },
                            Ok(ToolsServiceTerminalClientMessage::Control { action }) => {
                                let result = match action {
                                    ToolsServiceTerminalControlAction::Restart => {
                                        stop_runtime(&mut attached);
                                        spawn_runtime(&runtime, &terminal, cols, rows)
                                            .map(|next| attached = next)
                                    },
                                    ToolsServiceTerminalControlAction::CtrlC => {
                                        write_input(&mut attached, b"\x03")
                                    },
                                    ToolsServiceTerminalControlAction::Clear => {
                                        write_input(&mut attached, b"\x0c")
                                    },
                                };
                                if let Err(error) = result
                                    && !send_status(&mut ws_tx, error, "error").await
                                {
                                    break;
                                }
                            },
                            Ok(ToolsServiceTerminalClientMessage::Ping) => {
                                if !send_json(&mut ws_tx, serde_json::json!({ "type": "pong" })).await {
                                    break;
                                }
                            },
                            Err(error) => {
                                if !send_status(
                                    &mut ws_tx,
                                    format!("invalid terminal message: {error}"),
                                    "error",
                                )
                                .await
                                {
                                    break;
                                }
                            },
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

    stop_runtime(&mut attached);
}

fn spawn_runtime(
    runtime: &TmuxRuntime,
    terminal: &ToolsServiceTerminalInfo,
    cols: u16,
    rows: u16,
) -> Result<TerminalRuntime> {
    let pair = native_pty_system()
        .openpty(PtySize {
            rows: rows.max(1),
            cols: cols.max(2),
            pixel_width: 0,
            pixel_height: 0,
        })
        .context("allocating managed terminal PTY")?;
    let command = runtime.attach_command(
        &terminal.session_name,
        &terminal.window_id,
        &terminal.pane_id,
    );
    let child = pair
        .slave
        .spawn_command(command)
        .context("attaching to managed tmux terminal")?;
    drop(pair.slave);
    let writer = pair
        .master
        .take_writer()
        .context("opening managed terminal writer")?;
    let reader = pair
        .master
        .try_clone_reader()
        .context("opening managed terminal reader")?;
    let output_rx = spawn_reader(reader)?;
    Ok(TerminalRuntime {
        master: pair.master,
        writer,
        child,
        output_rx,
    })
}

fn spawn_reader(
    mut reader: Box<dyn Read + Send>,
) -> Result<tokio::sync::mpsc::UnboundedReceiver<TerminalOutputEvent>> {
    let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
    std::thread::Builder::new()
        .name("chelix-tools-terminal-reader".into())
        .spawn(move || {
            let mut buffer = vec![0_u8; 16 * 1024];
            loop {
                match reader.read(&mut buffer) {
                    Ok(0) => {
                        let _ = sender.send(TerminalOutputEvent::Closed);
                        return;
                    },
                    Ok(count) => {
                        if sender
                            .send(TerminalOutputEvent::Output(buffer[..count].to_vec()))
                            .is_err()
                        {
                            return;
                        }
                    },
                    Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {},
                    Err(error) => {
                        let _ = sender.send(TerminalOutputEvent::Error(format!(
                            "managed terminal stream failed: {error}"
                        )));
                        let _ = sender.send(TerminalOutputEvent::Closed);
                        return;
                    },
                }
            }
        })
        .context("starting managed terminal reader")?;
    Ok(receiver)
}

fn write_input(runtime: &mut TerminalRuntime, input: &[u8]) -> Result<()> {
    runtime
        .writer
        .write_all(input)
        .context("writing managed terminal input")?;
    runtime
        .writer
        .flush()
        .context("flushing managed terminal input")
}

fn resize(runtime: &TerminalRuntime, cols: u16, rows: u16) -> Result<()> {
    runtime
        .master
        .resize(PtySize {
            rows: rows.max(1),
            cols: cols.max(2),
            pixel_width: 0,
            pixel_height: 0,
        })
        .context("resizing managed terminal PTY")
}

fn stop_runtime(runtime: &mut TerminalRuntime) {
    if let Err(error) = runtime.child.kill() {
        tracing::debug!(%error, "managed terminal attachment already stopped");
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

    #[test]
    fn attach_requires_exact_terminal_metadata() {
        let terminal = ToolsServiceTerminalInfo {
            id: "terminal-id".into(),
            session_key: "session:test".into(),
            session_id: "$1".into(),
            session_name: "session-test".into(),
            window_id: "@2".into(),
            window_name: "bash".into(),
            pane_id: "%3".into(),
            running: false,
        };
        assert_eq!(terminal.id, "terminal-id");
        assert_eq!(terminal.pane_id, "%3");
    }
}
