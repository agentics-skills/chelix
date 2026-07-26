//! Wire types shared by the managed tools service and its client.

use std::fmt;

use serde::{Deserialize, Deserializer, Serialize};

pub const TOOLS_SERVICE_PROTOCOL_VERSION: u32 = 9;
pub const TOOLS_SERVICE_CONTAINER_PORT: u16 = 43_271;
pub const TOOLS_SERVICE_HEALTH_PATH: &str = "/v1/health";
pub const TOOLS_SERVICE_LIST_DIRECTORY_PATH: &str = "/v1/list-directory";
pub const TOOLS_SERVICE_READ_FILE_PATH: &str = "/v1/read-file";
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

pub const RIPGREP_DEFAULT_MAX_MATCHES: usize = 2000;
pub const RIPGREP_DEFAULT_MAX_FILES: usize = 200;
pub const RIPGREP_DEFAULT_MAX_OUTPUT_CHARS: usize = 200_000;
pub const RIPGREP_DEFAULT_TIMEOUT_MS: u64 = 10_000;

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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReadFileRange {
    pub start_line: i64,
    #[serde(
        default,
        deserialize_with = "deserialize_present_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub end_line: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReadFileRequest {
    pub file_path: String,
    #[serde(
        default,
        deserialize_with = "deserialize_present_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub offset: Option<i64>,
    #[serde(
        default,
        deserialize_with = "deserialize_present_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub limit: Option<i64>,
    #[serde(default)]
    pub ranges: Vec<ReadFileRange>,
    #[serde(default)]
    pub include_line_numbers: bool,
    #[serde(default)]
    pub number_blank_lines: bool,
    #[serde(default)]
    pub include_range_headers: bool,
}

