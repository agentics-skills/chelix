//! `ripgrep` agent tool backed exclusively by the managed tools service.

use std::sync::Arc;

use {
    async_trait::async_trait,
    chelix_agents::tool_registry::AgentTool,
    serde_json::{Value, json},
};

#[cfg(feature = "metrics")]
use chelix_metrics::{counter, labels, tools as tools_metrics};

use crate::tools_service::ToolsService;

const DEFAULT_MAX_MATCHES: usize = 2000;
const DEFAULT_MAX_FILES: usize = 200;
const DEFAULT_MAX_OUTPUT_CHARS: usize = 200_000;
const DEFAULT_TIMEOUT_MS: u64 = 10_000;

pub struct RipgrepTool {
    service: Arc<dyn ToolsService>,
}

impl RipgrepTool {
    #[must_use]
    pub fn new(service: Arc<dyn ToolsService>) -> Self {
        Self { service }
    }
}

#[async_trait]
impl AgentTool for RipgrepTool {
    fn name(&self) -> &str {
        "ripgrep"
    }

    fn description(&self) -> &str {
        "Search files using ripgrep (rg) with structured JSON output."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["pattern"],
            "properties": {
                "pattern": {
                    "type": "string",
                    "minLength": 1,
                    "description": "Pattern to search for."
                },
                "paths": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Paths to search (defaults to the working directory)."
                },
                "cwd": {
                    "type": "string",
                    "description": "Working directory for the rg process."
                },
                "fixedStrings": {
                    "type": "boolean",
                    "default": false,
                    "description": "Use fixed strings (-F)."
                },
                "caseMode": {
                    "type": "string",
                    "enum": ["sensitive", "ignore", "smart"],
                    "description": "Case matching mode."
                },
                "detail": {
                    "type": "string",
                    "enum": ["summary", "files", "lines", "lines+submatches"],
                    "default": "lines",
                    "description": "Detail level for results."
                },
                "glob": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Glob patterns mapped to --glob."
                },
                "type": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Ripgrep file type names from rg --type-list. Common extension-like values such as tsx/jsx are normalized; unknown extension-like values are converted to glob filters."
                },
                "typeNot": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Ripgrep file type names to exclude via --type-not. Common extension-like values such as tsx/jsx are normalized; unknown extension-like values are converted to exclusion glob filters."
                },
                "contextLines": {
                    "type": "integer",
                    "minimum": 0,
                    "description": "Context lines mapped to -C."
                },
                "maxMatches": {
                    "type": "integer",
                    "minimum": 1,
                    "default": DEFAULT_MAX_MATCHES,
                    "description": "Maximum number of match records to return."
                },
                "maxFiles": {
                    "type": "integer",
                    "minimum": 1,
                    "default": DEFAULT_MAX_FILES,
                    "description": "Maximum number of files with matches to include."
                },
                "maxOutputChars": {
                    "type": "integer",
                    "minimum": 1,
                    "default": DEFAULT_MAX_OUTPUT_CHARS,
                    "description": "Maximum rg stdout characters to process."
                },
                "timeoutMs": {
                    "type": "integer",
                    "minimum": 0,
                    "default": DEFAULT_TIMEOUT_MS,
                    "description": "Timeout in milliseconds for the search."
                },
                "includeHidden": {
                    "type": "boolean",
                    "default": true,
                    "description": "Include hidden files (maps to --hidden)."
                },
                "unrestricted": {
                    "type": "integer",
                    "enum": [0, 1, 2, 3],
                    "default": 3,
                    "description": "Ignore rules level (maps to -u/-uu/-uuu)."
                },
                "followSymlinks": {
                    "type": "boolean",
                    "default": false,
                    "description": "Follow symlinks (maps to --follow)."
                }
            }
        })
    }

    async fn execute(&self, mut params: Value) -> anyhow::Result<Value> {
        let session_key = params
            .get("_session_key")
            .and_then(Value::as_str)
            .unwrap_or("main")
            .to_string();
        strip_internal_and_null_params(&mut params);
        let result = self.service.ripgrep(&session_key, params).await;
        #[cfg(feature = "metrics")]
        match &result {
            Ok(_) => {
                counter!(
                    tools_metrics::EXECUTIONS_TOTAL,
                    labels::TOOL => "ripgrep".to_string(),
                    labels::SUCCESS => "true".to_string()
                )
                .increment(1);
            },
            Err(_) => {
                counter!(
                    tools_metrics::EXECUTION_ERRORS_TOTAL,
                    labels::TOOL => "ripgrep".to_string()
                )
                .increment(1);
            },
        }
        Ok(result?)
    }
}

fn strip_internal_and_null_params(value: &mut Value) {
    if let Some(map) = value.as_object_mut() {
        map.retain(|key, child| {
            if key.starts_with('_') || child.is_null() {
                return false;
            }
            strip_internal_and_null_params(child);
            !child.as_object().is_some_and(serde_json::Map::is_empty)
        });
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
        calls: Mutex<Vec<(String, Value)>>,
        result: Value,
        fail: bool,
    }

    #[async_trait]
    impl ToolsService for FakeToolsService {
        async fn list_directory(&self, _session_key: &str, _path: String) -> Result<String> {
            Err(Error::message("list_directory is not used by this test"))
        }

        async fn ripgrep(&self, session_key: &str, params: Value) -> Result<Value> {
            self.calls
                .lock()
                .map_err(|_| Error::message("test calls lock poisoned"))?
                .push((session_key.to_string(), params));
            if self.fail {
                return Err(Error::message("synthetic tools service failure"));
            }
            Ok(self.result.clone())
        }
    }

    #[tokio::test]
    async fn execute_routes_session_and_strips_internal_context() {
        let service = Arc::new(FakeToolsService {
            calls: Mutex::new(Vec::new()),
            result: json!({ "found": true }),
            fail: false,
        });
        let tool = RipgrepTool::new(service.clone());
        let result = tool
            .execute(json!({
                "pattern": "needle",
                "cwd": null,
                "_session_key": "session:test",
                "_channel": { "surface": "web" }
            }))
            .await
            .unwrap_or_else(|error| panic!("execute failed: {error}"));

        assert_eq!(result["found"], true);
        let calls = service
            .calls
            .lock()
            .unwrap_or_else(|error| panic!("calls lock failed: {error}"));
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0, "session:test");
        assert_eq!(calls[0].1, json!({ "pattern": "needle" }));
    }

    #[tokio::test]
    async fn execute_surfaces_service_failure() {
        let service = Arc::new(FakeToolsService {
            calls: Mutex::new(Vec::new()),
            result: Value::Null,
            fail: true,
        });
        let error = RipgrepTool::new(service)
            .execute(json!({ "pattern": "needle" }))
            .await
            .unwrap_err();

        assert!(
            error
                .to_string()
                .contains("synthetic tools service failure")
        );
    }
}
