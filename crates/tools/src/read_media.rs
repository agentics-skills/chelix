//! `read_media` agent tool backed exclusively by the managed tools service.

use std::sync::Arc;

use {
    async_trait::async_trait,
    chelix_agents::tool_registry::{AgentTool, ToolResultPersistence, Truncation},
    chelix_protocol::{ReadMediaRequest, ReadMediaResponse},
    serde_json::{Value, json},
};

#[cfg(feature = "metrics")]
use chelix_metrics::{counter, labels, tools as tools_metrics};

use crate::tools_service::ManagedToolsService;

pub struct ReadMediaTool {
    service: Arc<ManagedToolsService>,
}

impl ReadMediaTool {
    #[must_use]
    pub fn new(service: Arc<ManagedToolsService>) -> Self {
        Self { service }
    }
}

#[async_trait]
impl AgentTool for ReadMediaTool {
    fn name(&self) -> &str {
        "read_media"
    }

    fn description(&self) -> &str {
        "Read media from the filesystem. Supports PDF documents and image files. PDF responses return extracted text with page metadata; image responses return an optimized base64 payload with media type and dimensions. Use the optional nested `pdf.pages` field only when reading a PDF page slice such as '3' or '10-20'. Omit `pdf` for images. Maximum 20 PDF pages per request."
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
                    "description": "The absolute path to the image or PDF file to read."
                },
                "pdf": {
                    "description": "Optional PDF-only options. Omit this property for images.",
                    "oneOf": [
                        {
                            "type": "object",
                            "additionalProperties": false,
                            "properties": {}
                        },
                        {
                            "type": "object",
                            "additionalProperties": false,
                            "required": ["pages"],
                            "properties": {
                                "pages": {
                                    "type": "string",
                                    "description": "PDF page selector. Use a single 1-indexed page like '3' or an inclusive range like '10-20'. Maximum 20 pages per request."
                                }
                            }
                        }
                    ]
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
        let result = self.service.read_media(&session_key, input).await;
        record_metrics(&result);
        Ok(serde_json::to_value(result?)?)
    }
}

fn parse_input(mut params: Value) -> anyhow::Result<ReadMediaRequest> {
    let map = params
        .as_object_mut()
        .ok_or_else(|| anyhow::anyhow!("read_media parameters must be an object"))?;
    map.retain(|key, _| !key.starts_with('_'));
    let input: ReadMediaRequest = serde_json::from_value(params)
        .map_err(|error| anyhow::anyhow!("invalid read_media parameters: {error}"))?;
    input
        .validate()
        .map_err(|error| anyhow::anyhow!("invalid read_media parameters: {error}"))?;
    Ok(input)
}

#[cfg(feature = "metrics")]
fn record_metrics(result: &crate::Result<ReadMediaResponse>) {
    match result {
        Ok(_) => {
            counter!(
                tools_metrics::EXECUTIONS_TOTAL,
                labels::TOOL => "read_media".to_string(),
                labels::SUCCESS => "true".to_string()
            )
            .increment(1);
        },
        Err(_) => {
            counter!(
                tools_metrics::EXECUTION_ERRORS_TOTAL,
                labels::TOOL => "read_media".to_string()
            )
            .increment(1);
        },
    }
}

