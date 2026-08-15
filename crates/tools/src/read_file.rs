//! `read_file` agent tool backed exclusively by the managed tools service.

use std::sync::Arc;

use {
    async_trait::async_trait,
    chelix_agents::tool_registry::{AgentTool, ToolResultPersistence, Truncation},
    chelix_protocol::{ReadFileRequest, ReadFileResponse},
    serde_json::{Value, json},
};

#[cfg(feature = "metrics")]
use chelix_metrics::{counter, labels, tools as tools_metrics};

use crate::tools_service::ManagedToolsService;

pub struct ReadFileTool {
    service: Arc<ManagedToolsService>,
}

impl ReadFileTool {
    #[must_use]
    pub fn new(service: Arc<ManagedToolsService>) -> Self {
        Self { service }
    }
}

#[async_trait]
impl AgentTool for ReadFileTool {
    fn name(&self) -> &str {
        "read_file"
    }

    fn description(&self) -> &str {
        "Read a file by offset and limit or by inclusive text line ranges. The path must be absolute. Positive offsets and line numbers are 1-indexed; offset=-1 selects tail mode. limit=-1 or endLine=-1 reads to the end of the file. Bounded offset/limit text reads return at most 2000 lines, and binary reads return at most 512 bytes."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "additionalProperties": false,
            "required": ["filePath", "read"],
            "properties": {
                "filePath": {
                    "type": "string",
                    "minLength": 1,
                    "description": "The absolute path of the file to read."
                },
                "read": {
                    "description": "The read operation.",
                    "oneOf": [
                        {
                            "type": "object",
                            "additionalProperties": false,
                            "required": ["offset", "limit"],
                            "properties": {
                                "offset": {
                                    "type": "integer",
                                    "oneOf": [
                                        { "const": -1 },
                                        { "minimum": 1 }
                                    ],
                                    "description": "A positive 1-based line or byte position, or -1 for tail mode. Use 1 to read from the beginning."
                                },
                                "limit": {
                                    "type": "integer",
                                    "oneOf": [
                                        { "const": -1 },
                                        { "minimum": 1 }
                                    ],
                                    "description": "The maximum number of lines or bytes to read. In tail mode, the number of final lines or bytes to return. Use -1 to read to the end of the file."
                                }
                            }
                        },
                        {
                            "type": "object",
                            "additionalProperties": false,
                            "required": ["ranges"],
                            "properties": {
                                "ranges": {
                                    "type": "array",
                                    "minItems": 1,
                                    "description": "Inclusive text line ranges.",
                                    "items": {
                                        "type": "object",
                                        "additionalProperties": false,
                                        "required": ["startLine"],
                                        "properties": {
                                            "startLine": {
                                                "type": "integer",
                                                "minimum": 1,
                                                "description": "The inclusive 1-based start line for this range."
                                            },
                                            "endLine": {
                                                "type": "integer",
                                                "oneOf": [
                                                    { "const": -1 },
                                                    { "minimum": 1 }
                                                ],
                                                "description": "Optional inclusive 1-based end line for this range. If omitted, only startLine is read. Use -1 to read to the last line of the file."
                                            }
                                        }
                                    }
                                },
                                "includeRangeHeaders": {
                                    "type": "boolean",
                                    "default": false,
                                    "description": "Whether to add a header like '--- lines 10-20 ---' before each range."
                                }
                            }
                        }
                    ]
                },
                "includeLineNumbers": {
                    "type": "boolean",
                    "default": false,
                    "description": "Whether to include source line numbers in the result."
                },
                "numberBlankLines": {
                    "type": "boolean",
                    "default": false,
                    "description": "Whether blank lines receive line numbers when includeLineNumbers is true."
                }
            }
        })
    }

    fn validate(&self, params: &Value) -> anyhow::Result<()> {
        parse_input(params.clone()).map(|_| ())
    }

    fn truncation(&self, _params: &Value) -> Truncation {
        Truncation::Off
    }

    fn result_persistence(&self, _params: &Value) -> ToolResultPersistence {
        ToolResultPersistence::Off
    }

    async fn execute(&self, params: Value) -> anyhow::Result<Value> {
        let session_key = params
            .get("_session_key")
            .and_then(Value::as_str)
            .unwrap_or("main")
            .to_string();
        let input = parse_input(params)?;
        let result = self.service.read_file(&session_key, input).await;
        record_metrics(&result);
        Ok(Value::String(result?.result))
    }
}

