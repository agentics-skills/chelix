use {
    anyhow::{Result, bail},
    chelix_protocol::{ProcessAction, ProcessRequest, ProcessResponse},
};

use crate::terminal::TerminalManager;

pub async fn run(manager: &TerminalManager, request: ProcessRequest) -> Result<ProcessResponse> {
    if request.session_key.trim().is_empty() {
        bail!("session_key cannot be empty");
    }

    match request.action {
        ProcessAction::SendKeys { terminal_id, keys } => {
            manager
                .send_terminal_keys(&request.session_key, &terminal_id, &keys)
                .await?;
            Ok(ProcessResponse::SendKeys { terminal_id })
        },
        ProcessAction::Paste { terminal_id, text } => {
            manager
                .paste_terminal_text(&request.session_key, &terminal_id, &text)
                .await?;
            Ok(ProcessResponse::Paste { terminal_id })
        },
        ProcessAction::Kill { terminal_id } => {
            manager
                .kill_terminal(&request.session_key, &terminal_id)
                .await?;
            Ok(ProcessResponse::Kill { terminal_id })
        },
        ProcessAction::List => Ok(ProcessResponse::List {
            terminal_ids: manager.terminal_ids(&request.session_key).await?,
        }),
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use chelix_protocol::ReadTerminalOutputRequest;

    use super::*;

    fn manager() -> TerminalManager {
        TerminalManager::new(std::env::temp_dir())
            .unwrap_or_else(|error| panic!("terminal manager setup failed: {error}"))
    }

    async fn wait_until_idle(manager: &TerminalManager, session_key: &str, terminal_id: &str) {
        let result = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let info = manager
                    .terminal_info(session_key, terminal_id)
                    .await
                    .unwrap_or_else(|error| panic!("terminal info failed: {error}"));
                if !info.running {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await;
        assert!(result.is_ok(), "terminal {terminal_id} did not become idle");
    }

    #[test]
    fn actions_deserialize_only_shared_terminal_ids() {
        let action = serde_json::from_value::<ProcessAction>(serde_json::json!({
            "action": "send_keys",
            "terminalId": "3",
            "keys": "C-c"
        }))
        .unwrap_or_else(|error| panic!("action decode failed: {error}"));

        assert_eq!(action, ProcessAction::SendKeys {
            terminal_id: "3".into(),
            keys: "C-c".into(),
        });

        assert!(
            serde_json::from_value::<ProcessAction>(serde_json::json!({
                "action": "create"
            }))
            .is_err()
        );
    }

    #[tokio::test]
    async fn paste_and_send_keys_control_an_existing_terminal() {
        let manager = manager();
        let terminal = manager
            .create_interactive_terminal("session:control", &[])
            .await
            .unwrap_or_else(|error| panic!("terminal creation failed: {error}"));
        wait_until_idle(&manager, "session:control", &terminal.id).await;

        let paste_response = run(&manager, ProcessRequest {
            session_key: "session:control".into(),
            action: ProcessAction::Paste {
                terminal_id: terminal.id.clone(),
                text: "printf 'pasted-output\\n'\r".into(),
            },
        })
        .await
        .unwrap_or_else(|error| panic!("paste failed: {error}"));
        assert_eq!(paste_response, ProcessResponse::Paste {
            terminal_id: terminal.id.clone(),
        });
        wait_until_idle(&manager, "session:control", &terminal.id).await;
        let output = manager
            .read_terminal_output(ReadTerminalOutputRequest {
                session_key: "session:control".into(),
                terminal_id: terminal.id.clone(),
                max_lines: None,
            })
            .await
            .unwrap_or_else(|error| panic!("terminal output read failed: {error}"));
        assert!(output.output.contains("pasted-output"));

        run(&manager, ProcessRequest {
            session_key: "session:control".into(),
            action: ProcessAction::Paste {
                terminal_id: terminal.id.clone(),
                text: "sleep 30\r".into(),
            },
        })
        .await
        .unwrap_or_else(|error| panic!("sleep paste failed: {error}"));
        tokio::time::sleep(Duration::from_millis(100)).await;
        let keys_response = run(&manager, ProcessRequest {
            session_key: "session:control".into(),
            action: ProcessAction::SendKeys {
                terminal_id: terminal.id.clone(),
                keys: "C-c".into(),
            },
        })
        .await
        .unwrap_or_else(|error| panic!("send_keys failed: {error}"));
        assert_eq!(keys_response, ProcessResponse::SendKeys {
            terminal_id: terminal.id.clone(),
        });
        wait_until_idle(&manager, "session:control", &terminal.id).await;

        manager
            .shutdown()
            .await
            .unwrap_or_else(|error| panic!("terminal shutdown failed: {error}"));
    }

    #[tokio::test]
    async fn list_and_kill_are_session_scoped() {
        let manager = manager();
        let first = manager
            .create_interactive_terminal("session:first", &[])
            .await
            .unwrap_or_else(|error| panic!("first terminal creation failed: {error}"));
        let second = manager
            .create_interactive_terminal("session:second", &[])
            .await
            .unwrap_or_else(|error| panic!("second terminal creation failed: {error}"));

        let list = run(&manager, ProcessRequest {
            session_key: "session:first".into(),
            action: ProcessAction::List,
        })
        .await
        .unwrap_or_else(|error| panic!("terminal list failed: {error}"));
        assert_eq!(list, ProcessResponse::List {
            terminal_ids: vec![first.id.clone()],
        });

        for action in [
            ProcessAction::SendKeys {
                terminal_id: second.id.clone(),
                keys: "C-c".into(),
            },
            ProcessAction::Paste {
                terminal_id: second.id.clone(),
                text: "text".into(),
            },
            ProcessAction::Kill {
                terminal_id: second.id.clone(),
            },
        ] {
            let error = match run(&manager, ProcessRequest {
                session_key: "session:first".into(),
                action,
            })
            .await
            {
                Ok(_) => panic!("expected process session ownership error"),
                Err(error) => error,
            };
            assert_eq!(
                error.to_string(),
                format!("terminal {} does not belong to this session", second.id)
            );
        }

        let kill = run(&manager, ProcessRequest {
            session_key: "session:first".into(),
            action: ProcessAction::Kill {
                terminal_id: first.id.clone(),
            },
        })
        .await
        .unwrap_or_else(|error| panic!("terminal kill failed: {error}"));
        assert_eq!(kill, ProcessResponse::Kill {
            terminal_id: first.id,
        });
        assert_eq!(
            run(&manager, ProcessRequest {
                session_key: "session:first".into(),
                action: ProcessAction::List,
            })
            .await
            .unwrap_or_else(|error| panic!("terminal list after kill failed: {error}")),
            ProcessResponse::List {
                terminal_ids: Vec::new(),
            }
        );

        manager
            .shutdown()
            .await
            .unwrap_or_else(|error| panic!("terminal shutdown failed: {error}"));
    }
}
