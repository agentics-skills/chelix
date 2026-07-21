use std::{collections::HashMap, sync::Arc};

use {
    anyhow::{Result, bail},
    chelix_protocol::{
        ProcessAction, ProcessRequest, ProcessResponse, ToolsServiceTerminalInfo,
        ToolsServiceTerminalKind,
    },
    tokio::sync::Mutex,
};

use crate::tmux::{CommandOutput, TmuxRuntime, command_error, is_no_server};

pub struct ProcessManager {
    runtime: Arc<TmuxRuntime>,
    sessions: Mutex<HashMap<(String, String), String>>,
}

impl ProcessManager {
    pub fn new(runtime: Arc<TmuxRuntime>) -> Self {
        Self {
            runtime,
            sessions: Mutex::new(HashMap::new()),
        }
    }

    pub async fn run(&self, request: ProcessRequest) -> Result<ProcessResponse> {
        if request.session_key.trim().is_empty() {
            bail!("session_key cannot be empty");
        }

        match request.action {
            ProcessAction::Start {
                command,
                session_name,
            } => {
                self.start(&request.session_key, &command, session_name.as_deref())
                    .await
            },
            ProcessAction::Poll { session_name } => {
                self.poll(&request.session_key, &session_name).await
            },
            ProcessAction::SendKeys { session_name, keys } => {
                self.send_keys(&request.session_key, &session_name, &keys)
                    .await
            },
            ProcessAction::Paste { session_name, text } => {
                self.paste(&request.session_key, &session_name, &text).await
            },
            ProcessAction::Kill { session_name } => {
                self.kill(&request.session_key, &session_name).await
            },
            ProcessAction::List => self.list(&request.session_key).await,
        }
    }

    pub async fn terminal_infos(&self) -> Result<Vec<ToolsServiceTerminalInfo>> {
        let entries = self
            .sessions
            .lock()
            .await
            .iter()
            .map(|((session_key, logical_name), physical_name)| {
                (
                    session_key.clone(),
                    logical_name.clone(),
                    physical_name.clone(),
                )
            })
            .collect::<Vec<_>>();
        let mut infos = Vec::with_capacity(entries.len());
        for (session_key, logical_name, physical_name) in entries {
            match self
                .process_terminal_info(&session_key, &logical_name, &physical_name)
                .await?
            {
                Some(info) => infos.push(info),
                None => {
                    self.sessions
                        .lock()
                        .await
                        .remove(&(session_key, logical_name));
                },
            }
        }
        infos.sort_by(|left, right| left.id.cmp(&right.id));
        Ok(infos)
    }

    pub async fn terminal_info(
        &self,
        session_key: &str,
        logical_name: &str,
    ) -> Result<ToolsServiceTerminalInfo> {
        let physical_name = self
            .resolve_name(session_key, logical_name)
            .await
            .ok_or_else(|| anyhow::anyhow!("process session not found: {logical_name}"))?;
        let Some(info) = self
            .process_terminal_info(session_key, logical_name, &physical_name)
            .await?
        else {
            self.sessions
                .lock()
                .await
                .remove(&(session_key.to_string(), logical_name.to_string()));
            bail!("process session no longer exists: {logical_name}");
        };
        Ok(info)
    }

    async fn process_terminal_info(
        &self,
        session_key: &str,
        logical_name: &str,
        physical_name: &str,
    ) -> Result<Option<ToolsServiceTerminalInfo>> {
        let output = self
            .runtime
            .run(&[
                "list-panes".into(),
                "-t".into(),
                physical_name.into(),
                "-F".into(),
                "#{session_id}|chelix-tmux-field|#{session_name}|chelix-tmux-field|#{window_id}|chelix-tmux-field|#{window_name}|chelix-tmux-field|#{pane_id}|chelix-tmux-field|#{pane_dead}".into(),
            ])
            .await?;
        if output.exit_code != 0 {
            let error = command_error(&output);
            if is_missing_session(&error) || is_no_server(&error) {
                return Ok(None);
            }
            bail!("failed to inspect process session {logical_name}: {error}");
        }
        let line = output
            .stdout
            .lines()
            .find(|line| !line.trim().is_empty())
            .ok_or_else(|| anyhow::anyhow!("process session {logical_name} has no tmux pane"))?;
        Ok(Some(parse_process_terminal_info(
            session_key,
            logical_name,
            line,
        )?))
    }

