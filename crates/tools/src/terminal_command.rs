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
    approval::{ApprovalDecision, ApprovalManager},
    command::{CommandCompletionEvent, CommandCompletionFn, EnvVarProvider, redact_secret_values},
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
    response_timeout_ms: Option<u64>,
    #[serde(default)]
    terminal_id: Option<String>,
    #[serde(rename = "_session_key", default)]
    session_key: Option<String>,
    #[serde(rename = "_tool_call_id")]
    tool_call_id: String,
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
        if !manager.needs_approval() {
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
        "Execute a Bash command in a persistent terminal that preserves its working directory and environment variables across requests. Returns terminalId for follow-up read_terminal_output calls."
    }

    async fn agent_result(
        &self,
        _params: &serde_json::Value,
        raw_result: &serde_json::Value,
    ) -> anyhow::Result<serde_json::Value> {
        let mut response: ExecuteCommandResponse = serde_json::from_value(raw_result.clone())?;
        redact_secret_values(
            &mut response.output,
            &self.service.current_terminal_secret_values().await?,
        );
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
                    "description": "If true, create a new persistent terminal"
                },
                "destructiveFlag": {
                    "type": "boolean",
                    "description": "Approval UI hint for potentially destructive commands"
                },
                "background": {
                    "type": "boolean",
                    "description": "If true, start the command and return immediately"
                },
                "responseTimeoutMs": {
                    "type": "integer",
                    "description": format!("Blocking wait, in milliseconds, for capturing command output; when it expires, the tool detaches from the still-running command. Set this value to several times the command's expected completion time to prevent the wait from ending and detaching prematurely (default {timeout_default})")
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
        let custom_cwd = params.custom_cwd.filter(|value| !value.is_empty());
        let terminal_id = params.terminal_id.filter(|value| !value.is_empty());
        if command.is_empty() {
            return Err(Error::message("command cannot be empty").into());
        }
        if params.destructive_flag.unwrap_or(false) {
            tracing::debug!("execute_command destructive_flag provided for approval UI context");
        }
        self.approval_check(&command, &session_key).await?;
        let timeout_millis = self.effective_timeout_millis(params.response_timeout_ms)?;
        let request = ExecuteCommandRequest {
            session_key: session_key.clone(),
            tool_call_id: params.tool_call_id,
            command: command.clone(),
            custom_cwd,
            new_terminal: params.new_terminal,
            background: params.background,
            timeout_millis,
            terminal_id,
            env: self.command_env().await?,
        };
        info!(session = session_key, "execute_command tool invoked");
        let response = self.service.execute_command(&session_key, request).await?;
        self.fire_completion(&command, &response);
        let mut result = serde_json::to_value(&response)?;
        if response.completed && response.exit_code == Some(1) {
            result["error"] = serde_json::Value::String(format_execute_status(&response));
        }
        Ok(result)
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
        "Read retained history from a persistent terminal managed by chelix-tools-service."
    }

    async fn agent_result(
        &self,
        _params: &serde_json::Value,
        raw_result: &serde_json::Value,
    ) -> anyhow::Result<serde_json::Value> {
        let mut response: ReadTerminalOutputResponse = serde_json::from_value(raw_result.clone())?;
        redact_secret_values(
            &mut response.output,
            &self.service.current_terminal_secret_values().await?,
        );
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
                    "description": "Maximum number of retained terminal history lines to read"
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
    format_output(format_execute_status(response), &response.output)
}

