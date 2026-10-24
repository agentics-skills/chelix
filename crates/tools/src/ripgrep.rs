//! `ripgrep` agent tool backed exclusively by the managed tools service.

use std::sync::Arc;

use {
    async_trait::async_trait,
    chelix_agents::tool_registry::AgentTool,
    chelix_protocol::{
        RIPGREP_DEFAULT_MAX_FILES, RIPGREP_DEFAULT_MAX_MATCHES, RIPGREP_DEFAULT_MAX_OUTPUT_CHARS,
        RIPGREP_DEFAULT_TIMEOUT_MS, RipgrepInput, RipgrepRequest, RipgrepResult,
    },
    serde_json::{Value, json},
};

#[cfg(feature = "metrics")]
use chelix_metrics::{counter, labels, tools as tools_metrics};

use crate::tools_service::ManagedToolsService;

pub struct RipgrepTool {
    service: Arc<ManagedToolsService>,
}

impl RipgrepTool {
    #[must_use]
    pub fn new(service: Arc<ManagedToolsService>) -> Self {
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
            "additionalProperties": false,
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
                    "description": "Search within file contents at the specified paths. Accepts files or directories; directories are traversed recursively. Relative paths are resolved from cwd. If omitted, searches within file contents under cwd."
                },
                "cwd": {
                    "type": "string",
                    "description": "Optional working directory. If omitted, uses the home directory (e.g., /home/user)."
                },
                "fixedStrings": {
                    "type": "boolean",
                    "default": false,
                    "description": "Use fixed strings (-F)."
                },
                "multiline": {
                    "type": "boolean",
                    "default": false,
                    "description": "Allow matches to span multiple lines (maps to -U/--multiline). This does not make . match line terminators."
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
                    "description": "Include or exclude file paths using glob patterns, e.g., \"*.rs\", \"Cargo.toml\", \"src/**\". Prefix with \"!\" to exclude, e.g., \"!**/target/**\"."
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
                    "default": RIPGREP_DEFAULT_MAX_MATCHES,
                    "description": "Maximum number of match records to return."
                },
                "maxFiles": {
                    "type": "integer",
                    "minimum": 1,
                    "default": RIPGREP_DEFAULT_MAX_FILES,
                    "description": "Maximum number of files with matches to include."
                },
                "maxOutputChars": {
                    "type": "integer",
                    "minimum": 1,
                    "default": RIPGREP_DEFAULT_MAX_OUTPUT_CHARS,
                    "description": "Maximum combined rg stdout/stderr characters to process."
                },
                "timeoutMs": {
                    "type": "integer",
                    "minimum": 0,
                    "default": RIPGREP_DEFAULT_TIMEOUT_MS,
                    "description": "Timeout in milliseconds for the search."
                },
                "includeHidden": {
                    "type": "boolean",
                    "default": true,
                    "description": "Include hidden files (maps to --hidden)."
                },
                "unrestricted": {
                    "type": "integer",
                    "minimum": 0,
                    "maximum": 3,
                    "default": 3,
                    "description": "Ignore rules level (maps to -u/-uu/-uuu)."
                },
                "gitignore": {
                    "type": "boolean",
                    "default": true,
                    "description": "Respect git ignore rules (.gitignore, .git/info/exclude, core.excludesFile), including parent directories and outside git repositories."
                },
                "followSymlinks": {
                    "type": "boolean",
                    "default": false,
                    "description": "Follow symlinks (maps to --follow)."
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
            .filter(|value| !value.is_empty())
            .ok_or_else(|| anyhow::anyhow!("missing session id"))?
            .to_string();
        let input = parse_input(params)?;
        let result = self
            .service
            .ripgrep(&session_key, RipgrepRequest { params: input })
            .await;
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
        agent_result(result?.result)
    }
}