#[cfg(not(feature = "metrics"))]
fn record_metrics(_result: &crate::Result<ReadMediaResponse>) {}

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
        let tool = ReadMediaTool::new(client("http://127.0.0.1:1".into(), "unused"));

        assert_eq!(tool.name(), "read_media");
        let schema = tool.parameters_schema();
        assert_eq!(schema["required"], json!(["filePath"]));
        assert_eq!(schema["additionalProperties"], false);
        assert_eq!(schema["properties"]["filePath"]["type"], "string");
        assert_eq!(
            schema["properties"]["pdf"]["oneOf"][1]["required"],
            json!(["pages"])
        );
        assert_eq!(
            schema["properties"]["pdf"]["oneOf"][1]["properties"]["pages"]["type"],
            "string"
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
            json!({ "filePath": "/tmp/file.pdf", "pdf": null }),
            json!({ "filePath": "/tmp/file.pdf", "pdf": { "pages": null } }),
            json!({ "filePath": "/tmp/file.pdf", "pdf": { "unknown": true } }),
            json!({ "filePath": "/tmp/file.pdf", "obsolete": true }),
            json!({ "filePath": "   " }),
        ] {
            assert!(parse_input(invalid).is_err());
        }
    }

    #[test]
    fn parse_input_strips_only_internal_context() {
        let input = parse_input(json!({
            "filePath": "/workspace/file.pdf",
            "pdf": { "pages": "1-2" },
            "_session_key": "session:test",
            "_channel": { "surface": "web" }
        }))
        .unwrap_or_else(|error| panic!("parse failed: {error}"));

        assert_eq!(input.file_path, "/workspace/file.pdf");
        assert_eq!(
            input.pdf.as_ref().and_then(|pdf| pdf.pages.as_deref()),
            Some("1-2")
        );
    }

    #[tokio::test]
    async fn execute_routes_session_and_returns_structured_json() {
        let mut server = mockito::Server::new_async().await;
        let call = server
            .mock("POST", chelix_protocol::TOOLS_SERVICE_READ_MEDIA_PATH)
            .match_header("authorization", "Bearer media-token")
            .match_body(mockito::Matcher::Json(json!({
                "filePath": "/workspace/image.png"
            })))
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                json!({
                    "kind": "image",
                    "filePath": "/workspace/image.png",
                    "mediaType": "image/png",
                    "originalWidth": 1,
                    "originalHeight": 1,
                    "finalWidth": 1,
                    "finalHeight": 1,
                    "wasResized": false,
                    "bytes": 68,
                    "base64": "iVBORw0KGgoAAAANSUhEUgAAAAEAAAAB"
                })
                .to_string(),
            )
            .expect(1)
            .create_async()
            .await;
        let tool = ReadMediaTool::new(client(server.url(), "media-token"));

        let result = tool
            .execute(json!({
                "filePath": "/workspace/image.png",
                "_session_key": "session:test"
            }))
            .await
            .unwrap_or_else(|error| panic!("execute failed: {error}"));

        assert_eq!(result["kind"], "image");
        assert_eq!(result["mediaType"], "image/png");
        call.assert_async().await;
    }

    #[tokio::test]
    async fn execute_routes_nested_pdf_options_to_service() {
        let mut server = mockito::Server::new_async().await;
        let call = server
            .mock("POST", chelix_protocol::TOOLS_SERVICE_READ_MEDIA_PATH)
            .match_header("authorization", "Bearer media-token")
            .match_body(mockito::Matcher::Json(json!({
                "filePath": "/workspace/manual.pdf",
                "pdf": { "pages": "2-4" }
            })))
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                json!({
                    "kind": "pdf",
                    "filePath": "/workspace/manual.pdf",
                    "totalPages": 12,
                    "pagesReturned": 3,
                    "startPage": 2,
                    "endPage": 4,
                    "truncated": true,
                    "content": "--- Page 2 ---\nBody"
                })
                .to_string(),
            )
            .expect(1)
            .create_async()
            .await;
        let tool = ReadMediaTool::new(client(server.url(), "media-token"));

        let result = tool
            .execute(json!({
                "filePath": "/workspace/manual.pdf",
                "pdf": { "pages": "2-4" },
                "_session_key": "session:test"
            }))
            .await
            .unwrap_or_else(|error| panic!("execute failed: {error}"));

        assert_eq!(result["kind"], "pdf");
        assert_eq!(result["startPage"], 2);
        assert_eq!(result["endPage"], 4);
        call.assert_async().await;
    }

    #[tokio::test]
    async fn execute_surfaces_service_failure() {
        let mut server = mockito::Server::new_async().await;
        let call = server
            .mock("POST", chelix_protocol::TOOLS_SERVICE_READ_MEDIA_PATH)
            .with_status(422)
            .with_header("content-type", "application/json")
            .with_body("{\"error\":\"failed to decode PDF '/workspace/broken.pdf'\"}")
            .expect(1)
            .create_async()
            .await;
        let result = ReadMediaTool::new(client(server.url(), "test-token"))
            .execute(json!({ "filePath": "/workspace/broken.pdf" }))
            .await;
        let error = match result {
            Ok(_) => panic!("expected tools service failure"),
            Err(error) => error,
        };

        assert!(error.to_string().contains("failed to decode PDF"));
        call.assert_async().await;
    }
}
