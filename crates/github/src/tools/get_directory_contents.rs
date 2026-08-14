//! `github_get_directory_contents` — list one directory through
//! `GET /repos/{owner}/{repo}/contents/{path}`.

use std::sync::Arc;

use {
    async_trait::async_trait,
    chelix_agents::tool_registry::AgentTool,
    serde::Deserialize,
    serde_json::{Value, json},
};

use crate::{
    client::GitHubClient,
    error::{Error, Result},
    metrics::record_execution,
    tools::{parse_params, request::get_with_rate_limit_retry},
};

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
struct GetDirectoryContentsInput {
    owner: String,
    repo: String,
    #[serde(default)]
    path: Option<String>,
    #[serde(default)]
    r#ref: Option<String>,
}

impl GetDirectoryContentsInput {
    fn validate(&self) -> Result<()> {
        for (name, value) in [("owner", &self.owner), ("repo", &self.repo)] {
            if value.trim().is_empty() {
                return Err(Error::message(format!(
                    "Missing required parameter: {name}"
                )));
            }
        }
        Ok(())
    }

    /// Trim whitespace and slashes that do not belong to a GitHub contents path.
    fn normalized_path(&self) -> Option<String> {
        self.path
            .as_deref()
            .map(str::trim)
            .map(|path| path.trim_start_matches('/').trim_end_matches('/'))
            .filter(|path| !path.is_empty())
            .map(str::to_string)
    }

    /// Trimmed `ref`, or `None` when it is absent or blank.
    fn normalized_ref(&self) -> Option<&str> {
        self.r#ref
            .as_deref()
            .map(str::trim)
            .filter(|reference| !reference.is_empty())
    }
}

#[derive(Debug, Clone, Deserialize)]
struct DirectoryEntry {
    #[serde(rename = "type")]
    kind: String,
    size: Option<u64>,
    name: String,
    path: String,
    sha: String,
    html_url: Option<String>,
}

/// List directory contents from a GitHub repository.
pub struct GithubGetDirectoryContentsTool {
    client: Arc<GitHubClient>,
}

impl GithubGetDirectoryContentsTool {
    #[must_use]
    pub fn new(client: Arc<GitHubClient>) -> Self {
        Self { client }
    }

    async fn get_directory(
        &self,
        input: &GetDirectoryContentsInput,
        path: Option<&str>,
    ) -> Result<Vec<DirectoryEntry>> {
        let mut url = url::Url::parse(&format!("{}/repos/", self.client.base_url()))?;
        {
            let mut segments = url
                .path_segments_mut()
                .map_err(|()| Error::message("GitHub API base URL cannot have path segments"))?;
            segments.pop_if_empty();
            segments.push(input.owner.trim());
            segments.push(input.repo.trim());
            segments.push("contents");
            if let Some(path) = path {
                segments.extend(path.split('/'));
            }
        }
        if let Some(reference) = input.normalized_ref() {
            url.query_pairs_mut().append_pair("ref", reference);
        }

        let response = get_with_rate_limit_retry(&self.client, &url, self.name()).await?;
        if !response.is_success() {
            return Err(Error::message(response.failure_message()));
        }

        let content: Value = response.json()?;
        if content.is_array() {
            return Ok(serde_json::from_value(content)?);
        }
        if let Some(kind) = content.get("type").and_then(Value::as_str) {
            if kind == "file" {
                return Err(Error::message(
                    "The provided path points to a file. Use github_get_file_contents instead.",
                ));
            }
            return Err(Error::message(format!(
                "The provided path is not a directory (type: {kind})"
            )));
        }
        Err(Error::message(
            "Unexpected response from GitHub Contents API (expected an array for directory listing)",
        ))
    }
}

fn format_entry(entry: &DirectoryEntry) -> String {
    let mut lines = vec![
        format!("- Name: {}", entry.name),
        format!("- Path: {}", entry.path),
        format!("- Type: {}", entry.kind),
    ];
    if let Some(size) = entry.size {
        lines.push(format!("- Size: {size} bytes"));
    }
    lines.push(format!("- SHA: {}", entry.sha));
    lines.push(format!(
        "- URL: {}",
        entry.html_url.as_deref().unwrap_or("null")
    ));
    lines.join("\n")
}