fn agent_result(result: RipgrepResult) -> anyhow::Result<Value> {
    let limit = if result.truncated {
        match result.truncated_reason.as_deref() {
            Some("maxMatches") => Some(json!(result.limits.max_matches)),
            Some("maxFiles") => Some(json!(result.limits.max_files)),
            Some("maxOutputChars") => Some(json!(result.limits.max_output_chars)),
            Some("timeout") => Some(json!(result.limits.timeout_ms)),
            _ => None,
        }
    } else {
        None
    };
    let searched_files = result
        .summary
        .stats
        .as_ref()
        .and_then(|stats| stats.get("searches"))
        .cloned();
    let mut value = serde_json::to_value(result)?;
    let output = value
        .as_object_mut()
        .ok_or_else(|| anyhow::anyhow!("serialized ripgrep result must be an object"))?;
    for key in ["tool", "detail", "limits"] {
        output.remove(key);
    }
    let summary = output
        .get_mut("summary")
        .and_then(Value::as_object_mut)
        .ok_or_else(|| anyhow::anyhow!("serialized ripgrep summary must be an object"))?;
    summary.remove("elapsed");
    summary.remove("stats");
    if let Some(searched_files) = searched_files {
        summary.insert("searchedFiles".to_string(), searched_files);
    }
    if let Some(limit) = limit {
        output.insert("limit".to_string(), limit);
    }
    if output
        .get("context")
        .and_then(Value::as_array)
        .is_some_and(|context| context.is_empty())
    {
        output.remove("context");
    }
    if output.get("exitCode").and_then(Value::as_i64) == Some(0) {
        output.remove("exitCode");
    }
    if output
        .get("stderr")
        .and_then(Value::as_str)
        .is_some_and(str::is_empty)
    {
        output.remove("stderr");
    }
    Ok(value)
}

