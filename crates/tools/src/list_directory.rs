//! `list_directory` agent tool backed exclusively by the managed tools service.

use std::sync::Arc;

use {
    anyhow::Context,
    async_trait::async_trait,
    chelix_agents::tool_registry::AgentTool,
    chelix_protocol::ListDirectoryRequest,
    serde::Deserialize,
    serde_json::{Value, json},
};

#[cfg(feature = "metrics")]
use chelix_metrics::{counter, labels, tools as tools_metrics};

use crate::tools_service::ManagedToolsService;

#[derive(Debug, Deserialize)]
struct ListDirectoryInput {
    path: String,
}

pub struct ListDirectoryTool {
    service: Arc<ManagedToolsService>,
}

impl ListDirectoryTool {
    #[must_use]
    pub fn new(service: Arc<ManagedToolsService>) -> Self {
        Self { service }
    }
}

#[async_trait]
impl AgentTool for ListDirectoryTool {
    fn name(&self) -> &str {
        "list_directory"
    }

    fn description(&self) -> &str {
        "List the contents of a directory. Result will have the name of each child. If the name ends in /, it's a folder. Text files include their logical line count in parentheses; binary files include a binary marker and size."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["path"],
            "properties": {
                "path": {
                    "type": "string",
                    "minLength": 1,
                    "description": "The absolute path to the directory to list."
                }
            }
        })
    }

    async fn execute(&self, params: Value) -> anyhow::Result<Value> {
        let session_key = params
            .get("_session_key")
            .and_then(Value::as_str)
            .unwrap_or("main")
            .to_string();
        let input: ListDirectoryInput =
            serde_json::from_value(params).context("invalid list_directory parameters")?;
        let result = self
            .service
            .list_directory(&session_key, ListDirectoryRequest { path: input.path })
            .await;
        #[cfg(feature = "metrics")]
        match &result {
            Ok(_) => {
                counter!(
                    tools_metrics::EXECUTIONS_TOTAL,
                    labels::TOOL => "list_directory".to_string(),
                    labels::SUCCESS => "true".to_string()
                )
                .increment(1);
            },
            Err(_) => {
                counter!(
                    tools_metrics::EXECUTION_ERRORS_TOTAL,
                    labels::TOOL => "list_directory".to_string()
                )
                .increment(1);
            },
        }
        Ok(Value::String(result?.result))
    }
}

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
    fn exposes_reference_name_and_schema() {
        let tool = ListDirectoryTool::new(client("http://127.0.0.1:1".into(), "unused"));

        assert_eq!(tool.name(), "list_directory");
        let schema = tool.parameters_schema();
        assert_eq!(schema["required"], json!(["path"]));
        assert_eq!(schema["properties"]["path"]["type"], "string");
        assert_eq!(schema["properties"]["path"]["minLength"], 1);
    }

    #[tokio::test]
    async fn execute_routes_session_and_returns_plain_text() {
        let mut server = mockito::Server::new_async().await;
        let call = server
            .mock("POST", chelix_protocol::TOOLS_SERVICE_LIST_DIRECTORY_PATH)
            .match_header("authorization", "Bearer list-token")
            .match_body(mockito::Matcher::Json(json!({ "path": "/workspace" })))
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body("{\"result\":\"src/\\nCargo.toml (1 line)\"}")
            .expect(1)
            .create_async()
            .await;
        let tool = ListDirectoryTool::new(client(server.url(), "list-token"));

        let result = tool
            .execute(json!({
                "path": "/workspace",
                "_session_key": "session:test",
                "_channel": { "surface": "web" }
            }))
            .await
            .unwrap_or_else(|error| panic!("execute failed: {error}"));

        assert_eq!(result, Value::String("src/\nCargo.toml (1 line)".into()));
        call.assert_async().await;
    }

    #[tokio::test]
    async fn execute_rejects_missing_path() {
        let error = ListDirectoryTool::new(client("http://127.0.0.1:1".into(), "unused"))
            .execute(json!({}))
            .await
            .unwrap_err();

        assert!(
            error
                .to_string()
                .contains("invalid list_directory parameters")
        );
    }

    #[tokio::test]
    async fn execute_surfaces_service_failure() {
        let mut server = mockito::Server::new_async().await;
        let call = server
            .mock("POST", chelix_protocol::TOOLS_SERVICE_LIST_DIRECTORY_PATH)
            .with_status(422)
            .with_header("content-type", "application/json")
            .with_body("{\"error\":\"synthetic tools service failure\"}")
            .expect(1)
            .create_async()
            .await;
        let error = ListDirectoryTool::new(client(server.url(), "test-token"))
            .execute(json!({ "path": "/workspace" }))
            .await
            .unwrap_err();

        assert!(
            error
                .to_string()
                .contains("synthetic tools service failure")
        );
        call.assert_async().await;
    }
}
