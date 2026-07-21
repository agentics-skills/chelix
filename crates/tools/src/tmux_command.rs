use std::{sync::Arc, time::Duration};

use {
    async_trait::async_trait,
    chelix_agents::tool_registry::AgentTool,
    chelix_protocol::{
        ExecuteCommandRequest, ExecuteCommandResponse, ReadTerminalOutputRequest,
        ReadTerminalOutputResponse, ToolsServiceEnvVar,
    },
    secrecy::ExposeSecret,
    serde::Deserialize,
    tracing::info,
};

use crate::{
    Result,
    approval::{ApprovalAction, ApprovalDecision, ApprovalManager},
    command::{CommandCompletionEvent, CommandCompletionFn, EnvVarProvider},
    error::Error,
    params::without_null_params,
    tools_service::ManagedToolsService,
};

const DEFAULT_TIMEOUT_MILLIS: u64 = 300_000;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ExecuteCommandParams {
    command: String,
    #[serde(default)]
    custom_cwd: Option<String>,
    #[serde(default)]
    new_terminal: bool,
    #[serde(default)]
    destructive_flag: Option<bool>,
    #[serde(default)]
    background: bool,
    #[serde(default)]
    timeout: Option<u64>,
    #[serde(default)]
    terminal_id: Option<String>,
    #[serde(rename = "_session_key", default)]
    session_key: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ReadTerminalOutputParams {
    terminal_id: String,
    #[serde(default)]
    max_lines: Option<usize>,
    #[serde(rename = "_session_key", default)]
    session_key: Option<String>,
}

pub struct ExecuteCommandTool {
    service: Arc<ManagedToolsService>,
    default_timeout: Duration,
    rewrite_timeout: Option<Duration>,
    approval_manager: Option<Arc<ApprovalManager>>,
    broadcaster: Option<Arc<dyn crate::approval::ApprovalBroadcaster>>,
    env_provider: Option<Arc<dyn EnvVarProvider>>,
    completion_callback: Option<CommandCompletionFn>,
}

impl ExecuteCommandTool {
    #[must_use]
    pub fn new(service: Arc<ManagedToolsService>) -> Self {
        Self {
            service,
            default_timeout: Duration::from_millis(DEFAULT_TIMEOUT_MILLIS),
            rewrite_timeout: None,
            approval_manager: None,
            broadcaster: None,
            env_provider: None,
            completion_callback: None,
        }
    }

    #[must_use]
    pub fn with_default_timeout(mut self, timeout: Duration) -> Self {
        self.default_timeout = timeout;
        self
    }

    #[must_use]
    pub fn with_rewrite_timeout(mut self, timeout: Option<Duration>) -> Self {
        self.rewrite_timeout = timeout;
        self
    }

    #[must_use]
    pub fn with_approval(
        mut self,
        manager: Arc<ApprovalManager>,
        broadcaster: Arc<dyn crate::approval::ApprovalBroadcaster>,
    ) -> Self {
        self.approval_manager = Some(manager);
        self.broadcaster = Some(broadcaster);
        self
    }

    #[must_use]
    pub fn with_env_provider(mut self, provider: Arc<dyn EnvVarProvider>) -> Self {
        self.env_provider = Some(provider);
        self
    }

    #[must_use]
    pub fn with_completion_callback(mut self, callback: CommandCompletionFn) -> Self {
        self.completion_callback = Some(callback);
        self
    }

    async fn approval_check(&self, command: &str, session_key: &str) -> Result<()> {
        let Some(manager) = self.approval_manager.as_ref() else {
            return Ok(());
        };
        if manager.check_command(command).await? != ApprovalAction::NeedsApproval {
            return Ok(());
        }

        let (request_id, receiver) = manager.create_request(command, Some(session_key)).await;
        if let Some(broadcaster) = self.broadcaster.as_ref() {
            broadcaster
                .broadcast_request(&request_id, command, Some(session_key))
                .await
                .map_err(|error| {
                    Error::message(format!("failed to broadcast command approval: {error}"))
                })?;
        }
        match manager.wait_for_decision(receiver).await {
            ApprovalDecision::Approved => Ok(()),
            ApprovalDecision::Denied => {
                Err(Error::message(format!("command denied by user: {command}")))
            },
            ApprovalDecision::Timeout => Err(Error::message(format!(
                "approval timed out for command: {command}"
            ))),
        }
    }

    async fn command_env(&self) -> Result<Vec<ToolsServiceEnvVar>> {
        let Some(provider) = self.env_provider.as_ref() else {
            return Ok(Vec::new());
        };
        provider
            .get_env_vars()
            .await
            .map_err(|error| {
                Error::message(format!("failed to load command environment: {error}"))
            })?
            .into_iter()
            .map(|variable| {
                let value = variable.value.expose_secret().clone();
                Ok(ToolsServiceEnvVar {
                    key: variable.key,
                    value,
                    secret: variable.secret,
                })
            })
            .collect()
    }

    fn fire_completion(&self, command: &str, response: &ExecuteCommandResponse) {
        if !response.completed {
            return;
        }
        if let Some(callback) = self.completion_callback.as_ref() {
            callback(CommandCompletionEvent {
                command: command.to_string(),
                exit_code: response.exit_code.unwrap_or(-1),
                stdout_preview: response.output.chars().take(200).collect(),
                stderr_preview: String::new(),
            });
        }
    }

    fn effective_timeout_millis(&self, requested_timeout: Option<u64>) -> Result<u64> {
        let Some(requested_timeout) = requested_timeout else {
            return duration_millis(self.default_timeout, "default command timeout");
        };
        let Some(rewrite_timeout) = self.rewrite_timeout else {
            return Ok(requested_timeout);
        };
        Ok(requested_timeout.max(duration_millis(rewrite_timeout, "command timeout rewrite")?))
    }
}

fn duration_millis(duration: Duration, name: &str) -> Result<u64> {
    u64::try_from(duration.as_millis())
        .map_err(|_| Error::message(format!("{name} exceeds the supported millisecond range")))
}

#[async_trait]
impl AgentTool for ExecuteCommandTool {
    fn name(&self) -> &str {
        "execute_command"
    }

    fn description(&self) -> &str {
        "Execute a shell command in a tmux terminal managed by chelix-tools-service. Returns terminalId for follow-up read_terminal_output calls."
    }

    fn agent_result(
        &self,
        _params: &serde_json::Value,
        raw_result: &serde_json::Value,
    ) -> anyhow::Result<serde_json::Value> {
        let response: ExecuteCommandResponse = serde_json::from_value(raw_result.clone())?;
        Ok(serde_json::Value::String(format_execute_result(&response)))
    }

    fn parameters_schema(&self) -> serde_json::Value {
        let timeout_default = self.default_timeout.as_millis();
        serde_json::json!({
            "type": "object",
            "properties": {
                "command": {
                    "type": "string",
                    "description": "Shell command to execute"
                },
                "customCwd": {
                    "type": "string",
                    "description": "Working directory for the command"
                },
                "newTerminal": {
                    "type": "boolean",
                    "description": "If true, create a new tmux window/terminal"
                },
                "destructiveFlag": {
                    "type": "boolean",
                    "description": "Approval UI hint for potentially destructive commands"
                },
                "background": {
                    "type": "boolean",
                    "description": "If true, start the command and return immediately"
                },
                "timeout": {
                    "type": "integer",
                    "description": format!("Milliseconds to wait for completion (default {timeout_default})")
                },
                "terminalId": {
                    "type": "string",
                    "description": "Managed terminal id returned by a previous execute_command call"
                }
            },
            "required": ["command"],
            "additionalProperties": false
        })
    }

    async fn execute(&self, params: serde_json::Value) -> anyhow::Result<serde_json::Value> {
        let params: ExecuteCommandParams = serde_json::from_value(without_null_params(params))?;
        let session_key = params.session_key.as_deref().unwrap_or("main").to_string();
        let command = params.command.trim().to_string();
        if command.is_empty() {
            return Err(Error::message("command cannot be empty").into());
        }
        if params.destructive_flag.unwrap_or(false) {
            tracing::debug!("execute_command destructive_flag provided for approval UI context");
        }
        self.approval_check(&command, &session_key).await?;
        let timeout_millis = self.effective_timeout_millis(params.timeout)?;
        let request = ExecuteCommandRequest {
            session_key: session_key.clone(),
            command: command.clone(),
            custom_cwd: params.custom_cwd,
            new_terminal: params.new_terminal,
            background: params.background,
            timeout_millis,
            terminal_id: params.terminal_id,
            env: self.command_env().await?,
        };
        info!(session = session_key, "execute_command tool invoked");
        let response = self.service.execute_command(&session_key, request).await?;
        self.fire_completion(&command, &response);
        Ok(serde_json::to_value(response)?)
    }
}

pub struct ReadTerminalOutputTool {
    service: Arc<ManagedToolsService>,
}

impl ReadTerminalOutputTool {
    #[must_use]
    pub fn new(service: Arc<ManagedToolsService>) -> Self {
        Self { service }
    }
}

#[async_trait]
impl AgentTool for ReadTerminalOutputTool {
    fn name(&self) -> &str {
        "read_terminal_output"
    }

    fn description(&self) -> &str {
        "Read current output from a tmux terminal managed by chelix-tools-service."
    }

    fn agent_result(
        &self,
        _params: &serde_json::Value,
        raw_result: &serde_json::Value,
    ) -> anyhow::Result<serde_json::Value> {
        let response: ReadTerminalOutputResponse = serde_json::from_value(raw_result.clone())?;
        Ok(serde_json::Value::String(format_terminal_output(&response)))
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "terminalId": {
                    "type": "string",
                    "description": "Managed terminal id returned by execute_command"
                },
                "maxLines": {
                    "type": "integer",
                    "description": "Maximum number of tmux scrollback lines to read"
                }
            },
            "required": ["terminalId"],
            "additionalProperties": false
        })
    }

    async fn execute(&self, params: serde_json::Value) -> anyhow::Result<serde_json::Value> {
        let params: ReadTerminalOutputParams = serde_json::from_value(without_null_params(params))?;
        let session_key = params.session_key.as_deref().unwrap_or("main").to_string();
        let request = ReadTerminalOutputRequest {
            session_key: session_key.clone(),
            terminal_id: params.terminal_id,
            max_lines: params.max_lines,
        };
        Ok(serde_json::to_value(
            self.service
                .read_terminal_output(&session_key, request)
                .await?,
        )?)
    }
}