fn parse_input(mut params: Value) -> anyhow::Result<RipgrepInput> {
    let map = params
        .as_object_mut()
        .ok_or_else(|| anyhow::anyhow!("ripgrep parameters must be an object"))?;
    map.retain(|key, _| !key.starts_with('_'));
    let input: RipgrepInput = serde_json::from_value(params)
        .map_err(|error| anyhow::anyhow!("invalid ripgrep parameters: {error}"))?;
    input
        .validate()
        .map_err(|error| anyhow::anyhow!("invalid ripgrep parameters: {error}"))?;
    Ok(input)
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

    #[tokio::test]
    async fn execute_routes_session_and_strips_only_internal_context() {
        let mut server = mockito::Server::new_async().await;
        let call = server
            .mock("POST", chelix_protocol::TOOLS_SERVICE_RIPGREP_PATH)
            .match_header("authorization", "Bearer rg-token")
            .match_body(mockito::Matcher::Json(json!({
                "params": {
                    "pattern": "needle",
                    "paths": [],
                    "fixedStrings": false,
                    "multiline": false,
                    "detail": "lines",
                    "glob": [],
                    "type": [],
                    "typeNot": [],
                    "maxMatches": 300,
                    "maxFiles": 100,
                    "maxOutputChars": 30000,
                    "timeoutMs": 30000,
                    "includeHidden": true,
                    "unrestricted": 3,
                    "gitignore": true,
                    "followSymlinks": false
                }
            })))
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                serde_json::json!({
                    "result": {
                        "tool": "ripgrep",
                        "detail": "lines",
                        "found": true,
                        "timedOut": false,
                        "truncated": false,
                        "limits": {
                            "maxMatches": 300,
                            "maxFiles": 100,
                            "maxOutputChars": 30000,
                            "timeoutMs": 30000
                        },
                        "summary": {
                            "filesWithMatches": 1,
                            "matchCount": 1,
                            "elapsed": null
                        },
                        "matches": [],
                        "context": [],
                        "exitCode": 0
                    }
                })
                .to_string(),
            )
            .expect(1)
            .create_async()
            .await;
        let tool = RipgrepTool::new(client(server.url(), "rg-token"));
        let result = tool
            .execute(json!({
                "pattern": "needle",
                "_session_key": "session:test",
                "_channel": { "surface": "web" }
            }))
            .await
            .unwrap_or_else(|error| panic!("execute failed: {error}"));

        assert_eq!(result["found"], true);
        call.assert_async().await;
    }

    #[test]
    fn parse_input_rejects_null_and_unknown_public_fields() {
        for invalid in [
            json!({ "pattern": "needle", "cwd": null }),
            json!({ "pattern": "needle", "maxMatches": null }),
            json!({ "pattern": "needle", "obsolete": true }),
        ] {
            assert!(parse_input(invalid).is_err());
        }
    }

    #[test]
    fn parse_input_ignores_enriched_internal_fields() {
        let input = parse_input(json!({
            "pattern": "needle",
            "_session_key": "session:test",
            "_channel": { "surface": "web" }
        }))
        .unwrap_or_else(|error| panic!("parse failed: {error}"));

        assert_eq!(input.pattern, "needle");
    }

    /// The advertised bounds must be exactly the values the tool accepts, so a
    /// model cannot read a range the server later rejects.
    #[test]
    fn unrestricted_schema_bounds_match_the_accepted_levels() {
        let schema =
            RipgrepTool::new(client("http://tools.invalid".into(), "unused")).parameters_schema();
        let unrestricted = &schema["properties"]["unrestricted"];

        assert_eq!(unrestricted["type"], "integer");
        assert_eq!(unrestricted["default"], 3);
        let minimum = unrestricted["minimum"]
            .as_i64()
            .unwrap_or_else(|| panic!("'unrestricted' must declare a numeric minimum"));
        let maximum = unrestricted["maximum"]
            .as_i64()
            .unwrap_or_else(|| panic!("'unrestricted' must declare a numeric maximum"));

        for level in minimum..=maximum {
            let input = parse_input(json!({ "pattern": "needle", "unrestricted": level }))
                .unwrap_or_else(|error| {
                    panic!("level {level} is advertised but rejected: {error}")
                });
            assert_eq!(i64::from(input.unrestricted), level);
        }
        assert!(parse_input(json!({ "pattern": "needle", "unrestricted": maximum + 1 })).is_err());
        assert!(parse_input(json!({ "pattern": "needle", "unrestricted": minimum - 1 })).is_err());
    }

    #[tokio::test]
    async fn execute_surfaces_service_failure() {
        let mut server = mockito::Server::new_async().await;
        let call = server
            .mock("POST", chelix_protocol::TOOLS_SERVICE_RIPGREP_PATH)
            .with_status(422)
            .with_header("content-type", "application/json")
            .with_body("{\"error\":\"synthetic tools service failure\"}")
            .expect(1)
            .create_async()
            .await;
        let result = RipgrepTool::new(client(server.url(), "test-token"))
            .execute(json!({ "pattern": "needle", "_session_key": "session:test" }))
            .await;
        let error = match result {
            Ok(_) => panic!("expected tools service failure"),
            Err(error) => error,
        };

        assert!(
            error
                .to_string()
                .contains("synthetic tools service failure")
        );
        call.assert_async().await;
    }

    async fn mock_execute(result: Value) -> Value {
        let mut server = mockito::Server::new_async().await;
        server
            .mock("POST", chelix_protocol::TOOLS_SERVICE_RIPGREP_PATH)
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(serde_json::json!({ "result": result }).to_string())
            .create_async()
            .await;
        RipgrepTool::new(client(server.url(), "rg-token"))
            .execute(json!({
                "pattern": "needle",
                "_session_key": "session:test"
            }))
            .await
            .unwrap_or_else(|error| panic!("execute failed: {error}"))
    }

    #[tokio::test]
    async fn execute_reduces_completed_search_json() {
        let result = mock_execute(json!({
            "tool": "ripgrep",
            "detail": "lines",
            "found": true,
            "timedOut": false,
            "truncated": false,
            "limits": {
                "maxMatches": 300,
                "maxFiles": 100,
                "maxOutputChars": 30000,
                "timeoutMs": 30000
            },
            "summary": {
                "filesWithMatches": 1,
                "matchCount": 1,
                "elapsed": {"secs": 0, "nanos": 1, "human": "0.000000001s"},
                "stats": {"searches": 4, "matches": 9, "bytes_searched": 13}
            },
            "matches": [{"path": "ok.txt", "line_number": 1, "lines": "alpha needle\n"}],
            "context": [],
            "stderr": "",
            "exitCode": 0
        }))
        .await;

        assert_eq!(
            result,
            json!({
                "found": true,
                "timedOut": false,
                "truncated": false,
                "summary": {"filesWithMatches": 1, "matchCount": 1, "searchedFiles": 4},
                "matches": [{"path": "ok.txt", "line_number": 1, "lines": "alpha needle\n"}]
            })
        );
    }

    #[tokio::test]
    async fn execute_preserves_nonzero_exit_stderr_context_and_submatches() {
        let stderr = "rg: missing.txt: IO error for operation on missing.txt: No such file or directory (os error 2)\n";
        let result = mock_execute(json!({
            "tool": "ripgrep",
            "detail": "lines+submatches",
            "found": true,
            "timedOut": false,
            "truncated": false,
            "limits": {
                "maxMatches": 300,
                "maxFiles": 100,
                "maxOutputChars": 30000,
                "timeoutMs": 30000
            },
            "summary": {
                "filesWithMatches": 1,
                "matchCount": 1,
                "elapsed": null,
                "stats": {"searches": 1, "searches_with_match": 1}
            },
            "matches": [{
                "path": "ok.txt",
                "line_number": 1,
                "lines": "alpha needle\n",
                "submatches": [{"match": "needle", "start": 6, "end": 12}]
            }],
            "context": [{"path": "ok.txt", "line_number": 2, "lines": "tail\n"}],
            "stderr": stderr,
            "exitCode": 2
        }))
        .await;

        assert_eq!(
            result,
            json!({
                "found": true,
                "timedOut": false,
                "truncated": false,
                "summary": {"filesWithMatches": 1, "matchCount": 1, "searchedFiles": 1},
                "matches": [{
                    "path": "ok.txt",
                    "line_number": 1,
                    "lines": "alpha needle\n",
                    "submatches": [{"match": "needle", "start": 6, "end": 12}]
                }],
                "context": [{"path": "ok.txt", "line_number": 2, "lines": "tail\n"}],
                "stderr": stderr,
                "exitCode": 2
            })
        );
    }

    #[tokio::test]
    async fn execute_maps_truncation_reasons_to_configured_limits() {
        let max_matches = mock_execute(json!({
            "tool": "ripgrep",
            "detail": "lines",
            "found": true,
            "timedOut": false,
            "truncated": true,
            "truncatedReason": "maxMatches",
            "limits": {
                "maxMatches": 2,
                "maxFiles": 3,
                "maxOutputChars": 30000,
                "timeoutMs": 30000
            },
            "summary": {
                "filesWithMatches": 1,
                "matchCount": 2,
                "elapsed": null
            },
            "matches": [
                {"path": "a.txt", "line_number": 1, "lines": "alpha\n"},
                {"path": "a.txt", "line_number": 2, "lines": "beta\n"}
            ],
            "context": [],
            "exitCode": null
        }))
        .await;
        assert_eq!(
            max_matches,
            json!({
                "found": true,
                "timedOut": false,
                "truncated": true,
                "truncatedReason": "maxMatches",
                "limit": 2,
                "summary": {"filesWithMatches": 1, "matchCount": 2},
                "matches": [
                    {"path": "a.txt", "line_number": 1, "lines": "alpha\n"},
                    {"path": "a.txt", "line_number": 2, "lines": "beta\n"}
                ],
                "exitCode": null
            })
        );

        let max_files = mock_execute(json!({
            "tool": "ripgrep",
            "detail": "files",
            "found": true,
            "timedOut": false,
            "truncated": true,
            "truncatedReason": "maxFiles",
            "limits": {
                "maxMatches": 300,
                "maxFiles": 3,
                "maxOutputChars": 30000,
                "timeoutMs": 30000
            },
            "summary": {
                "filesWithMatches": 3,
                "matchCount": 3,
                "elapsed": null
            },
            "files": ["a.txt", "b.txt", "c.txt"],
            "exitCode": null
        }))
        .await;
        assert_eq!(
            max_files,
            json!({
                "found": true,
                "timedOut": false,
                "truncated": true,
                "truncatedReason": "maxFiles",
                "limit": 3,
                "summary": {"filesWithMatches": 3, "matchCount": 3},
                "files": ["a.txt", "b.txt", "c.txt"],
                "exitCode": null
            })
        );

        let max_output = mock_execute(json!({
            "tool": "ripgrep",
            "detail": "lines",
            "found": false,
            "timedOut": false,
            "truncated": true,
            "truncatedReason": "maxOutputChars",
            "limits": {
                "maxMatches": 300,
                "maxFiles": 100,
                "maxOutputChars": 5,
                "timeoutMs": 30000
            },
            "summary": {
                "filesWithMatches": 0,
                "matchCount": 0,
                "elapsed": null
            },
            "matches": [],
            "context": [],
            "exitCode": null
        }))
        .await;
        assert_eq!(
            max_output,
            json!({
                "found": false,
                "timedOut": false,
                "truncated": true,
                "truncatedReason": "maxOutputChars",
                "limit": 5,
                "summary": {"filesWithMatches": 0, "matchCount": 0},
                "matches": [],
                "exitCode": null
            })
        );

        let timeout = mock_execute(json!({
            "tool": "ripgrep",
            "detail": "lines",
            "found": false,
            "timedOut": true,
            "truncated": true,
            "truncatedReason": "timeout",
            "limits": {
                "maxMatches": 300,
                "maxFiles": 100,
                "maxOutputChars": 30000,
                "timeoutMs": 7
            },
            "summary": {
                "filesWithMatches": 0,
                "matchCount": 0,
                "elapsed": null
            },
            "matches": [],
            "context": [],
            "exitCode": null
        }))
        .await;
        assert_eq!(
            timeout,
            json!({
                "found": false,
                "timedOut": true,
                "truncated": true,
                "truncatedReason": "timeout",
                "limit": 7,
                "summary": {"filesWithMatches": 0, "matchCount": 0},
                "matches": [],
                "exitCode": null
            })
        );
    }
}
