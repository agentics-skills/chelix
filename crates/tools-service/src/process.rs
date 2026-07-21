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
    use super::*;

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
    }
}