fn format_execute_result(response: &ExecuteCommandResponse) -> String {
    let status = if response.background {
        format!(
            "Command started in terminal (id: {}).",
            response.terminal_id
        )
    } else if response.timed_out {
        format!(
            "Command is still running in terminal (id: {}).",
            response.terminal_id
        )
    } else {
        format!(
            "Command finished in terminal (id: {}).",
            response.terminal_id
        )
    };
    format_output(status, &response.output)
}

fn format_terminal_output(response: &ReadTerminalOutputResponse) -> String {
    let status = if response.running {
        format!("Terminal {} is running.", response.terminal_id)
    } else if response.completed {
        format!("Terminal {} completed.", response.terminal_id)
    } else {
        format!("Terminal {} output read.", response.terminal_id)
    };
    format_output(status, &response.output)
}

fn format_output(status: String, output: &str) -> String {
    if output.is_empty() {
        status
    } else {
        format!("{status}\nOutput:\n{output}")
    }
}

#[cfg(test)]
mod tests {
    use crate::sandbox::ToolsServiceEndpoint;

    use super::*;

    fn client(base_url: String, token: &str) -> Arc<ManagedToolsService> {
        ManagedToolsService::for_test(ToolsServiceEndpoint {
            base_url,
            token: token.into(),
        })
        .unwrap_or_else(|error| panic!("test client failed: {error}"))
    }