impl ReadFileRequest {
    /// Validate constraints that cannot be represented by serde alone.
    pub fn validate(&self) -> Result<(), ReadFileRequestValidationError> {
        if self.file_path.trim().is_empty() {
            return Err(ReadFileRequestValidationError::EmptyFilePath);
        }
        if let Some(offset) = self.offset {
            if offset == 0 {
                return Err(ReadFileRequestValidationError::ZeroOffset);
            }
            if offset < -1 {
                return Err(ReadFileRequestValidationError::InvalidNegativeOffset);
            }
        }
        if self.limit.is_some_and(|limit| limit < 1) {
            return Err(ReadFileRequestValidationError::InvalidLimit);
        }
        if !self.ranges.is_empty() && (self.offset.is_some() || self.limit.is_some()) {
            return Err(ReadFileRequestValidationError::MixedReadModes);
        }
        for (index, range) in self.ranges.iter().enumerate() {
            if range.start_line < 1 {
                return Err(ReadFileRequestValidationError::InvalidRangeStart(index));
            }
            if range.end_line.is_some_and(|end_line| end_line < 1) {
                return Err(ReadFileRequestValidationError::InvalidRangeEnd(index));
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ReadFileRequestValidationError {
    #[error("filePath must be a non-empty string.")]
    EmptyFilePath,
    #[error(
        "offset must not be 0. Omit offset to read from the beginning, use a positive offset to read from a 1-indexed line or byte, or use -1 for tail mode."
    )]
    ZeroOffset,
    #[error(
        "offset must be a positive integer or -1 for tail mode. Other negative offsets are not supported."
    )]
    InvalidNegativeOffset,
    #[error("limit must be a positive integer.")]
    InvalidLimit,
    #[error("Use either offset/limit or ranges, not both.")]
    MixedReadModes,
    #[error("ranges[{0}].startLine must be a positive integer.")]
    InvalidRangeStart(usize),
    #[error("ranges[{0}].endLine must be a positive integer.")]
    InvalidRangeEnd(usize),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReadFileResponse {
    pub result: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RipgrepCaseMode {
    Sensitive,
    Ignore,
    Smart,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum RipgrepDetail {
    #[serde(rename = "summary")]
    Summary,
    #[serde(rename = "files")]
    Files,
    #[default]
    #[serde(rename = "lines")]
    Lines,
    #[serde(rename = "lines+submatches")]
    LinesSubmatches,
}

fn ripgrep_default_max_matches() -> usize {
    RIPGREP_DEFAULT_MAX_MATCHES
}

fn ripgrep_default_max_files() -> usize {
    RIPGREP_DEFAULT_MAX_FILES
}

fn ripgrep_default_max_output_chars() -> usize {
    RIPGREP_DEFAULT_MAX_OUTPUT_CHARS
}

fn ripgrep_default_timeout_ms() -> u64 {
    RIPGREP_DEFAULT_TIMEOUT_MS
}

fn default_true() -> bool {
    true
}

fn ripgrep_default_unrestricted() -> u8 {
    3
}

fn deserialize_present_option<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    T::deserialize(deserializer).map(Some)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RipgrepInput {
    pub pattern: String,
    #[serde(default)]
    pub paths: Vec<String>,
    #[serde(
        default,
        deserialize_with = "deserialize_present_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub cwd: Option<String>,
    #[serde(default)]
    pub fixed_strings: bool,
    #[serde(
        default,
        deserialize_with = "deserialize_present_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub case_mode: Option<RipgrepCaseMode>,
    #[serde(default)]
    pub detail: RipgrepDetail,
    #[serde(default)]
    pub glob: Vec<String>,
    #[serde(default, rename = "type")]
    pub include_types: Vec<String>,
    #[serde(default)]
    pub type_not: Vec<String>,
    #[serde(
        default,
        deserialize_with = "deserialize_present_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub context_lines: Option<u64>,
    #[serde(default = "ripgrep_default_max_matches")]
    pub max_matches: usize,
    #[serde(default = "ripgrep_default_max_files")]
    pub max_files: usize,
    #[serde(default = "ripgrep_default_max_output_chars")]
    pub max_output_chars: usize,
    #[serde(default = "ripgrep_default_timeout_ms")]
    pub timeout_ms: u64,
    #[serde(default = "default_true")]
    pub include_hidden: bool,
    #[serde(default = "ripgrep_default_unrestricted")]
    pub unrestricted: u8,
    #[serde(default)]
    pub follow_symlinks: bool,
}

impl RipgrepInput {
    /// Validate numeric and semantic constraints not expressible in serde.
    pub fn validate(&self) -> Result<(), RipgrepInputValidationError> {
        if self.pattern.is_empty() {
            return Err(RipgrepInputValidationError::EmptyPattern);
        }
        if self.max_matches == 0 {
            return Err(RipgrepInputValidationError::ZeroMaxMatches);
        }
        if self.max_files == 0 {
            return Err(RipgrepInputValidationError::ZeroMaxFiles);
        }
        if self.max_output_chars == 0 {
            return Err(RipgrepInputValidationError::ZeroMaxOutputChars);
        }
        if self.unrestricted > 3 {
            return Err(RipgrepInputValidationError::InvalidUnrestricted(
                self.unrestricted,
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum RipgrepInputValidationError {
    #[error("'pattern' must not be empty")]
    EmptyPattern,
    #[error("'maxMatches' must be at least 1")]
    ZeroMaxMatches,
    #[error("'maxFiles' must be at least 1")]
    ZeroMaxFiles,
    #[error("'maxOutputChars' must be at least 1")]
    ZeroMaxOutputChars,
    #[error("'unrestricted' must be between 0 and 3, got {0}")]
    InvalidUnrestricted(u8),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RipgrepRequest {
    pub params: RipgrepInput,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RipgrepResponse {
    pub result: RipgrepResult,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RipgrepSubmatch {
    #[serde(rename = "match")]
    pub matched: String,
    pub start: u64,
    pub end: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RipgrepMatch {
    pub path: String,
    pub line_number: u64,
    pub lines: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub submatches: Option<Vec<RipgrepSubmatch>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RipgrepContextLine {
    pub path: String,
    pub line_number: u64,
    pub lines: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RipgrepLimits {
    pub max_matches: usize,
    pub max_files: usize,
    pub max_output_chars: usize,
    pub timeout_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RipgrepSummary {
    pub files_with_matches: usize,
    pub match_count: usize,
    pub elapsed: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stats: Option<serde_json::Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RipgrepResult {
    pub tool: String,
    pub detail: RipgrepDetail,
    pub found: bool,
    pub timed_out: bool,
    pub truncated: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub truncated_reason: Option<String>,
    pub limits: RipgrepLimits,
    pub summary: RipgrepSummary,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub files: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub matches: Option<Vec<RipgrepMatch>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context: Option<Vec<RipgrepContextLine>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stderr: Option<String>,
    pub exit_code: Option<i32>,
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
    fn read_file_messages_round_trip_with_camel_case_fields() {
        let request = ReadFileRequest {
            file_path: "/workspace/src/main.rs".into(),
            offset: None,
            limit: None,
            ranges: vec![ReadFileRange {
                start_line: 12,
                end_line: Some(20),
            }],
            include_line_numbers: true,
            number_blank_lines: false,
            include_range_headers: true,
        };
        let json = serde_json::to_value(&request)
            .unwrap_or_else(|error| panic!("read file request encode failed: {error}"));
        assert_eq!(
            json,
            serde_json::json!({
                "filePath": "/workspace/src/main.rs",
                "ranges": [{ "startLine": 12, "endLine": 20 }],
                "includeLineNumbers": true,
                "numberBlankLines": false,
                "includeRangeHeaders": true
            })
        );
        let decoded: ReadFileRequest = serde_json::from_value(json)
            .unwrap_or_else(|error| panic!("read file request decode failed: {error}"));
        assert_eq!(decoded, request);
        assert!(decoded.validate().is_ok());

        let response = ReadFileResponse {
            result: "12\tfn main() {}".into(),
        };
        let json = serde_json::to_string(&response).unwrap_or_default();
        let decoded: ReadFileResponse = serde_json::from_str(&json)
            .unwrap_or_else(|error| panic!("read file response decode failed: {error}"));
        assert_eq!(decoded, response);
    }

    #[test]
    fn read_file_request_rejects_null_unknown_and_invalid_values() {
        for invalid in [
            serde_json::json!({ "filePath": "/tmp/file", "offset": null }),
            serde_json::json!({ "filePath": "/tmp/file", "limit": null }),
            serde_json::json!({ "filePath": "/tmp/file", "ranges": null }),
            serde_json::json!({
                "filePath": "/tmp/file",
                "ranges": [{ "startLine": 1, "endLine": null }]
            }),
            serde_json::json!({ "filePath": "/tmp/file", "obsolete": true }),
        ] {
            assert!(serde_json::from_value::<ReadFileRequest>(invalid).is_err());
        }

        let invalid = [
            (
                serde_json::json!({ "filePath": " " }),
                "filePath must be a non-empty string.",
            ),
            (
                serde_json::json!({ "filePath": "/tmp/file", "offset": 0 }),
                "offset must not be 0.",
            ),
            (
                serde_json::json!({ "filePath": "/tmp/file", "offset": -2 }),
                "offset must be a positive integer or -1",
            ),
            (
                serde_json::json!({ "filePath": "/tmp/file", "limit": 0 }),
                "limit must be a positive integer.",
            ),
            (
                serde_json::json!({
                    "filePath": "/tmp/file",
                    "offset": 1,
                    "ranges": [{ "startLine": 1 }]
                }),
                "Use either offset/limit or ranges, not both.",
            ),
            (
                serde_json::json!({
                    "filePath": "/tmp/file",
                    "ranges": [{ "startLine": 0 }]
                }),
                "ranges[0].startLine must be a positive integer.",
            ),
        ];
        for (value, expected) in invalid {
            let request: ReadFileRequest = serde_json::from_value(value)
                .unwrap_or_else(|error| panic!("read file request decode failed: {error}"));
            let error = match request.validate() {
                Ok(()) => panic!("request should fail validation"),
                Err(error) => error,
            };
            assert!(error.to_string().contains(expected));
        }
    }

    #[test]
    fn ripgrep_input_applies_explicit_defaults() {
        let input: RipgrepInput = serde_json::from_value(serde_json::json!({
            "pattern": "needle"
        }))
        .unwrap_or_else(|error| panic!("ripgrep input decode failed: {error}"));

        assert!(input.paths.is_empty());
        assert!(input.cwd.is_none());
        assert_eq!(input.detail, RipgrepDetail::Lines);
        assert_eq!(input.max_matches, RIPGREP_DEFAULT_MAX_MATCHES);
        assert_eq!(input.max_files, RIPGREP_DEFAULT_MAX_FILES);
        assert_eq!(input.max_output_chars, RIPGREP_DEFAULT_MAX_OUTPUT_CHARS);
        assert_eq!(input.timeout_ms, RIPGREP_DEFAULT_TIMEOUT_MS);
        assert!(input.include_hidden);
        assert_eq!(input.unrestricted, 3);
        assert!(input.validate().is_ok());
    }

    #[test]
    fn ripgrep_input_rejects_null_and_unknown_fields() {
        for invalid in [
            serde_json::json!({ "pattern": "needle", "cwd": null }),
            serde_json::json!({ "pattern": "needle", "paths": null }),
            serde_json::json!({ "pattern": "needle", "maxMatches": null }),
            serde_json::json!({ "pattern": "needle", "obsolete": true }),
        ] {
            assert!(serde_json::from_value::<RipgrepInput>(invalid).is_err());
        }
    }

    #[test]
    fn ripgrep_input_validation_rejects_out_of_range_values() {
        let cases = [
            (
                serde_json::json!({ "pattern": "" }),
                "'pattern' must not be empty",
            ),
            (
                serde_json::json!({ "pattern": "x", "maxMatches": 0 }),
                "'maxMatches' must be at least 1",
            ),
            (
                serde_json::json!({ "pattern": "x", "maxFiles": 0 }),
                "'maxFiles' must be at least 1",
            ),
            (
                serde_json::json!({ "pattern": "x", "maxOutputChars": 0 }),
                "'maxOutputChars' must be at least 1",
            ),
            (
                serde_json::json!({ "pattern": "x", "unrestricted": 4 }),
                "'unrestricted' must be between 0 and 3, got 4",
            ),
        ];

        for (value, expected) in cases {
            let input: RipgrepInput = serde_json::from_value(value)
                .unwrap_or_else(|error| panic!("ripgrep input decode failed: {error}"));
            let error = match input.validate() {
                Ok(()) => panic!("expected validation error"),
                Err(error) => error,
            };
            assert_eq!(error.to_string(), expected);
        }
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
