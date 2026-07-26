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
        "Read the contents of a file. Line numbers are 1-indexed. This tool limits each text read to 2000 lines and each binary hexadecimal dump to 512 bytes. Use exactly one mode: offset/limit or ranges; using both is invalid. In offset/limit mode, use a positive offset to start at a specific line, or use offset=-1 for tail mode where limit controls how many final lines to read. Other negative offsets and offset=0 are invalid. Binary files use offset and limit as byte ranges; offset=-1 reads the last limit bytes. In ranges mode, read multiple inclusive line ranges in one call, optionally include line numbers, number blank lines, and add range headers."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "additionalProperties": false,
            "required": ["filePath"],
            "properties": {
                "filePath": {
                    "type": "string",
                    "minLength": 1,
                    "description": "The absolute path of the file to read."
                },
                "offset": {
                    "type": "integer",
                    "description": "Optional: a positive 1-based line number to start reading from, or -1 for tail mode. In tail mode, limit controls how many final lines are returned; if limit is omitted, only the final line is returned. Binary files use byte offsets; offset=-1 reads the last limit bytes. Offset 0 and negative offsets other than -1 are invalid. Do not use with ranges. If not specified, the file will be read from the beginning."
                },
                "limit": {
                    "type": "integer",
                    "minimum": 1,
                    "description": "Optional: the maximum number of lines to read in offset/limit mode. With offset=-1, this is the number of final lines to return. Binary files use this as a byte count. Do not use with ranges."
                },
                "ranges": {
                    "type": "array",
                    "description": "Optional: multiple inclusive line ranges to read in one call. Use ranges mode only when both offset and limit are omitted; using ranges with offset or limit is invalid.",
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
                                "minimum": 1,
                                "description": "Optional inclusive 1-based end line for this range. If omitted, only startLine is read."
                            }
                        }
                    }
                },
                "includeLineNumbers": {
                    "type": "boolean",
                    "default": false,
                    "description": "Optional: when using ranges mode, include source line numbers in the result. Ignored when offset or limit is provided."
                },
                "numberBlankLines": {
                    "type": "boolean",
                    "default": false,
                    "description": "Optional: when using ranges mode with includeLineNumbers, also include line numbers for blank lines. Ignored when offset or limit is provided."
                },
                "includeRangeHeaders": {
                    "type": "boolean",
                    "default": false,
                    "description": "Optional: when using ranges mode, add text headers like '--- lines 10-20 ---' before each range block. Ignored when offset or limit is provided."
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
    use {super::*, crate::sandbox::ToolsServiceEndpoint};

    fn client(base_url: String, token: &str) -> Arc<ManagedToolsService> {
        ManagedToolsService::for_test(ToolsServiceEndpoint {
            base_url,
            token: token.into(),
        })
        .unwrap_or_else(|error| panic!("test client failed: {error}"))
    }

    #[test]
    fn exposes_name_schema_and_non_persistent_result_policy() {
        let tool = ReadFileTool::new(client("http://127.0.0.1:1".into(), "unused"));

        assert_eq!(tool.name(), "read_file");
        let schema = tool.parameters_schema();
        assert_eq!(schema["required"], json!(["filePath"]));
        assert_eq!(schema["additionalProperties"], false);
        assert_eq!(schema["properties"]["filePath"]["type"], "string");
        assert_eq!(
            schema["properties"]["ranges"]["items"]["required"],
            json!(["startLine"])
        );
        assert_eq!(tool.truncation(&json!({})), Truncation::Off);
        assert_eq!(
            tool.result_persistence(&json!({})),
            ToolResultPersistence::Off
        );
    }

    #[test]
    fn parse_input_rejects_null_unknown_and_invalid_public_fields() {
        for invalid in [
            json!({ "filePath": "/tmp/file", "offset": null }),
            json!({ "filePath": "/tmp/file", "obsolete": true }),
            json!({ "filePath": "/tmp/file", "offset": 0 }),
            json!({
                "filePath": "/tmp/file",
                "limit": 2,
                "ranges": [{ "startLine": 1 }]
            }),
        ] {
            assert!(parse_input(invalid).is_err());
        }
    }

    #[test]
    fn parse_input_strips_only_internal_context() {
        let input = parse_input(json!({
            "filePath": "/workspace/file.txt",
            "offset": -1,
            "limit": 2,
            "_session_key": "session:test",
            "_channel": { "surface": "web" }
        }))
        .unwrap_or_else(|error| panic!("parse failed: {error}"));

        assert_eq!(input.file_path, "/workspace/file.txt");
        assert_eq!(input.offset, Some(-1));
        assert_eq!(input.limit, Some(2));
    }

    #[tokio::test]
    async fn execute_routes_session_and_returns_plain_text() {
        let mut server = mockito::Server::new_async().await;
        let call = server
            .mock("POST", chelix_protocol::TOOLS_SERVICE_READ_FILE_PATH)
            .match_header("authorization", "Bearer read-token")
            .match_body(mockito::Matcher::Json(json!({
                "filePath": "/workspace/file.txt",
                "offset": 2,
                "limit": 2,
                "ranges": [],
                "includeLineNumbers": false,
                "numberBlankLines": false,
                "includeRangeHeaders": false
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
                "offset": 2,
                "limit": 2,
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
            .execute(json!({ "filePath": "/workspace/missing.txt" }))
            .await;
        let error = match result {
            Ok(_) => panic!("expected tools service failure"),
            Err(error) => error,
        };

        assert!(error.to_string().contains("failed to open file"));
        call.assert_async().await;
    }
}