    async fn start(
        &self,
        session_key: &str,
        command: &str,
        requested_name: Option<&str>,
    ) -> Result<ProcessResponse> {
        if command.trim().is_empty() {
            return Ok(error_response("command cannot be empty"));
        }
        let logical_name = match requested_name.filter(|name| !name.is_empty()) {
            Some(name) if is_valid_session_name(name) => name.to_string(),
            Some(_) => return Ok(invalid_name_response()),
            None => format!("proc-{}", &uuid::Uuid::new_v4().simple().to_string()[..8]),
        };
        let key = (session_key.to_string(), logical_name.clone());
        let mut sessions = self.sessions.lock().await;
        if sessions.contains_key(&key) {
            return Ok(error_response(format!(
                "process session already exists: {logical_name}"
            )));
        }
        let physical_name = format!("chelix-process-{}", uuid::Uuid::new_v4().simple());
        let output = self
            .runtime
            .run(&[
                "new-session".into(),
                "-d".into(),
                "-s".into(),
                physical_name.clone(),
                "-x".into(),
                "200".into(),
                "-y".into(),
                "50".into(),
                "bash".into(),
                "-lc".into(),
                command.into(),
            ])
            .await?;
        if output.exit_code != 0 {
            return Ok(error_response(format!(
                "tmux start failed: {}",
                command_error(&output)
            )));
        }
        sessions.insert(key, physical_name);
        Ok(success_response(
            Some(logical_name),
            Some("session started".into()),
        ))
    }

    async fn poll(&self, session_key: &str, logical_name: &str) -> Result<ProcessResponse> {
        let Some(physical_name) = self.resolve_name(session_key, logical_name).await else {
            return Ok(not_found_response(logical_name));
        };
        let output = self
            .runtime
            .run(&[
                "capture-pane".into(),
                "-t".into(),
                physical_name,
                "-p".into(),
            ])
            .await?;
        if output.exit_code != 0 {
            self.remove_if_missing(session_key, logical_name, &output)
                .await;
            return Ok(error_response(format!(
                "poll failed: {}",
                command_error(&output)
            )));
        }
        Ok(success_response(
            Some(logical_name.into()),
            Some(output.stdout),
        ))
    }

    async fn send_keys(
        &self,
        session_key: &str,
        logical_name: &str,
        keys: &str,
    ) -> Result<ProcessResponse> {
        let Some(physical_name) = self.resolve_name(session_key, logical_name).await else {
            return Ok(not_found_response(logical_name));
        };
        let output = self
            .runtime
            .run(&["send-keys".into(), "-t".into(), physical_name, keys.into()])
            .await?;
        if output.exit_code != 0 {
            self.remove_if_missing(session_key, logical_name, &output)
                .await;
            return Ok(error_response(format!(
                "send_keys failed: {}",
                command_error(&output)
            )));
        }
        Ok(success_response(
            Some(logical_name.into()),
            Some("keys sent".into()),
        ))
    }

    async fn paste(
        &self,
        session_key: &str,
        logical_name: &str,
        text: &str,
    ) -> Result<ProcessResponse> {
        let Some(physical_name) = self.resolve_name(session_key, logical_name).await else {
            return Ok(not_found_response(logical_name));
        };
        let buffer = format!("chelix-process-{}", uuid::Uuid::new_v4().simple());
        let set = self
            .runtime
            .run(&[
                "set-buffer".into(),
                "-b".into(),
                buffer.clone(),
                "--".into(),
                text.into(),
            ])
            .await?;
        if set.exit_code != 0 {
            return Ok(error_response(format!(
                "set-buffer failed: {}",
                command_error(&set)
            )));
        }
        let output = self
            .runtime
            .run(&[
                "paste-buffer".into(),
                "-d".into(),
                "-b".into(),
                buffer,
                "-t".into(),
                physical_name,
            ])
            .await?;
        if output.exit_code != 0 {
            self.remove_if_missing(session_key, logical_name, &output)
                .await;
            return Ok(error_response(format!(
                "paste-buffer failed: {}",
                command_error(&output)
            )));
        }
        Ok(success_response(
            Some(logical_name.into()),
            Some("text pasted".into()),
        ))
    }

    async fn kill(&self, session_key: &str, logical_name: &str) -> Result<ProcessResponse> {
        let Some(physical_name) = self.resolve_name(session_key, logical_name).await else {
            return Ok(not_found_response(logical_name));
        };
        let output = self
            .runtime
            .run(&["kill-session".into(), "-t".into(), physical_name])
            .await?;
        if output.exit_code != 0 && !is_missing_session(&command_error(&output)) {
            return Ok(error_response(format!(
                "kill failed: {}",
                command_error(&output)
            )));
        }
        self.sessions
            .lock()
            .await
            .remove(&(session_key.to_string(), logical_name.to_string()));
        Ok(success_response(
            Some(logical_name.into()),
            Some("session killed".into()),
        ))
    }

    async fn list(&self, session_key: &str) -> Result<ProcessResponse> {
        let entries = self
            .sessions
            .lock()
            .await
            .iter()
            .filter(|((owner, _), _)| owner == session_key)
            .map(|((_, logical), physical)| (logical.clone(), physical.clone()))
            .collect::<Vec<_>>();
        let mut active = Vec::new();
        for (logical_name, physical_name) in entries {
            let output = self
                .runtime
                .run(&["has-session".into(), "-t".into(), physical_name])
                .await?;
            if output.exit_code == 0 {
                active.push(logical_name);
            } else if is_missing_session(&command_error(&output))
                || is_no_server(&command_error(&output))
            {
                self.sessions
                    .lock()
                    .await
                    .remove(&(session_key.to_string(), logical_name));
            } else {
                return Ok(error_response(format!(
                    "list failed: {}",
                    command_error(&output)
                )));
            }
        }
        active.sort();
        let output = if active.is_empty() {
            "no active sessions".into()
        } else {
            active.join("\n")
        };
        Ok(success_response(None, Some(output)))
    }

