//! `multiedit_file` agent tool backed exclusively by the managed tools service.

use std::sync::Arc;

use {
    async_trait::async_trait,
    chelix_agents::tool_registry::AgentTool,
    chelix_protocol::{MultieditFileRequest, MultieditFileResponse},
    serde_json::{Value, json},
};

#[cfg(feature = "metrics")]
use chelix_metrics::{counter, labels, tools as tools_metrics};

use crate::tools_service::ManagedToolsService;

pub struct MultieditFileTool {
    service: Arc<ManagedToolsService>,
}

impl MultieditFileTool {
    #[must_use]
    pub fn new(service: Arc<ManagedToolsService>) -> Self {
        Self { service }
    }
}

#[async_trait]
impl AgentTool for MultieditFileTool {
    fn name(&self) -> &str {
        "multiedit_file"
    }

    fn description(&self) -> &str {
        "Apply an ordered batch of exact UTF-8 text replacements to one existing regular file. Each edit sees the output of the previous edit. The path must be absolute. Every edit must succeed in memory before the existing file is written in place; symbolic links are followed."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "additionalProperties": false,
            "required": ["filePath", "edits"],
            "properties": {
                "filePath": {
                    "type": "string",
                    "minLength": 1,
                    "description": "The absolute path of the existing UTF-8 file to edit."
                },
                "edits": {
                    "type": "array",
                    "minItems": 1,
                    "description": "The ordered exact replacements to prepare before writing the file.",
                    "items": {
                        "oneOf": [
                            {
                                "type": "object",
                                "additionalProperties": false,
                                "required": ["oldString", "newString"],
                                "properties": {
                                    "oldString": {
                                        "type": "string",
                                        "minLength": 1,
                                        "description": "The exact text to replace. It must identify one location."
                                    },
                                    "newString": {
                                        "type": "string",
                                        "description": "The replacement text. It must differ from oldString."
                                    }
                                }
                            },
                            {
                                "type": "object",
                                "additionalProperties": false,
                                "required": ["oldString", "newString", "replaceAll"],
                                "properties": {
                                    "oldString": {
                                        "type": "string",
                                        "minLength": 1,
                                        "description": "The exact text to replace."
                                    },
                                    "newString": {
                                        "type": "string",
                                        "description": "The replacement text. It must differ from oldString."
                                    },
                                    "replaceAll": {
                                        "type": "boolean",
                                        "description": "Whether to replace every match instead of requiring a unique match."
                                    }
                                }
                            }
                        ]
                    }
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
        let result = self.service.multiedit_file(&session_key, request).await;
        record_metrics(&result);
        Ok(serde_json::to_value(result?)?)
    }
}

fn parse_input(mut params: Value) -> anyhow::Result<MultieditFileRequest> {
    let map = params
        .as_object_mut()
        .ok_or_else(|| anyhow::anyhow!("multiedit_file parameters must be an object"))?;
    map.retain(|key, _| !key.starts_with('_'));
    let request: MultieditFileRequest = serde_json::from_value(params)
        .map_err(|error| anyhow::anyhow!("invalid multiedit_file parameters: {error}"))?;
    request
        .validate()
        .map_err(|error| anyhow::anyhow!("invalid multiedit_file parameters: {error}"))?;
    Ok(request)
}

#[cfg(feature = "metrics")]
fn record_metrics(result: &crate::Result<MultieditFileResponse>) {
    match result {
        Ok(_) => {
            counter!(
                tools_metrics::EXECUTIONS_TOTAL,
                labels::TOOL => "multiedit_file".to_string(),
                labels::SUCCESS => "true".to_string()
            )
            .increment(1);
        },
        Err(_) => {
            counter!(
                tools_metrics::EXECUTION_ERRORS_TOTAL,
                labels::TOOL => "multiedit_file".to_string()
            )
            .increment(1);
        },
    }
}

#[cfg(not(feature = "metrics"))]
fn record_metrics(_result: &crate::Result<MultieditFileResponse>) {}

#[cfg(test)]
mod tests {
    use {super::*, crate::sandbox::ToolsServiceEndpoint, chelix_protocol::EditFileRecovery};

    fn client(base_url: String, token: &str) -> Arc<ManagedToolsService> {
        ManagedToolsService::for_test(ToolsServiceEndpoint {
            base_url,
            token: token.into(),
        })
        .unwrap_or_else(|error| panic!("test client failed: {error}"))
    }

    #[test]
    fn exposes_name_and_strict_schema() {
        let tool = MultieditFileTool::new(client("http://127.0.0.1:1".into(), "unused"));

        assert_eq!(tool.name(), "multiedit_file");
        let schema = tool.parameters_schema();
        assert_eq!(schema["required"], json!(["filePath", "edits"]));
        assert_eq!(schema["additionalProperties"], false);
        assert_eq!(schema["properties"]["edits"]["minItems"], 1);
        let variants = schema["properties"]["edits"]["items"]["oneOf"]
            .as_array()
            .unwrap_or_else(|| panic!("edits.items.oneOf must be an array"));
        assert_eq!(variants.len(), 2);
        assert_eq!(variants[0]["required"], json!(["oldString", "newString"]));
        assert_eq!(
            variants[1]["required"],
            json!(["oldString", "newString", "replaceAll"])
        );
        assert!(variants[0]["properties"].get("replaceAll").is_none());
        assert!(
            variants[1]["properties"]["replaceAll"]
                .get("default")
                .is_none()
        );
    }

    #[test]
    fn parse_input_rejects_non_object_missing_null_unknown_and_invalid_fields() {
        for invalid in [
            Value::Null,
            json!({ "filePath": "/tmp/file", "edits": [] }),
            json!({
                "filePath": "/tmp/file",
                "edits": [{ "oldString": "old" }]
            }),
            json!({
                "filePath": "/tmp/file",
                "edits": [{ "oldString": "old", "newString": "new", "replaceAll": null }]
            }),
            json!({
                "filePath": "/tmp/file",
                "edits": [{ "oldString": "old", "newString": "new", "obsolete": true }]
            }),
            json!({
                "filePath": "/tmp/file",
                "edits": [{ "oldString": "same", "newString": "same" }]
            }),
            json!({
                "filePath": "/tmp/file",
                "edits": [{ "old_string": "old", "new_string": "new" }]
            }),
        ] {
            assert!(parse_input(invalid).is_err());
        }
    }

    #[test]
    fn parse_input_accepts_both_edit_forms_and_strips_only_internal_context() {
        let request = parse_input(json!({
            "filePath": "/workspace/file.txt",
            "edits": [
                { "oldString": "old", "newString": "intermediate" },
                { "oldString": "intermediate", "newString": "new", "replaceAll": true }
            ],
            "_session_key": "session:test",
            "_channel": { "surface": "web" }
        }))
        .unwrap_or_else(|error| panic!("parse failed: {error}"));

        assert_eq!(request.file_path, "/workspace/file.txt");
        assert_eq!(request.edits.len(), 2);
        assert!(!request.edits[0].replace_all());
        assert!(request.edits[1].replace_all());
    }

    #[tokio::test]
    async fn execute_routes_session_and_returns_structured_result() {
        let mut server = mockito::Server::new_async().await;
        let call = server
            .mock("POST", chelix_protocol::TOOLS_SERVICE_MULTIEDIT_FILE_PATH)
            .match_header("authorization", "Bearer multiedit-token")
            .match_body(mockito::Matcher::Json(json!({
                "filePath": "/workspace/file.txt",
                "edits": [
                    { "oldString": "old", "newString": "intermediate" },
                    { "oldString": "intermediate", "newString": "new", "replaceAll": true }
                ]
            })))
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                "{\"filePath\":\"/workspace/file.txt\",\"editsApplied\":2,\"replacementsPerEdit\":[1,1],\"recoveriesPerEdit\":[null,\"crlf\"]}",
            )
            .expect(1)
            .create_async()
            .await;
        let tool = MultieditFileTool::new(client(server.url(), "multiedit-token"));

        let result = tool
            .execute(json!({
                "filePath": "/workspace/file.txt",
                "edits": [
                    { "oldString": "old", "newString": "intermediate" },
                    { "oldString": "intermediate", "newString": "new", "replaceAll": true }
                ],
                "_session_key": "session:test"
            }))
            .await
            .unwrap_or_else(|error| panic!("execute failed: {error}"));

        assert_eq!(
            result,
            json!({
                "filePath": "/workspace/file.txt",
                "editsApplied": 2,
                "replacementsPerEdit": [1, 1],
                "recoveriesPerEdit": [null, EditFileRecovery::Crlf]
            })
        );
        call.assert_async().await;
    }

    #[tokio::test]
    async fn execute_surfaces_service_failure() {
        let mut server = mockito::Server::new_async().await;
        let call = server
            .mock("POST", chelix_protocol::TOOLS_SERVICE_MULTIEDIT_FILE_PATH)
            .with_status(422)
            .with_header("content-type", "application/json")
            .with_body("{\"error\":\"edit #2: oldString not found in file; edit refused.\"}")
            .expect(1)
            .create_async()
            .await;
        let result = MultieditFileTool::new(client(server.url(), "test-token"))
            .execute(json!({
                "filePath": "/workspace/file.txt",
                "edits": [
                    { "oldString": "old", "newString": "intermediate" },
                    { "oldString": "missing", "newString": "new" }
                ]
            }))
            .await;
        let error = match result {
            Ok(_) => panic!("expected tools service failure"),
            Err(error) => error,
        };

        assert!(error.to_string().contains("edit #2"));
        call.assert_async().await;
    }
}
