//! `overwrite_file` agent tool backed exclusively by the managed tools service.

use std::sync::Arc;

use {
    async_trait::async_trait,
    chelix_agents::tool_registry::AgentTool,
    chelix_protocol::{OverwriteFileRequest, OverwriteFileResponse},
    serde_json::{Value, json},
};

#[cfg(feature = "metrics")]
use chelix_metrics::{counter, labels, tools as tools_metrics};

use crate::tools_service::ManagedToolsService;

pub struct OverwriteFileTool {
    service: Arc<ManagedToolsService>,
}

impl OverwriteFileTool {
    #[must_use]
    pub fn new(service: Arc<ManagedToolsService>) -> Self {
        Self { service }
    }
}

#[async_trait]
impl AgentTool for OverwriteFileTool {
    fn name(&self) -> &str {
        "overwrite_file"
    }

    fn description(&self) -> &str {
        "Create a file or atomically replace its complete contents. The path must be absolute and its parent directory must already exist. Symbolic-link targets are rejected."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "additionalProperties": false,
            "required": ["filePath", "content"],
            "properties": {
                "filePath": {
                    "type": "string",
                    "minLength": 1,
                    "description": "The absolute path of the file to create or overwrite."
                },
                "content": {
                    "type": "string",
                    "description": "The complete UTF-8 file contents. An empty string truncates the file."
                }
            }
        })
    }

    fn validate(&self, params: &Value) -> anyhow::Result<()> {
        parse_input(params.clone()).map(|_| ())
    }

    async fn execute(&self, params: Value) -> anyhow::Result<Value> {
        let session_key = params
            .get("_session_key")
            .and_then(Value::as_str)
            .unwrap_or("main")
            .to_string();
        let request = parse_input(params)?;
        let result = self.service.overwrite_file(&session_key, request).await;
        record_metrics(&result);
        Ok(serde_json::to_value(result?)?)
    }
}

fn parse_input(mut params: Value) -> anyhow::Result<OverwriteFileRequest> {
    let map = params
        .as_object_mut()
        .ok_or_else(|| anyhow::anyhow!("overwrite_file parameters must be an object"))?;
    map.retain(|key, _| !key.starts_with('_'));
    let request: OverwriteFileRequest = serde_json::from_value(params)
        .map_err(|error| anyhow::anyhow!("invalid overwrite_file parameters: {error}"))?;
    request
        .validate()
        .map_err(|error| anyhow::anyhow!("invalid overwrite_file parameters: {error}"))?;
    Ok(request)
}

#[cfg(feature = "metrics")]
fn record_metrics(result: &crate::Result<OverwriteFileResponse>) {
    match result {
        Ok(_) => {
            counter!(
                tools_metrics::EXECUTIONS_TOTAL,
                labels::TOOL => "overwrite_file".to_string(),
                labels::SUCCESS => "true".to_string()
            )
            .increment(1);
        },
        Err(_) => {
            counter!(
                tools_metrics::EXECUTION_ERRORS_TOTAL,
                labels::TOOL => "overwrite_file".to_string()
            )
            .increment(1);
        },
    }
}

#[cfg(not(feature = "metrics"))]
fn record_metrics(_result: &crate::Result<OverwriteFileResponse>) {}

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
    fn exposes_name_and_strict_schema() {
        let tool = OverwriteFileTool::new(client("http://127.0.0.1:1".into(), "unused"));

        assert_eq!(tool.name(), "overwrite_file");
        let schema = tool.parameters_schema();
        assert_eq!(schema["required"], json!(["filePath", "content"]));
        assert_eq!(schema["additionalProperties"], false);
        assert_eq!(schema["properties"]["filePath"]["type"], "string");
        assert_eq!(schema["properties"]["content"]["type"], "string");
    }

    #[test]
    fn parse_input_rejects_null_missing_unknown_and_empty_fields() {
        for invalid in [
            json!({ "filePath": "/tmp/file", "content": null }),
            json!({ "filePath": "/tmp/file" }),
            json!({ "filePath": " ", "content": "value" }),
            json!({
                "filePath": "/tmp/file",
                "content": "value",
                "obsolete": true
            }),
        ] {
            assert!(parse_input(invalid).is_err());
        }
    }

    #[test]
    fn parse_input_strips_only_internal_context() {
        let request = parse_input(json!({
            "filePath": "/workspace/file.txt",
            "content": "value",
            "_session_key": "session:test",
            "_channel": { "surface": "web" }
        }))
        .unwrap_or_else(|error| panic!("parse failed: {error}"));

        assert_eq!(request.file_path, "/workspace/file.txt");
        assert_eq!(request.content, "value");
    }

    #[tokio::test]
    async fn execute_routes_session_and_returns_structured_result() {
        let mut server = mockito::Server::new_async().await;
        let call = server
            .mock("POST", chelix_protocol::TOOLS_SERVICE_OVERWRITE_FILE_PATH)
            .match_header("authorization", "Bearer overwrite-token")
            .match_body(mockito::Matcher::Json(json!({
                "filePath": "/workspace/file.txt",
                "content": "new value"
            })))
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body("{\"filePath\":\"/workspace/file.txt\",\"bytesWritten\":9}")
            .expect(1)
            .create_async()
            .await;
        let tool = OverwriteFileTool::new(client(server.url(), "overwrite-token"));

        let result = tool
            .execute(json!({
                "filePath": "/workspace/file.txt",
                "content": "new value",
                "_session_key": "session:test"
            }))
            .await
            .unwrap_or_else(|error| panic!("execute failed: {error}"));

        assert_eq!(
            result,
            json!({
                "filePath": "/workspace/file.txt",
                "bytesWritten": 9
            })
        );
        call.assert_async().await;
    }

    #[tokio::test]
    async fn execute_surfaces_service_failure() {
        let mut server = mockito::Server::new_async().await;
        let call = server
            .mock("POST", chelix_protocol::TOOLS_SERVICE_OVERWRITE_FILE_PATH)
            .with_status(422)
            .with_header("content-type", "application/json")
            .with_body("{\"error\":\"refusing to overwrite symbolic link\"}")
            .expect(1)
            .create_async()
            .await;
        let result = OverwriteFileTool::new(client(server.url(), "test-token"))
            .execute(json!({
                "filePath": "/workspace/link.txt",
                "content": "value"
            }))
            .await;
        let error = match result {
            Ok(_) => panic!("expected tools service failure"),
            Err(error) => error,
        };

        assert!(error.to_string().contains("symbolic link"));
        call.assert_async().await;
    }
}