fn format_directory_listing(
    input: &GetDirectoryContentsInput,
    path: Option<&str>,
    entries: &[DirectoryEntry],
) -> String {
    let mut header_lines = vec![
        "GitHub Directory Contents".to_string(),
        String::new(),
        format!("- Repo: {}/{}", input.owner.trim(), input.repo.trim()),
        format!("- Path: {}", path.unwrap_or("/")),
    ];
    if let Some(reference) = input.normalized_ref() {
        header_lines.push(format!("- Ref: {reference}"));
    }
    header_lines.push(format!("- Entries: {}", entries.len()));

    if entries.is_empty() {
        return format!("{}\n\n(empty directory)", header_lines.join("\n"));
    }
    let entries = entries
        .iter()
        .map(format_entry)
        .collect::<Vec<_>>()
        .join("\n----------\n");
    format!("{}\n\n{entries}", header_lines.join("\n"))
}

#[async_trait]
impl AgentTool for GithubGetDirectoryContentsTool {
    fn name(&self) -> &str {
        "github_get_directory_contents"
    }

    fn description(&self) -> &str {
        "List directory contents from a GitHub repository via GitHub Contents API."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "additionalProperties": false,
            "required": ["owner", "repo"],
            "properties": {
                "owner": {
                    "type": "string",
                    "minLength": 1,
                    "description": "Repository owner (organization or user)."
                },
                "repo": {
                    "type": "string",
                    "minLength": 1,
                    "description": "Repository name."
                },
                "path": {
                    "type": "string",
                    "description": "Path to the directory within the repository. Omit or pass empty for repository root."
                },
                "ref": {
                    "type": "string",
                    "description": "The name of the commit/branch/tag."
                }
            }
        })
    }

    fn validate(&self, params: &Value) -> anyhow::Result<()> {
        parse_input(params.clone())?;
        Ok(())
    }

    async fn execute(&self, params: Value) -> anyhow::Result<Value> {
        let result = self.run(params).await;
        record_execution(self.name(), result.is_ok());
        Ok(Value::String(result?))
    }
}

impl GithubGetDirectoryContentsTool {
    async fn run(&self, params: Value) -> Result<String> {
        let input = parse_input(params)?;
        let path = input.normalized_path();
        let entries = self.get_directory(&input, path.as_deref()).await?;
        Ok(format_directory_listing(&input, path.as_deref(), &entries))
    }
}

fn parse_input(params: Value) -> Result<GetDirectoryContentsInput> {
    let input: GetDirectoryContentsInput = parse_params("github_get_directory_contents", params)?;
    input.validate()?;
    Ok(input)
}

#[cfg(test)]
mod tests {
    use {super::*, secrecy::Secret};

    fn tool(base_url: String, token: Option<&str>) -> GithubGetDirectoryContentsTool {
        GithubGetDirectoryContentsTool::new(Arc::new(GitHubClient::for_test(
            base_url,
            token.map(|value| Secret::new(value.to_string())),
        )))
    }

    fn directory_body() -> String {
        json!([
            {
                "type": "file",
                "size": 11,
                "name": "README.md",
                "path": "docs/README.md",
                "sha": "sha-1",
                "url": "https://api.github.com/repos/o/r/contents/docs/README.md",
                "html_url": "https://github.com/o/r/blob/main/docs/README.md"
            },
            {
                "type": "dir",
                "size": 0,
                "name": "guides",
                "path": "docs/guides",
                "sha": "sha-2",
                "url": "https://api.github.com/repos/o/r/contents/docs/guides",
                "html_url": "https://github.com/o/r/tree/main/docs/guides"
            }
        ])
        .to_string()
    }

    #[test]
    fn exposes_the_documented_description_and_a_strict_schema() {
        let tool = tool("http://127.0.0.1:1".into(), None);

        assert_eq!(tool.name(), "github_get_directory_contents");
        assert_eq!(
            tool.description(),
            "List directory contents from a GitHub repository via GitHub Contents API."
        );
        let schema = tool.parameters_schema();
        assert_eq!(schema["additionalProperties"], false);
        assert_eq!(schema["required"], json!(["owner", "repo"]));
        assert_eq!(
            schema["properties"]["owner"]["description"],
            "Repository owner (organization or user)."
        );
        assert_eq!(
            schema["properties"]["repo"]["description"],
            "Repository name."
        );
        assert_eq!(
            schema["properties"]["path"]["description"],
            "Path to the directory within the repository. Omit or pass empty for repository root."
        );
        assert_eq!(
            schema["properties"]["ref"]["description"],
            "The name of the commit/branch/tag."
        );
    }