    async fn resolve_name(&self, session_key: &str, logical_name: &str) -> Option<String> {
        if !is_valid_session_name(logical_name) {
            return None;
        }
        self.sessions
            .lock()
            .await
            .get(&(session_key.to_string(), logical_name.to_string()))
            .cloned()
    }

    async fn remove_if_missing(
        &self,
        session_key: &str,
        logical_name: &str,
        output: &CommandOutput,
    ) {
        let message = command_error(output);
        if is_missing_session(&message) || is_no_server(&message) {
            self.sessions
                .lock()
                .await
                .remove(&(session_key.to_string(), logical_name.to_string()));
        }
    }
}

fn invalid_name_response() -> ProcessResponse {
    error_response("invalid session_name: only [a-zA-Z0-9_-] allowed, max 64 chars")
}

fn not_found_response(logical_name: &str) -> ProcessResponse {
    if is_valid_session_name(logical_name) {
        error_response(format!("process session not found: {logical_name}"))
    } else {
        invalid_name_response()
    }
}

fn is_valid_session_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || matches!(character, '_' | '-'))
}

fn is_missing_session(message: &str) -> bool {
    let lower = message.to_ascii_lowercase();
    lower.contains("can't find session") || lower.contains("no such session")
}

fn parse_process_terminal_info(
    session_key: &str,
    logical_name: &str,
    line: &str,
) -> Result<ToolsServiceTerminalInfo> {
    let fields = line.split("|chelix-tmux-field|").collect::<Vec<_>>();
    if fields.len() != 6 || !matches!(fields[5], "0" | "1") {
        bail!("invalid tmux process pane metadata: {line}");
    }
    Ok(ToolsServiceTerminalInfo {
        kind: ToolsServiceTerminalKind::Process,
        id: logical_name.to_string(),
        session_key: session_key.to_string(),
        session_id: fields[0].to_string(),
        session_name: fields[1].to_string(),
        window_id: fields[2].to_string(),
        window_name: fields[3].to_string(),
        pane_id: fields[4].to_string(),
        running: fields[5] == "0",
    })
}

fn success_response(session_name: Option<String>, output: Option<String>) -> ProcessResponse {
    ProcessResponse {
        success: true,
        session_name,
        output,
        error: None,
    }
}

fn error_response(error: impl Into<String>) -> ProcessResponse {
    ProcessResponse {
        success: false,
        session_name: None,
        output: None,
        error: Some(error.into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn valid_names_are_accepted() {
        assert!(is_valid_session_name("my-session_1"));
        assert!(!is_valid_session_name("has space"));
        assert!(!is_valid_session_name("has;separator"));
        assert!(!is_valid_session_name(&"a".repeat(65)));
    }

    #[test]
    fn invalid_and_unknown_names_are_distinct() {
        assert_eq!(
            invalid_name_response().error.as_deref(),
            Some("invalid session_name: only [a-zA-Z0-9_-] allowed, max 64 chars")
        );
        assert_eq!(
            not_found_response("repl").error.as_deref(),
            Some("process session not found: repl")
        );
    }

    #[test]
    fn error_response_has_no_success_payload() {
        let response = error_response("failed");

        assert!(!response.success);
        assert_eq!(response.error.as_deref(), Some("failed"));
        assert!(response.session_name.is_none());
        assert!(response.output.is_none());
    }

    #[test]
    fn process_terminal_metadata_uses_tmux_pane_dead_state() {
        let running = parse_process_terminal_info(
            "session:test",
            "repl",
            "$1|chelix-tmux-field|physical|chelix-tmux-field|@2|chelix-tmux-field|bash|chelix-tmux-field|%3|chelix-tmux-field|0",
        )
        .unwrap_or_else(|error| panic!("parse failed: {error}"));
        let stopped = parse_process_terminal_info(
            "session:test",
            "repl",
            "$1|chelix-tmux-field|physical|chelix-tmux-field|@2|chelix-tmux-field|bash|chelix-tmux-field|%3|chelix-tmux-field|1",
        )
        .unwrap_or_else(|error| panic!("parse failed: {error}"));

        assert!(running.running);
        assert!(!stopped.running);
        assert_eq!(running.id, "repl");
        assert_eq!(running.pane_id, "%3");
    }

    #[test]
    fn process_terminal_metadata_rejects_unknown_pane_state() {
        let result = parse_process_terminal_info(
            "session:test",
            "repl",
            "$1|chelix-tmux-field|physical|chelix-tmux-field|@2|chelix-tmux-field|bash|chelix-tmux-field|%3|chelix-tmux-field|unknown",
        );

        assert!(result.is_err());
    }
}