fn format_execute_status(response: &ExecuteCommandResponse) -> String {
    if response.background {
        return format!(
            "Command started in terminal (id: {}).",
            response.terminal_id
        );
    }
    if response.timed_out {
        return format!(
            "Command is still running in terminal (id: {}).",
            response.terminal_id
        );
    }
    let Some(exit_code) = response.exit_code else {
        return format!(
            "Command finished in terminal (id: {}).",
            response.terminal_id
        );
    };
    format!(
        "Command finished in terminal (id: {}) with exit code {exit_code}.",
        response.terminal_id
    )
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
    use {
        crate::{
            command::{EnvVarProvider, InjectedEnvVar},
            sandbox::ToolsServiceEndpoint,
        },
        secrecy::Secret,
    };

    use super::*;

    struct StaticEnvProvider {
        variables: Vec<InjectedEnvVar>,
    }

    #[async_trait]
    impl EnvVarProvider for StaticEnvProvider {
        async fn get_env_vars(&self) -> anyhow::Result<Vec<InjectedEnvVar>> {
            Ok(self.variables.clone())
        }
    }

    fn response(
        output: &str,
        completed: bool,
        timed_out: bool,
        background: bool,
    ) -> ExecuteCommandResponse {
        ExecuteCommandResponse {
            terminal_id: "7".into(),
            run_id: "run".into(),
            output: output.into(),
            exit_code: completed.then_some(0),
            completed,
            alive: true,
            timed_out,
            background,
            message: "state".into(),
        }
    }

    fn execute_response(terminal_id: &str, exit_code: i32) -> String {
        serde_json::json!({
            "terminalId": terminal_id,
            "runId": "run",
            "output": "ok",
            "exitCode": exit_code,
            "completed": true,
            "alive": true,
            "timedOut": false,
            "background": false,
            "message": "done"
        })
        .to_string()
    }

    fn client(base_url: String, token: &str) -> Arc<ManagedToolsService> {
        ManagedToolsService::for_test(ToolsServiceEndpoint {
            base_url,
            token: token.into(),
        })
        .unwrap_or_else(|error| panic!("test client failed: {error}"))
    }

    fn initialize_empty_environment(service: &ManagedToolsService) {
        service
            .set_env_provider(Arc::new(StaticEnvProvider {
                variables: Vec::new(),
            }))
            .unwrap_or_else(|error| panic!("environment provider setup failed: {error}"));
    }

    #[tokio::test]
    async fn execute_routes_exclusively_to_service() {
        let mut server = mockito::Server::new_async().await;
        let call = server
            .mock("POST", chelix_protocol::TOOLS_SERVICE_EXECUTE_COMMAND_PATH)
            .match_header("authorization", "Bearer command-token")
            .match_body(mockito::Matcher::PartialJson(serde_json::json!({
                "sessionKey": "session:test",
                "toolCallId": "call-routes",
                "command": "printf ok",
                "customCwd": null,
                "newTerminal": false,
                "background": false,
                "timeoutMillis": DEFAULT_TIMEOUT_MILLIS,
                "terminalId": null,
                "env": []
            })))
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(execute_response("terminal", 0))
            .expect(1)
            .create_async()
            .await;
        let service = client(server.url(), "command-token");
        initialize_empty_environment(&service);
        let tool = ExecuteCommandTool::new(service);

        let result = tool
            .execute(serde_json::json!({
                "command": "printf ok",
                "_session_key": "session:test",
                "_tool_call_id": "call-routes"
            }))
            .await
            .unwrap_or_else(|error| panic!("execute failed: {error}"));

        assert_eq!(result["terminalId"], "terminal");
        assert_eq!(
            tool.agent_result(&serde_json::json!({}), &result)
                .await
                .unwrap_or_else(|error| panic!("agent result failed: {error}")),
            "Command finished in terminal (id: terminal) with exit code 0.\nOutput:\nok"
        );
        call.assert_async().await;
    }

    #[tokio::test]
    async fn execute_marks_only_completed_exit_code_one_as_error() {
        let mut server = mockito::Server::new_async().await;
        let exit_one_call = server
            .mock("POST", chelix_protocol::TOOLS_SERVICE_EXECUTE_COMMAND_PATH)
            .match_body(mockito::Matcher::PartialJson(serde_json::json!({
                "toolCallId": "call-exit-one"
            })))
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(execute_response("1", 1))
            .expect(1)
            .create_async()
            .await;
        let exit_two_call = server
            .mock("POST", chelix_protocol::TOOLS_SERVICE_EXECUTE_COMMAND_PATH)
            .match_body(mockito::Matcher::PartialJson(serde_json::json!({
                "toolCallId": "call-exit-two"
            })))
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(execute_response("2", 2))
            .expect(1)
            .create_async()
            .await;
        let background_exit_one_call = server
            .mock("POST", chelix_protocol::TOOLS_SERVICE_EXECUTE_COMMAND_PATH)
            .match_body(mockito::Matcher::PartialJson(serde_json::json!({
                "toolCallId": "call-background-exit-one"
            })))
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                serde_json::json!({
                    "terminalId": "3",
                    "runId": "run",
                    "output": "",
                    "exitCode": 1,
                    "completed": false,
                    "alive": true,
                    "timedOut": false,
                    "background": true,
                    "message": "started"
                })
                .to_string(),
            )
            .expect(1)
            .create_async()
            .await;
        let service = client(server.url(), "command-token");
        initialize_empty_environment(&service);
        let tool = ExecuteCommandTool::new(service);

        let exit_one = tool
            .execute(serde_json::json!({
                "command": "exit 1",
                "_tool_call_id": "call-exit-one"
            }))
            .await
            .unwrap_or_else(|error| panic!("exit 1 execution failed: {error}"));
        let exit_two = tool
            .execute(serde_json::json!({
                "command": "exit 2",
                "_tool_call_id": "call-exit-two"
            }))
            .await
            .unwrap_or_else(|error| panic!("exit 2 execution failed: {error}"));
        let background_exit_one = tool
            .execute(serde_json::json!({
                "command": "exit 1",
                "background": true,
                "_tool_call_id": "call-background-exit-one"
            }))
            .await
            .unwrap_or_else(|error| panic!("background exit 1 execution failed: {error}"));

        assert_eq!(
            exit_one["error"],
            "Command finished in terminal (id: 1) with exit code 1."
        );
        assert_eq!(
            tool.agent_result(&serde_json::json!({}), &exit_one)
                .await
                .unwrap_or_else(|error| panic!("exit 1 agent result failed: {error}")),
            "Command finished in terminal (id: 1) with exit code 1.\nOutput:\nok"
        );
        assert!(exit_two.get("error").is_none());
        assert!(background_exit_one.get("error").is_none());
        exit_one_call.assert_async().await;
        exit_two_call.assert_async().await;
        background_exit_one_call.assert_async().await;
    }

    #[tokio::test]
    async fn execute_treats_empty_routing_strings_as_omitted() {
        let mut server = mockito::Server::new_async().await;
        let call = server
            .mock("POST", chelix_protocol::TOOLS_SERVICE_EXECUTE_COMMAND_PATH)
            .match_body(mockito::Matcher::PartialJson(serde_json::json!({
                "command": "pwd",
                "customCwd": null,
                "newTerminal": true,
                "terminalId": null
            })))
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(execute_response("1", 0))
            .expect(1)
            .create_async()
            .await;
        let tool = ExecuteCommandTool::new(client(server.url(), "command-token"));

        let result = tool
            .execute(serde_json::json!({
                "command": "pwd",
                "customCwd": "",
                "newTerminal": true,
                "terminalId": "",
                "_tool_call_id": "call-empty-routing"
            }))
            .await
            .unwrap_or_else(|error| panic!("execute failed: {error}"));

        assert_eq!(result["terminalId"], "1");
        call.assert_async().await;
    }

    #[tokio::test]
    async fn execute_preserves_non_empty_routing_values() {
        let mut server = mockito::Server::new_async().await;
        let call = server
            .mock("POST", chelix_protocol::TOOLS_SERVICE_EXECUTE_COMMAND_PATH)
            .match_body(mockito::Matcher::PartialJson(serde_json::json!({
                "command": "pwd",
                "customCwd": "/tmp",
                "newTerminal": false,
                "terminalId": "42"
            })))
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(execute_response("42", 0))
            .expect(1)
            .create_async()
            .await;
        let tool = ExecuteCommandTool::new(client(server.url(), "command-token"));

        let result = tool
            .execute(serde_json::json!({
                "command": "pwd",
                "customCwd": "/tmp",
                "terminalId": "42",
                "_tool_call_id": "call-routing-values"
            }))
            .await
            .unwrap_or_else(|error| panic!("execute failed: {error}"));

        assert_eq!(result["terminalId"], "42");
        call.assert_async().await;
    }

    #[tokio::test]
    async fn execute_propagates_new_terminal_id_conflict() {
        let mut server = mockito::Server::new_async().await;
        let call = server
            .mock("POST", chelix_protocol::TOOLS_SERVICE_EXECUTE_COMMAND_PATH)
            .match_body(mockito::Matcher::PartialJson(serde_json::json!({
                "command": "pwd",
                "newTerminal": true,
                "terminalId": "42"
            })))
            .with_status(400)
            .with_header("content-type", "application/json")
            .with_body(r#"{"error":"terminalId cannot be combined with newTerminal=true"}"#)
            .expect(1)
            .create_async()
            .await;
        let tool = ExecuteCommandTool::new(client(server.url(), "command-token"));

        let error = match tool
            .execute(serde_json::json!({
                "command": "pwd",
                "newTerminal": true,
                "terminalId": "42",
                "_tool_call_id": "call-routing-conflict"
            }))
            .await
        {
            Ok(_) => panic!("expected terminal routing conflict"),
            Err(error) => error,
        };

        assert_eq!(
            error.to_string(),
            "terminalId cannot be combined with newTerminal=true"
        );
        call.assert_async().await;
    }

    #[tokio::test]
    async fn execute_requires_internal_tool_call_id_before_transport() {
        let tool = ExecuteCommandTool::new(client("http://127.0.0.1:1".into(), "unused"));

        let error = match tool.execute(serde_json::json!({ "command": "pwd" })).await {
            Ok(_) => panic!("expected missing internal tool call id error"),
            Err(error) => error,
        };

        assert!(
            error.to_string().contains("missing field `_tool_call_id`"),
            "unexpected error: {error}"
        );
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
    fn execute_result_formats_finished_timed_out_and_background_states() {
        assert_eq!(
            format_execute_result(&response("done", true, false, false)),
            "Command finished in terminal (id: 7) with exit code 0.\nOutput:\ndone"
        );
        assert_eq!(
            format_execute_result(&response("partial", false, true, false)),
            "Command is still running in terminal (id: 7).\nOutput:\npartial"
        );
        assert_eq!(
            format_execute_result(&response("started", false, false, true)),
            "Command started in terminal (id: 7).\nOutput:\nstarted"
        );
    }

    #[test]
    fn execute_result_omits_output_section_when_empty() {
        assert_eq!(
            format_execute_result(&response("", true, false, false)),
            "Command finished in terminal (id: 7) with exit code 0."
        );
    }

    #[test]
    fn execute_result_preserves_finished_status_without_exit_code() {
        let mut response = response("", true, false, false);
        response.exit_code = None;

        assert_eq!(
            format_execute_result(&response),
            "Command finished in terminal (id: 7)."
        );
    }

    #[tokio::test]
    async fn agent_result_redacts_only_secret_environment_values() {
        let service = client("http://127.0.0.1:1".into(), "unused");
        service
            .set_env_provider(Arc::new(StaticEnvProvider {
                variables: vec![
                    InjectedEnvVar {
                        key: "SECRET_TOKEN".into(),
                        value: Secret::new("secret-value".into()),
                        secret: true,
                    },
                    InjectedEnvVar {
                        key: "PUBLIC_VALUE".into(),
                        value: Secret::new("public-value".into()),
                        secret: false,
                    },
                ],
            }))
            .unwrap_or_else(|error| panic!("environment provider setup failed: {error}"));
        let tool = ExecuteCommandTool::new(service);
        let raw_result =
            serde_json::to_value(response("secret-value public-value", true, false, false))
                .unwrap_or_else(|error| panic!("response encoding failed: {error}"));

        let result = tool
            .agent_result(&serde_json::json!({}), &raw_result)
            .await
            .unwrap_or_else(|error| panic!("agent result failed: {error}"));

        assert_eq!(
            result,
            "Command finished in terminal (id: 7) with exit code 0.\nOutput:\n[REDACTED] public-value"
        );
    }

    #[test]
    fn execute_schema_requires_only_command() {
        let schema = ExecuteCommandTool::new(client("http://127.0.0.1:1".into(), "unused"))
            .parameters_schema();

        assert_eq!(schema["required"], serde_json::json!(["command"]));
        assert!(schema["properties"].get("node").is_none());
        assert_eq!(schema["additionalProperties"], false);
    }

    #[test]
    fn read_terminal_output_schema_requires_terminal_id_only() {
        let schema = ReadTerminalOutputTool::new(client("http://127.0.0.1:1".into(), "unused"))
            .parameters_schema();

        assert_eq!(schema["required"], serde_json::json!(["terminalId"]));
        assert_eq!(schema["properties"]["terminalId"]["type"], "string");
        assert_eq!(schema["properties"]["maxLines"]["type"], "integer");
        assert_eq!(schema["additionalProperties"], false);
    }
}