    #[tokio::test]
    async fn execute_routes_exclusively_to_service() {
        let mut server = mockito::Server::new_async().await;
        let call = server
            .mock("POST", chelix_protocol::TOOLS_SERVICE_EXECUTE_COMMAND_PATH)
            .match_header("authorization", "Bearer command-token")
            .match_body(mockito::Matcher::PartialJson(serde_json::json!({
                "sessionKey": "session:test",
                "command": "printf ok",
                "timeoutMillis": DEFAULT_TIMEOUT_MILLIS
            })))
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                serde_json::json!({
                    "terminalId": "terminal",
                    "runId": "run",
                    "sessionId": "$0",
                    "sessionName": "main",
                    "windowId": "@0",
                    "windowName": "bash",
                    "paneId": "%0",
                    "output": "ok",
                    "exitCode": 0,
                    "completed": true,
                    "timedOut": false,
                    "background": false,
                    "message": "done"
                })
                .to_string(),
            )
            .expect(1)
            .create_async()
            .await;
        let tool = ExecuteCommandTool::new(client(server.url(), "command-token"));

        let result = tool
            .execute(serde_json::json!({
                "command": "printf ok",
                "_session_key": "session:test"
            }))
            .await
            .unwrap_or_else(|error| panic!("execute failed: {error}"));

        assert_eq!(result["terminalId"], "terminal");
        assert_eq!(
            tool.agent_result(&serde_json::json!({}), &result)
                .unwrap_or_else(|error| panic!("agent result failed: {error}")),
            "Command finished in terminal (id: terminal).\nOutput:\nok"
        );
        call.assert_async().await;
    }

    #[test]
    fn effective_timeout_uses_default_or_rewrites_explicit_value() {
        let tool = ExecuteCommandTool::new(client("http://127.0.0.1:1".into(), "unused"))
            .with_default_timeout(Duration::from_secs(60))
            .with_rewrite_timeout(Some(Duration::from_secs(300)));

        for (requested, expected) in [
            (None, 60_000),
            (Some(10_000), 300_000),
            (Some(100_000), 300_000),
            (Some(300_000), 300_000),
            (Some(600_000), 600_000),
            (Some(7_200_000), 7_200_000),
        ] {
            assert_eq!(
                tool.effective_timeout_millis(requested)
                    .unwrap_or_else(|error| panic!("timeout resolution failed: {error}")),
                expected
            );
        }
    }

    #[test]
    fn effective_timeout_preserves_explicit_value_without_rewrite() {
        let tool = ExecuteCommandTool::new(client("http://127.0.0.1:1".into(), "unused"))
            .with_default_timeout(Duration::from_secs(60));

        assert_eq!(
            tool.effective_timeout_millis(Some(10_000))
                .unwrap_or_else(|error| panic!("timeout resolution failed: {error}")),
            10_000
        );
    }

    #[test]
    fn schemas_have_no_node_route() {
        let schema = ExecuteCommandTool::new(client("http://127.0.0.1:1".into(), "unused"))
            .parameters_schema();

        assert!(schema["properties"].get("node").is_none());
        assert_eq!(schema["additionalProperties"], false);
    }
}