    #[test]
    fn validates_required_fields_and_normalizes_optional_values() {
        for (params, expected) in [
            (json!({ "owner": " ", "repo": "r" }), "owner"),
            (json!({ "owner": "o", "repo": "" }), "repo"),
        ] {
            let message = parse_input(params)
                .err()
                .map(|error| error.to_string())
                .unwrap_or_default();
            assert_eq!(message, format!("Missing required parameter: {expected}"));
        }
        assert!(parse_input(json!({ "owner": "o", "repo": "r", "branch": "main" })).is_err());

        let input = parse_input(json!({
            "owner": "o",
            "repo": "r",
            "path": "  /docs/guides///  ",
            "ref": "  main  ",
            "_session_key": "session:test"
        }))
        .unwrap_or_else(|error| panic!("parse failed: {error}"));
        assert_eq!(input.normalized_path().as_deref(), Some("docs/guides"));
        assert_eq!(input.normalized_ref(), Some("main"));

        let root = parse_input(json!({ "owner": "o", "repo": "r", "path": "///" }))
            .unwrap_or_else(|error| panic!("parse failed: {error}"));
        assert_eq!(root.normalized_path(), None);
    }

    #[tokio::test]
    async fn renders_the_reference_layout_without_a_token() {
        let mut server = mockito::Server::new_async().await;
        let call = server
            .mock("GET", "/repos/o/r/contents/docs")
            .match_query(mockito::Matcher::UrlEncoded("ref".into(), "main".into()))
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(directory_body())
            .expect(1)
            .create_async()
            .await;

        let result = tool(server.url(), None)
            .execute(json!({
                "owner": "o",
                "repo": "r",
                "path": "/docs/",
                "ref": "main"
            }))
            .await
            .unwrap_or_else(|error| panic!("execute failed: {error}"));

        assert_eq!(
            result,
            json!(
                "GitHub Directory Contents\n\n- Repo: o/r\n- Path: docs\n- Ref: main\n- Entries: 2\n\n\
                 - Name: README.md\n- Path: docs/README.md\n- Type: file\n- Size: 11 bytes\n- SHA: sha-1\n- URL: https://github.com/o/r/blob/main/docs/README.md\n\
                 ----------\n\
                 - Name: guides\n- Path: docs/guides\n- Type: dir\n- Size: 0 bytes\n- SHA: sha-2\n- URL: https://github.com/o/r/tree/main/docs/guides"
            )
        );
        call.assert_async().await;
    }

    #[tokio::test]
    async fn external_submodule_with_null_url_uses_the_reference_layout() {
        let mut server = mockito::Server::new_async().await;
        let call = server
            .mock("GET", "/repos/o/r/contents/vendor")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                json!([{
                    "type": "submodule",
                    "size": 0,
                    "name": "external",
                    "path": "vendor/external",
                    "sha": "sha-submodule",
                    "html_url": null
                }])
                .to_string(),
            )
            .expect(1)
            .create_async()
            .await;

        let result = tool(server.url(), None)
            .execute(json!({ "owner": "o", "repo": "r", "path": "vendor" }))
            .await
            .unwrap_or_else(|error| panic!("execute failed: {error}"));

