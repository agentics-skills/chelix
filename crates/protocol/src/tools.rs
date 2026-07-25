//! Wire types shared by the managed tools service and its client.

use std::fmt;

use serde::{Deserialize, Serialize};

pub const TOOLS_SERVICE_PROTOCOL_VERSION: u32 = 7;
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
    pub output: String,
    pub exit_code: Option<i32>,
    pub completed: bool,
    pub alive: bool,
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
    pub output: String,
    pub exit_code: Option<i32>,
    pub completed: bool,
    pub running: bool,
    pub alive: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolsServiceTerminalInfo {
    pub id: String,
    pub session_key: String,
    pub running: bool,
    pub alive: bool,
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
    pub env: Vec<ToolsServiceEnvVar>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateToolsServiceTerminalResponse {
    pub terminal: ToolsServiceTerminalInfo,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolsServiceTerminalAttachQuery {
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
    CtrlC,
    Clear,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum ProcessAction {
    SendKeys {
        #[serde(rename = "terminalId")]
        terminal_id: String,
        keys: String,
    },
    Paste {
        #[serde(rename = "terminalId")]
        terminal_id: String,
        text: String,
    },
    Kill {
        #[serde(rename = "terminalId")]
        terminal_id: String,
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
#[serde(tag = "action", rename_all = "snake_case")]
pub enum ProcessResponse {
    SendKeys {
        #[serde(rename = "terminalId")]
        terminal_id: String,
    },
    Paste {
        #[serde(rename = "terminalId")]
        terminal_id: String,
    },
    Kill {
        #[serde(rename = "terminalId")]
        terminal_id: String,
    },
    List {
        #[serde(rename = "terminalIds")]
        terminal_ids: Vec<String>,
    },
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
    fn execute_command_messages_use_camel_case_wire_fields() {
        let request = ExecuteCommandRequest {
            session_key: "session:test".into(),
            command: "printf hello".into(),
            custom_cwd: Some("/workspace".into()),
            new_terminal: false,
            background: false,
            timeout_millis: 5_000,
            terminal_id: Some("3".into()),
            env: vec![ToolsServiceEnvVar {
                key: "TOKEN".into(),
                value: "secret-value".into(),
                secret: true,
            }],
        };
        let json = serde_json::to_value(&request)
            .unwrap_or_else(|error| panic!("execute request encode failed: {error}"));
        assert_eq!(
            json,
            serde_json::json!({
                "sessionKey": "session:test",
                "command": "printf hello",
                "customCwd": "/workspace",
                "newTerminal": false,
                "background": false,
                "timeoutMillis": 5_000,
                "terminalId": "3",
                "env": [{
                    "key": "TOKEN",
                    "value": "secret-value",
                    "secret": true
                }]
            })
        );
        let decoded: ExecuteCommandRequest = serde_json::from_value(json)
            .unwrap_or_else(|error| panic!("execute request decode failed: {error}"));
        assert_eq!(decoded, request);

        let response = ExecuteCommandResponse {
            terminal_id: "3".into(),
            run_id: "run-1".into(),
            output: "hello".into(),
            exit_code: Some(0),
            completed: true,
            alive: true,
            timed_out: false,
            background: false,
            message: "done".into(),
        };
        let json = serde_json::to_value(&response)
            .unwrap_or_else(|error| panic!("execute response encode failed: {error}"));
        assert_eq!(json["terminalId"], "3");
        assert_eq!(json["runId"], "run-1");
        assert_eq!(json["exitCode"], 0);
        assert!(json.get("terminal_id").is_none());
        let decoded: ExecuteCommandResponse = serde_json::from_value(json)
            .unwrap_or_else(|error| panic!("execute response decode failed: {error}"));
        assert_eq!(decoded, response);
    }

    #[test]
    fn read_terminal_output_messages_use_string_terminal_id() {
        let request = ReadTerminalOutputRequest {
            session_key: "session:test".into(),
            terminal_id: "3".into(),
            max_lines: Some(250),
        };
        let json = serde_json::to_value(&request)
            .unwrap_or_else(|error| panic!("read request encode failed: {error}"));
        assert_eq!(
            json,
            serde_json::json!({
                "sessionKey": "session:test",
                "terminalId": "3",
                "maxLines": 250
            })
        );
        let decoded: ReadTerminalOutputRequest = serde_json::from_value(json)
            .unwrap_or_else(|error| panic!("read request decode failed: {error}"));
        assert_eq!(decoded, request);

        let response = ReadTerminalOutputResponse {
            terminal_id: "3".into(),
            output: "hello".into(),
            exit_code: Some(0),
            completed: true,
            running: false,
            alive: true,
        };
        let json = serde_json::to_value(&response)
            .unwrap_or_else(|error| panic!("read response encode failed: {error}"));
        assert_eq!(json["terminalId"], "3");
        assert_eq!(json["exitCode"], 0);
        assert!(json.get("terminal_id").is_none());
        let decoded: ReadTerminalOutputResponse = serde_json::from_value(json)
            .unwrap_or_else(|error| panic!("read response decode failed: {error}"));
        assert_eq!(decoded, response);
    }

    #[test]
    fn terminal_and_process_messages_round_trip() {
        let process = ProcessRequest {
            session_key: "session:test".into(),
            action: ProcessAction::SendKeys {
                terminal_id: "3".into(),
                keys: "C-c".into(),
            },
        };
        let json = serde_json::to_string(&process).unwrap_or_default();
        let decoded: ProcessRequest = serde_json::from_str(&json)
            .unwrap_or_else(|error| panic!("process request decode failed: {error}"));
        assert_eq!(decoded, process);

        let terminal = ToolsServiceTerminalInfo {
            id: "terminal-id".into(),
            session_key: "session:test".into(),
            running: true,
            alive: true,
        };
        let json = serde_json::to_string(&terminal).unwrap_or_default();
        let decoded: ToolsServiceTerminalInfo = serde_json::from_str(&json)
            .unwrap_or_else(|error| panic!("terminal info decode failed: {error}"));
        assert_eq!(decoded, terminal);
    }
}