fn parse_input(mut params: Value) -> anyhow::Result<ReadFileRequest> {
    let map = params
        .as_object_mut()
        .ok_or_else(|| anyhow::anyhow!("read_file parameters must be an object"))?;
    map.retain(|key, _| !key.starts_with('_'));
    let input: ReadFileRequest = serde_json::from_value(params)
        .map_err(|error| anyhow::anyhow!("invalid read_file parameters: {error}"))?;
    input
        .validate()
        .map_err(|error| anyhow::anyhow!("invalid read_file parameters: {error}"))?;
    Ok(input)
}

#[cfg(feature = "metrics")]
fn record_metrics(result: &crate::Result<ReadFileResponse>) {
    match result {
        Ok(_) => {
            counter!(
                tools_metrics::EXECUTIONS_TOTAL,
                labels::TOOL => "read_file".to_string(),
                labels::SUCCESS => "true".to_string()
            )
            .increment(1);
        },
        Err(_) => {
            counter!(
                tools_metrics::EXECUTION_ERRORS_TOTAL,
                labels::TOOL => "read_file".to_string()
            )
            .increment(1);
        },
    }
}

#[cfg(not(feature = "metrics"))]
fn record_metrics(_result: &crate::Result<ReadFileResponse>) {}

#[cfg(test)]
mod tests {
    use {
        super::*,
        crate::sandbox::ToolsServiceEndpoint,
        chelix_protocol::{
            ReadFileOffsetLimitOperation, ReadFileOperation, ReadFileRange, ReadFileRangesOperation,
        },
    };

    fn client(base_url: String, token: &str) -> Arc<ManagedToolsService> {
        ManagedToolsService::for_test(ToolsServiceEndpoint {
            base_url,
            token: token.into(),
        })
        .unwrap_or_else(|error| panic!("test client failed: {error}"))
    }

    #[test]
    fn exposes_strict_nested_read_schema_and_non_persistent_result_policy() {
        let tool = ReadFileTool::new(client("http://127.0.0.1:1".into(), "unused"));

        assert_eq!(tool.name(), "read_file");
        let schema = tool.parameters_schema();
        assert_eq!(schema["required"], json!(["filePath", "read"]));
        assert_eq!(schema["additionalProperties"], false);
        assert_eq!(schema["properties"]["filePath"]["type"], "string");
        assert_eq!(
            schema["properties"]["includeLineNumbers"]["type"],
            "boolean"
        );
        assert_eq!(schema["properties"]["numberBlankLines"]["type"], "boolean");

        let variants = schema["properties"]["read"]["oneOf"]
            .as_array()
            .unwrap_or_else(|| panic!("read must expose oneOf variants"));
        assert_eq!(variants.len(), 2);
        assert_eq!(variants[0]["required"], json!(["offset", "limit"]));
        assert_eq!(variants[0]["additionalProperties"], false);
        assert_eq!(variants[1]["required"], json!(["ranges"]));
        assert_eq!(variants[1]["additionalProperties"], false);
        assert_eq!(variants[1]["properties"]["ranges"]["minItems"], 1);
        assert_eq!(
            variants[1]["properties"]["ranges"]["items"]["required"],
            json!(["startLine"])
        );
        assert_eq!(tool.truncation(&json!({})), Truncation::Off);
        assert_eq!(
            tool.result_persistence(&json!({})),
            ToolResultPersistence::Off
        );
    }

    #[test]
    fn parse_input_rejects_missing_empty_malformed_and_mixed_read_forms() {
        for invalid in [
            json!({ "filePath": "/tmp/file" }),
            json!({ "filePath": "/tmp/file", "read": null }),
            json!({ "filePath": "/tmp/file", "read": "" }),
            json!({ "filePath": "/tmp/file", "read": {} }),
            json!({ "filePath": "/tmp/file", "read": { "offset": 1 } }),
            json!({ "filePath": "/tmp/file", "read": { "limit": 2 } }),
            json!({
                "filePath": "/tmp/file",
                "read": { "offset": 1, "limit": 2, "ranges": [{ "startLine": 1 }] }
            }),
            json!({ "filePath": "/tmp/file", "read": { "ranges": [] } }),
            json!({ "filePath": "/tmp/file", "offset": 1, "limit": 2 }),
            json!({
                "filePath": "/tmp/file",
                "read": { "offset": 0, "limit": 2 }
            }),
            json!({
                "filePath": "/tmp/file",
                "read": { "offset": 1, "limit": -2 }
            }),
            json!({
                "filePath": "/tmp/file",
                "read": { "ranges": [{ "startLine": 1, "endLine": -2 }] }
            }),
            json!({
                "filePath": "/tmp/file",
                "read": { "offset": 1, "limit": 2 },
                "obsolete": true
            }),
        ] {
            assert!(parse_input(invalid).is_err());
        }
    }