        assert_eq!(
            result,
            json!(
                "GitHub Directory Contents\n\n- Repo: o/r\n- Path: vendor\n- Entries: 1\n\n\
                 - Name: external\n- Path: vendor/external\n- Type: submodule\n- Size: 0 bytes\n\
                 - SHA: sha-submodule\n- URL: null"
            )
        );
        call.assert_async().await;
    }

    #[tokio::test]
    async fn root_and_empty_directories_use_the_reference_layout() {
        let mut server = mockito::Server::new_async().await;
        let call = server
            .mock("GET", "/repos/o/r/contents")
            .match_query(mockito::Matcher::Missing)
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body("[]")
            .expect(1)
            .create_async()
            .await;

        let result = tool(server.url(), None)
            .execute(json!({ "owner": "o", "repo": "r", "path": "///" }))
            .await
            .unwrap_or_else(|error| panic!("execute failed: {error}"));

        assert_eq!(
            result,
            json!(
                "GitHub Directory Contents\n\n- Repo: o/r\n- Path: /\n- Entries: 0\n\n(empty directory)"
            )
        );
        call.assert_async().await;
    }

    #[tokio::test]
    async fn file_paths_report_the_reference_error() {
        let mut server = mockito::Server::new_async().await;
        let call = server
            .mock("GET", "/repos/o/r/contents/README.md")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(json!({ "type": "file" }).to_string())
            .expect(1)
            .create_async()
            .await;

        let error = match tool(server.url(), None)
            .execute(json!({ "owner": "o", "repo": "r", "path": "README.md" }))
            .await
        {
            Ok(_) => panic!("expected a file-path error"),
            Err(error) => error,
        };

        assert_eq!(
            error.to_string(),
            "The provided path points to a file. Use github_get_file_contents instead."
        );
        call.assert_async().await;
    }

    #[tokio::test]
    async fn non_directory_shapes_report_the_reference_errors() {
        for (body, expected) in [
            (
                json!({ "type": "symlink" }).to_string(),
                "The provided path is not a directory (type: symlink)",
            ),
            (
                json!({ "message": "unexpected" }).to_string(),
                "Unexpected response from GitHub Contents API (expected an array for directory listing)",
            ),
        ] {
            let mut server = mockito::Server::new_async().await;
            let call = server
                .mock("GET", "/repos/o/r/contents/docs")
                .with_status(200)
                .with_header("content-type", "application/json")
                .with_body(body)
                .expect(1)
                .create_async()
                .await;

            let error = match tool(server.url(), None)
                .execute(json!({ "owner": "o", "repo": "r", "path": "docs" }))
                .await
            {
                Ok(_) => panic!("expected a directory-shape error"),
                Err(error) => error,
            };

            assert_eq!(error.to_string(), expected);
            call.assert_async().await;
        }
    }

    #[tokio::test]
    async fn unauthenticated_access_denial_reports_the_missing_token() {
        let mut server = mockito::Server::new_async().await;
        let call = server
            .mock("GET", "/repos/o/private/contents")
            .with_status(403)
            .with_body("{\"message\":\"Resource not accessible\"}")
            .expect(1)
            .create_async()
            .await;

        let error = match tool(server.url(), None)
            .execute(json!({ "owner": "o", "repo": "private" }))
            .await
        {
            Ok(_) => panic!("expected a missing token error"),
            Err(error) => error,
        };

        assert_eq!(
            error.to_string(),
            "GitHub personal access token is not configured: set `tools.github.pat` in chelix.toml"
        );
        call.assert_async().await;
    }

    #[tokio::test]
    async fn authorization_failure_with_a_token_is_an_error() {
        let mut server = mockito::Server::new_async().await;
        let call = server
            .mock("GET", "/repos/o/private/contents")
            .with_status(401)
            .with_body("{\"message\":\"Bad credentials\"}")
            .expect(1)
            .create_async()
            .await;

        let error = match tool(server.url(), Some("pat-token"))
            .execute(json!({ "owner": "o", "repo": "private" }))
            .await
        {
            Ok(_) => panic!("expected an authorization error"),
            Err(error) => error,
        };

        assert_eq!(error.to_string(), "{\"message\":\"Bad credentials\"}");
        call.assert_async().await;
    }

    #[tokio::test(start_paused = true)]
    async fn rate_limited_response_is_retried_once_after_the_cooldown() {
        let mut server = mockito::Server::new_async().await;
        let limited = server
            .mock("GET", "/repos/o/r/contents")
            .with_status(429)
            .with_header("retry-after", "1")
            .with_body("API rate limit exceeded")
            .expect(1)
            .create_async()
            .await;
        let retried = server
            .mock("GET", "/repos/o/r/contents")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body("[]")
            .expect(1)
            .create_async()
            .await;

        let result = tool(server.url(), None)
            .execute(json!({ "owner": "o", "repo": "r" }))
            .await
            .unwrap_or_else(|error| panic!("execute failed: {error}"));

        assert_eq!(
            result,
            json!(
                "GitHub Directory Contents\n\n- Repo: o/r\n- Path: /\n- Entries: 0\n\n(empty directory)"
            )
        );
        limited.assert_async().await;
        retried.assert_async().await;
    }
}
