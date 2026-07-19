//! `list_directory` agent tool backed exclusively by the managed tools service.

use std::sync::Arc;

use {
    anyhow::Context,
    async_trait::async_trait,
    chelix_agents::tool_registry::AgentTool,
    serde::Deserialize,
    serde_json::{Value, json},
};

#[cfg(feature = "metrics")]
use chelix_metrics::{counter, labels, tools as tools_metrics};

use crate::tools_service::ToolsService;

#[derive(Debug, Deserialize)]
struct ListDirectoryInput {
    path: String,
}

pub struct ListDirectoryTool {
    service: Arc<dyn ToolsService>,
}

impl ListDirectoryTool {
    #[must_use]
    pub fn new(service: Arc<dyn ToolsService>) -> Self {
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
        let result = self.service.list_directory(&session_key, input.path).await;
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
        Ok(Value::String(result?))
    }
}

#[cfg(test)]
mod tests {
    use {
        super::*,
        crate::{Result, error::Error},
        std::sync::Mutex,
    };

    struct FakeToolsService {
        calls: Mutex<Vec<(String, String)>>,
        result: String,
        fail: bool,
    }

    #[async_trait]
    impl ToolsService for FakeToolsService {
        async fn list_directory(&self, session_key: &str, path: String) -> Result<String> {
            self.calls
                .lock()
                .map_err(|_| Error::message("test calls lock poisoned"))?
                .push((session_key.to_string(), path));
            if self.fail {
                return Err(Error::message("synthetic tools service failure"));
            }
            Ok(self.result.clone())
        }

        async fn ripgrep(&self, _session_key: &str, _params: Value) -> Result<Value> {
            Err(Error::message("ripgrep is not used by this test"))
        }
    }

    #[test]
    fn exposes_reference_name_and_schema() {
        let service = Arc::new(FakeToolsService {
            calls: Mutex::new(Vec::new()),
            result: String::new(),
            fail: false,
        });
        let tool = ListDirectoryTool::new(service);

        assert_eq!(tool.name(), "list_directory");
        let schema = tool.parameters_schema();
        assert_eq!(schema["required"], json!(["path"]));
        assert_eq!(schema["properties"]["path"]["type"], "string");
        assert_eq!(schema["properties"]["path"]["minLength"], 1);
    }

    #[tokio::test]
    async fn execute_routes_session_and_returns_plain_text() {
        let service = Arc::new(FakeToolsService {
            calls: Mutex::new(Vec::new()),
            result: "src/\nCargo.toml (1 line)".into(),
            fail: false,
        });
        let tool = ListDirectoryTool::new(service.clone());

        let result = tool
            .execute(json!({
                "path": "/workspace",
                "_session_key": "session:test",
                "_channel": { "surface": "web" }
            }))
            .await
            .unwrap_or_else(|error| panic!("execute failed: {error}"));

        assert_eq!(result, Value::String("src/\nCargo.toml (1 line)".into()));
        let calls = service
            .calls
            .lock()
            .unwrap_or_else(|error| panic!("calls lock failed: {error}"));
        assert_eq!(calls.as_slice(), &[(
            "session:test".into(),
            "/workspace".into()
        )]);
    }

    #[tokio::test]
    async fn execute_uses_main_session_by_default() {
        let service = Arc::new(FakeToolsService {
            calls: Mutex::new(Vec::new()),
            result: "Folder is empty".into(),
            fail: false,
        });

        ListDirectoryTool::new(service.clone())
            .execute(json!({ "path": "/workspace" }))
            .await
            .unwrap_or_else(|error| panic!("execute failed: {error}"));

        let calls = service
            .calls
            .lock()
            .unwrap_or_else(|error| panic!("calls lock failed: {error}"));
        assert_eq!(calls.as_slice(), &[("main".into(), "/workspace".into())]);
    }

    #[tokio::test]
    async fn execute_rejects_missing_path() {
        let service = Arc::new(FakeToolsService {
            calls: Mutex::new(Vec::new()),
            result: String::new(),
            fail: false,
        });

        let error = ListDirectoryTool::new(service)
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
        let service = Arc::new(FakeToolsService {
            calls: Mutex::new(Vec::new()),
            result: String::new(),
            fail: true,
        });

        let error = ListDirectoryTool::new(service)
            .execute(json!({ "path": "/workspace" }))
            .await
            .unwrap_err();

        assert!(
            error
                .to_string()
                .contains("synthetic tools service failure")
        );
    }
}