    #[test]
    fn parse_input_accepts_end_of_file_markers() {
        let unbounded = parse_input(json!({
            "filePath": "/workspace/file.txt",
            "read": { "offset": 3, "limit": -1 }
        }))
        .unwrap_or_else(|error| panic!("parse failed: {error}"));
        assert_eq!(
            unbounded.read,
            ReadFileOperation::OffsetLimit(ReadFileOffsetLimitOperation {
                offset: 3,
                limit: -1,
            })
        );

        let tail = parse_input(json!({
            "filePath": "/workspace/file.txt",
            "read": { "offset": -1, "limit": -1 }
        }))
        .unwrap_or_else(|error| panic!("parse failed: {error}"));
        assert_eq!(
            tail.read,
            ReadFileOperation::OffsetLimit(ReadFileOffsetLimitOperation {
                offset: -1,
                limit: -1,
            })
        );

        let ranges = parse_input(json!({
            "filePath": "/workspace/file.txt",
            "read": { "ranges": [{ "startLine": 12, "endLine": -1 }] }
        }))
        .unwrap_or_else(|error| panic!("parse failed: {error}"));
        assert_eq!(
            ranges.read,
            ReadFileOperation::Ranges(ReadFileRangesOperation {
                ranges: vec![ReadFileRange {
                    start_line: 12,
                    end_line: Some(-1),
                }],
                include_range_headers: false,
            })
        );
    }

    #[test]
    fn parse_input_strips_only_internal_context() {
        let input = parse_input(json!({
            "filePath": "/workspace/file.txt",
            "read": {
                "offset": -1,
                "limit": 2
            },
            "_session_key": "session:test",
            "_channel": { "surface": "web" }
        }))
        .unwrap_or_else(|error| panic!("parse failed: {error}"));

        assert_eq!(input.file_path, "/workspace/file.txt");
        assert!(!input.include_line_numbers);
        assert!(!input.number_blank_lines);
        assert_eq!(
            serde_json::to_value(input.read)
                .unwrap_or_else(|error| panic!("read operation encode failed: {error}")),
            json!({ "offset": -1, "limit": 2 })
        );
    }

    #[tokio::test]
    async fn execute_routes_session_and_returns_plain_text() {
        let mut server = mockito::Server::new_async().await;
        let call = server
            .mock("POST", chelix_protocol::TOOLS_SERVICE_READ_FILE_PATH)
            .match_header("authorization", "Bearer read-token")
            .match_body(mockito::Matcher::Json(json!({
                "filePath": "/workspace/file.txt",
                "read": {
                    "offset": 2,
                    "limit": 2
                },
                "includeLineNumbers": false,
                "numberBlankLines": false
            })))
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body("{\"result\":\"line 2\\nline 3\"}")
            .expect(1)
            .create_async()
            .await;
        let tool = ReadFileTool::new(client(server.url(), "read-token"));

        let result = tool
            .execute(json!({
                "filePath": "/workspace/file.txt",
                "read": {
                    "offset": 2,
                    "limit": 2
                },
                "_session_key": "session:test"
            }))
            .await
            .unwrap_or_else(|error| panic!("execute failed: {error}"));

        assert_eq!(result, Value::String("line 2\nline 3".into()));
        call.assert_async().await;
    }

    #[tokio::test]
    async fn execute_surfaces_service_failure() {
        let mut server = mockito::Server::new_async().await;
        let call = server
            .mock("POST", chelix_protocol::TOOLS_SERVICE_READ_FILE_PATH)
            .with_status(422)
            .with_header("content-type", "application/json")
            .with_body("{\"error\":\"failed to open file\"}")
            .expect(1)
            .create_async()
            .await;
        let result = ReadFileTool::new(client(server.url(), "test-token"))
            .execute(json!({
                "filePath": "/workspace/missing.txt",
                "read": { "offset": 1, "limit": 2000 }
            }))
            .await;
        let error = match result {
            Ok(_) => panic!("expected tools service failure"),
            Err(error) => error,
        };

        assert!(error.to_string().contains("failed to open file"));
        call.assert_async().await;
    }
}
