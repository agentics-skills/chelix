//! Wire types shared by the managed tools service and its client.

use std::fmt;

use serde::{Deserialize, Serialize};

pub const TOOLS_SERVICE_PROTOCOL_VERSION: u32 = 5;
pub const TOOLS_SERVICE_CONTAINER_PORT: u16 = 43_271;
pub const TOOLS_SERVICE_HEALTH_PATH: &str = "/v1/health";
pub const TOOLS_SERVICE_LIST_DIRECTORY_PATH: &str = "/v1/list-directory";
pub const TOOLS_SERVICE_RIPGREP_PATH: &str = "/v1/ripgrep";
pub const TOOLS_SERVICE_EXECUTE_COMMAND_PATH: &str = "/v1/execute-command";
pub const TOOLS_SERVICE_READ_TERMINAL_OUTPUT_PATH: &str = "/v1/read-terminal-output";
pub const TOOLS_SERVICE_PROCESS_PATH: &str = "/v1/process";
pub const TOOLS_SERVICE_TERMINALS_PATH: &str = "/v1/terminals";
pub const TOOLS_SERVICE_TERMINAL_WS_PATH: &str = "/v1/terminal-ws";
pub const TOOLS_SERVICE_AUTH_HEADER: &str = "authorization";
pub const TOOLS_SERVICE_TOKEN_ENV: &str = "CHELIX_TOOLS_SERVICE_TOKEN";
pub const TOOLS_SERVICE_BINARY_ENV: &str = "CHELIX_TOOLS_SERVICE_BINARY";
pub const TOOLS_SERVICE_LINUX_BINARY_ENV: &str = "CHELIX_TOOLS_SERVICE_LINUX_BINARY";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolsServiceReady {
    pub protocol_version: u32,
    pub port: u16,
    pub token: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolsServiceHealth {
    pub protocol_version: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ListDirectoryRequest {
    pub path: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ListDirectoryResponse {
    pub result: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RipgrepRequest {
    pub params: serde_json::Value,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RipgrepResponse {
    pub result: serde_json::Value,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolsServiceEnvVar {
    pub key: String,
    pub value: String,
    pub secret: bool,
}

impl fmt::Debug for ToolsServiceEnvVar {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ToolsServiceEnvVar")
            .field("key", &self.key)
            .field(
                "value",
                if self.secret {
                    &"[redacted]" as &dyn fmt::Debug
                } else {
                    &self.value as &dyn fmt::Debug
                },
            )
            .field("secret", &self.secret)
            .finish()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExecuteCommandRequest {
    pub session_key: String,
    pub command: String,
    pub custom_cwd: Option<String>,
    pub new_terminal: bool,
    pub background: bool,
    pub timeout_millis: u64,
    pub terminal_id: Option<String>,
    pub env: Vec<ToolsServiceEnvVar>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExecuteCommandResponse {
    pub terminal_id: String,
    pub run_id: String,
    pub session_id: String,
    pub session_name: String,
    pub window_id: String,
    pub window_name: String,
    pub pane_id: String,
    pub output: String,
    pub exit_code: Option<i32>,
    pub completed: bool,
    pub timed_out: bool,
    pub background: bool,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReadTerminalOutputRequest {
    pub session_key: String,
    pub terminal_id: String,
    pub max_lines: Option<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReadTerminalOutputResponse {
    pub terminal_id: String,
    pub session_id: String,
    pub session_name: String,
    pub window_id: String,
    pub window_name: String,
    pub pane_id: String,
    pub output: String,
    pub exit_code: Option<i32>,
    pub completed: bool,
    pub running: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolsServiceTerminalKind {
    Execute,
    Process,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolsServiceTerminalInfo {
    pub kind: ToolsServiceTerminalKind,
    pub id: String,
    pub session_key: String,
    pub session_id: String,
    pub session_name: String,
    pub window_id: String,
    pub window_name: String,
    pub pane_id: String,
    pub running: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolsServiceTerminalsResponse {
    pub terminals: Vec<ToolsServiceTerminalInfo>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolsServiceInstanceInfo {
    pub id: String,
    pub label: String,
    pub terminals: Vec<ToolsServiceTerminalInfo>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateToolsServiceTerminalRequest {
    pub session_key: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateToolsServiceTerminalResponse {
    pub terminal: ToolsServiceTerminalInfo,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolsServiceTerminalAttachQuery {
    pub kind: ToolsServiceTerminalKind,
    pub id: String,
    pub session_key: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ToolsServiceTerminalClientMessage {
    Input {
        data: String,
    },
    Resize {
        cols: u16,
        rows: u16,
    },
    Control {
        action: ToolsServiceTerminalControlAction,
    },
    Ping,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolsServiceTerminalControlAction {
    Restart,
    CtrlC,
    Clear,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum ProcessAction {
    Start {
        command: String,
        #[serde(default)]
        session_name: Option<String>,
    },
    Poll {
        session_name: String,
    },
    SendKeys {
        session_name: String,
        keys: String,
    },
    Paste {
        session_name: String,
        text: String,
    },
    Kill {
        session_name: String,
    },
    List,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProcessRequest {
    pub session_key: String,
    pub action: ProcessAction,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProcessResponse {
    pub success: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolsServiceError {
    pub error: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ready_message_round_trips() {
        let ready = ToolsServiceReady {
            protocol_version: TOOLS_SERVICE_PROTOCOL_VERSION,
            port: 31_337,
            token: "secret".into(),
        };
        let json = serde_json::to_string(&ready).unwrap_or_default();
        let decoded: ToolsServiceReady =
            serde_json::from_str(&json).unwrap_or_else(|error| panic!("decode failed: {error}"));

        assert_eq!(decoded, ready);
    }

    #[test]
    fn list_directory_messages_round_trip() {
        let request = ListDirectoryRequest {
            path: "/workspace".into(),
        };
        let request_json = serde_json::to_string(&request).unwrap_or_default();
        let decoded_request: ListDirectoryRequest = serde_json::from_str(&request_json)
            .unwrap_or_else(|error| panic!("request decode failed: {error}"));
        assert_eq!(decoded_request, request);

        let response = ListDirectoryResponse {
            result: "src/\nCargo.toml (1 line)".into(),
        };
        let response_json = serde_json::to_string(&response).unwrap_or_default();
        let decoded_response: ListDirectoryResponse = serde_json::from_str(&response_json)
            .unwrap_or_else(|error| panic!("response decode failed: {error}"));
        assert_eq!(decoded_response, response);
    }

    #[test]
    fn secret_environment_debug_is_redacted() {
        let variable = ToolsServiceEnvVar {
            key: "TOKEN".into(),
            value: "do-not-log".into(),
            secret: true,
        };

        let debug = format!("{variable:?}");

        assert!(debug.contains("[redacted]"));
        assert!(!debug.contains("do-not-log"));
    }

    #[test]
    fn terminal_and_process_messages_round_trip() {
        let request = ExecuteCommandRequest {
            session_key: "session:test".into(),
            command: "printf hello".into(),
            custom_cwd: Some("/workspace".into()),
            new_terminal: true,
            background: false,
            timeout_millis: 5_000,
            terminal_id: None,
            env: vec![ToolsServiceEnvVar {
                key: "MODE".into(),
                value: "test".into(),
                secret: false,
            }],
        };
        let json = serde_json::to_string(&request).unwrap_or_default();
        let decoded: ExecuteCommandRequest = serde_json::from_str(&json)
            .unwrap_or_else(|error| panic!("execute request decode failed: {error}"));
        assert_eq!(decoded, request);

        let process = ProcessRequest {
            session_key: "session:test".into(),
            action: ProcessAction::SendKeys {
                session_name: "repl".into(),
                keys: "C-c".into(),
            },
        };
        let json = serde_json::to_string(&process).unwrap_or_default();
        let decoded: ProcessRequest = serde_json::from_str(&json)
            .unwrap_or_else(|error| panic!("process request decode failed: {error}"));
        assert_eq!(decoded, process);

        let terminal = ToolsServiceTerminalInfo {
            kind: ToolsServiceTerminalKind::Execute,
            id: "terminal-id".into(),
            session_key: "session:test".into(),
            session_id: "$1".into(),
            session_name: "session-test".into(),
            window_id: "@2".into(),
            window_name: "bash".into(),
            pane_id: "%3".into(),
            running: true,
        };
        let json = serde_json::to_string(&terminal).unwrap_or_default();
        let decoded: ToolsServiceTerminalInfo = serde_json::from_str(&json)
            .unwrap_or_else(|error| panic!("terminal info decode failed: {error}"));
        assert_eq!(decoded, terminal);
    }
}
