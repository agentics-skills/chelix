//! `github_get_file_contents` — read one file from a GitHub repository via
//! `GET /repos/{owner}/{repo}/contents/{path}`.

use std::sync::Arc;

use {
    async_trait::async_trait,
    base64::{Engine, engine::general_purpose::STANDARD as BASE64},
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

/// Invisible separator placed between the header and the file body.
const CONTENT_SEPARATOR: &str = "\u{2063}";

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
struct GetFileContentsInput {
    owner: String,
    repo: String,
    path: String,
    #[serde(default)]
    r#ref: Option<String>,
}

impl GetFileContentsInput {
    fn validate(&self) -> Result<()> {
        for (name, value) in [
            ("owner", &self.owner),
            ("repo", &self.repo),
            ("path", &self.path),
        ] {
            if value.trim().is_empty() {
                return Err(Error::message(format!(
                    "Missing required parameter: {name}"
                )));
            }
        }
        Ok(())
    }

    /// Trimmed `ref`, or `None` when it is absent or blank.
    fn normalized_ref(&self) -> Option<&str> {
        self.r#ref
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
    }
}

/// File entry returned by `GET /repos/{owner}/{repo}/contents/{path}`.
#[derive(Debug, Clone, Deserialize)]
struct FileContent {
    #[serde(default)]
    encoding: Option<String>,
    size: u64,
    name: String,
    path: String,
    #[serde(default)]
    content: Option<String>,
    sha: String,
    html_url: String,
}

/// Read one file from a GitHub repository.
pub struct GithubGetFileContentsTool {
    client: Arc<GitHubClient>,
}

impl GithubGetFileContentsTool {
    #[must_use]
    pub fn new(client: Arc<GitHubClient>) -> Self {
        Self { client }
    }

    /// Fetch the file entry, or `None` when GitHub does not return a readable
    /// file for the request.
    async fn get_file(&self, input: &GetFileContentsInput) -> Result<Option<FileContent>> {
        let mut url = url::Url::parse(&format!("{}/repos/", self.client.base_url()))?;
        {
            let mut segments = url
                .path_segments_mut()
                .map_err(|()| Error::message("GitHub API base URL cannot have path segments"))?;
            segments.pop_if_empty();
            segments.push(input.owner.trim());
            segments.push(input.repo.trim());
            segments.push("contents");
            segments.extend(input.path.trim().split('/'));
        }
        if let Some(reference) = input.normalized_ref() {
            url.query_pairs_mut().append_pair("ref", reference);
        }

        let response = get_with_rate_limit_retry(&self.client, &url, self.name()).await?;
        if response.is_rate_limited() {
            return Err(Error::message(response.failure_message()));
        }
        if !response.is_success() {
            return Ok(None);
        }
        // A directory answers with an array and a symlink or submodule answers
        // with another `type`. Every non-file shape is reported through the
        // caller's "not found or unsupported type" error.
        let content: Value = response.json()?;
        if content.get("type").and_then(Value::as_str) != Some("file") {
            return Ok(None);
        }
        Ok(Some(serde_json::from_value(content)?))
    }
}

/// Decode base64 content, tolerating the line breaks GitHub inserts.
fn decode_base64(encoded: &str) -> Option<String> {
    let normalized: String = encoded.chars().filter(|c| *c != '\n').collect();
    BASE64
        .decode(normalized)
        .ok()
        .and_then(|bytes| String::from_utf8(bytes).ok())
}

fn format_result(input: &GetFileContentsInput, file: &FileContent, content: &str) -> String {
    let reference = input
        .normalized_ref()
        .map(|value| format!("\nRef: {value}"))
        .unwrap_or_default();
    let header = format!(
        "# {}\n\nRepository: {}/{}\nPath: {}{reference}\nSize: {} bytes\nSHA: {}\nURL: {}",
        file.name,
        input.owner.trim(),
        input.repo.trim(),
        file.path,
        file.size,
        file.sha,
        file.html_url
    );
    format!("{header}\n\n\n{CONTENT_SEPARATOR}\n\n\n~~~\n{content}\n~~~")
}

#[async_trait]
impl AgentTool for GithubGetFileContentsTool {
    fn name(&self) -> &str {
        "github_get_file_contents"
    }

    fn description(&self) -> &str {
        "Retrieve the contents of a file from a GitHub repository."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "additionalProperties": false,
            "required": ["owner", "repo", "path"],
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
                    "minLength": 1,
                    "description": "Path to the file within the repository."
                },
                "ref": {
                    "type": "string",
                    "minLength": 1,
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

impl GithubGetFileContentsTool {
    async fn run(&self, params: Value) -> Result<String> {
        let input = parse_input(params)?;
        self.client.require_token()?;
        let file = self.get_file(&input).await?.ok_or_else(|| {
            Error::message(
                "Failed to retrieve file content from GitHub (not found or unsupported type)",
            )
        })?;
        let content = match (file.encoding.as_deref(), file.content.as_deref()) {
            (Some("base64"), Some(encoded)) => decode_base64(encoded).unwrap_or_default(),
            _ => String::new(),
        };
        if content.is_empty() {
            return Err(Error::message(
                "Unsupported or empty file content returned by GitHub API",
            ));
        }
        Ok(format_result(&input, &file, &content))
    }
}

fn parse_input(params: Value) -> Result<GetFileContentsInput> {
    let input: GetFileContentsInput = parse_params("github_get_file_contents", params)?;
    input.validate()?;
    Ok(input)
}

#[cfg(test)]
mod tests {
    use {super::*, secrecy::Secret};

    fn tool(base_url: String, token: Option<&str>) -> GithubGetFileContentsTool {
        GithubGetFileContentsTool::new(Arc::new(GitHubClient::for_test(
            base_url,
            token.map(|value| Secret::new(value.to_string())),
        )))
    }

    fn file_body(content: &str) -> String {
        json!({
            "type": "file",
            "encoding": "base64",
            "size": 11,
            "name": "README.md",
            "path": "docs/README.md",
            "content": BASE64.encode(content),
            "sha": "sha-1",
            "url": "https://api.github.com/repos/o/r/contents/docs/README.md",
            "html_url": "https://github.com/o/r/blob/main/docs/README.md"
        })
        .to_string()
    }

    #[test]
    fn exposes_the_documented_description_and_a_strict_schema() {
        let tool = tool("http://127.0.0.1:1".into(), None);

        assert_eq!(tool.name(), "github_get_file_contents");
        assert_eq!(
            tool.description(),
            "Retrieve the contents of a file from a GitHub repository."
        );
        let schema = tool.parameters_schema();
        assert_eq!(schema["additionalProperties"], false);
        assert_eq!(schema["required"], json!(["owner", "repo", "path"]));
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
            "Path to the file within the repository."
        );
        assert_eq!(
            schema["properties"]["ref"]["description"],
            "The name of the commit/branch/tag."
        );
    }

    #[test]
    fn rejects_blank_required_fields_and_unknown_fields() {
        for (params, expected) in [
            (json!({ "owner": " ", "repo": "r", "path": "p" }), "owner"),
            (json!({ "owner": "o", "repo": "", "path": "p" }), "repo"),
            (json!({ "owner": "o", "repo": "r", "path": " " }), "path"),
        ] {
            let message = match parse_input(params) {
                Ok(input) => panic!("expected a validation error, parsed {input:?}"),
                Err(error) => error.to_string(),
            };
            assert_eq!(message, format!("Missing required parameter: {expected}"));
        }
        assert!(
            parse_input(json!({ "owner": "o", "repo": "r", "path": "p", "branch": "main" }))
                .is_err()
        );
    }

    #[test]
    fn blank_ref_is_treated_as_absent() {
        let input = parse_input(json!({ "owner": "o", "repo": "r", "path": "p", "ref": "  " }))
            .unwrap_or_else(|error| panic!("parse failed: {error}"));

        assert_eq!(input.normalized_ref(), None);
    }

    #[tokio::test]
    async fn missing_token_fails_before_any_request() {
        let mut server = mockito::Server::new_async().await;
        let call = server
            .mock("GET", mockito::Matcher::Any)
            .expect(0)
            .create_async()
            .await;

        let error = match tool(server.url(), None)
            .execute(json!({ "owner": "o", "repo": "r", "path": "docs/README.md" }))
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
    async fn renders_the_documented_markdown_layout() {
        let mut server = mockito::Server::new_async().await;
        let call = server
            .mock("GET", "/repos/o/r/contents/docs/README.md")
            .match_query(mockito::Matcher::UrlEncoded("ref".into(), "main".into()))
            .match_header("authorization", "Bearer pat-token")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(file_body("hello world"))
            .expect(1)
            .create_async()
            .await;

        let result = tool(server.url(), Some("pat-token"))
            .execute(json!({
                "owner": "o",
                "repo": "r",
                "path": "docs/README.md",
                "ref": "main"
            }))
            .await
            .unwrap_or_else(|error| panic!("execute failed: {error}"));

        assert_eq!(
            result,
            json!(
                "# README.md\n\nRepository: o/r\nPath: docs/README.md\nRef: main\nSize: 11 bytes\n\
                 SHA: sha-1\nURL: https://github.com/o/r/blob/main/docs/README.md\n\n\n\u{2063}\n\n\n\
                 ~~~\nhello world\n~~~"
            )
        );
        call.assert_async().await;
    }

    #[tokio::test]
    async fn omits_the_ref_line_when_no_ref_is_requested() {
        let mut server = mockito::Server::new_async().await;
        let call = server
            .mock("GET", "/repos/o/r/contents/docs/README.md")
            .match_query(mockito::Matcher::Missing)
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(file_body("hello world"))
            .expect(1)
            .create_async()
            .await;

        let result = tool(server.url(), Some("pat-token"))
            .execute(json!({ "owner": "o", "repo": "r", "path": "docs/README.md" }))
            .await
            .unwrap_or_else(|error| panic!("execute failed: {error}"));

        let rendered = result.as_str().unwrap_or_default();
        assert!(!rendered.contains("Ref:"), "{rendered}");
        call.assert_async().await;
    }

    #[tokio::test]
    async fn directory_entries_are_reported_as_unsupported() {
        let mut server = mockito::Server::new_async().await;
        let call = server
            .mock("GET", "/repos/o/r/contents/docs")
            .with_status(200)
            .with_header("content-type", "application/json")
            // GitHub answers a directory request with an array of entries.
            .with_body(
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
                .to_string(),
            )
            .expect(1)
            .create_async()
            .await;

        let error = match tool(server.url(), Some("pat-token"))
            .execute(json!({ "owner": "o", "repo": "r", "path": "docs" }))
            .await
        {
            Ok(_) => panic!("expected an unsupported type error"),
            Err(error) => error,
        };

        assert_eq!(
            error.to_string(),
            "Failed to retrieve file content from GitHub (not found or unsupported type)"
        );
        call.assert_async().await;
    }

    #[tokio::test]
    async fn non_file_entry_types_are_reported_as_unsupported() {
        for entry_type in ["symlink", "submodule"] {
            let mut server = mockito::Server::new_async().await;
            let call = server
                .mock("GET", "/repos/o/r/contents/docs/link")
                .with_status(200)
                .with_header("content-type", "application/json")
                .with_body(
                    json!({
                        "type": entry_type,
                        "size": 0,
                        "name": "link",
                        "path": "docs/link",
                        "sha": "sha-1",
                        "url": "https://api.github.com/repos/o/r/contents/docs/link",
                        "html_url": "https://github.com/o/r/blob/main/docs/link"
                    })
                    .to_string(),
                )
                .expect(1)
                .create_async()
                .await;

            let error = match tool(server.url(), Some("pat-token"))
                .execute(json!({ "owner": "o", "repo": "r", "path": "docs/link" }))
                .await
            {
                Ok(_) => panic!("expected an unsupported type error for {entry_type}"),
                Err(error) => error,
            };

            assert_eq!(
                error.to_string(),
                "Failed to retrieve file content from GitHub (not found or unsupported type)"
            );
            call.assert_async().await;
        }
    }

    #[tokio::test]
    async fn empty_content_is_reported_as_unsupported() {
        let mut server = mockito::Server::new_async().await;
        let call = server
            .mock("GET", "/repos/o/r/contents/docs/EMPTY.md")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(file_body(""))
            .expect(1)
            .create_async()
            .await;

        let error = match tool(server.url(), Some("pat-token"))
            .execute(json!({ "owner": "o", "repo": "r", "path": "docs/EMPTY.md" }))
            .await
        {
            Ok(_) => panic!("expected an empty content error"),
            Err(error) => error,
        };

        assert_eq!(
            error.to_string(),
            "Unsupported or empty file content returned by GitHub API"
        );
        call.assert_async().await;
    }

    #[tokio::test]
    async fn rate_limit_without_timing_preserves_the_github_error() {
        let mut server = mockito::Server::new_async().await;
        let call = server
            .mock("GET", "/repos/o/r/contents/docs/README.md")
            .with_status(429)
            .with_body("You have exceeded a secondary rate limit")
            .expect(1)
            .create_async()
            .await;

        let error = match tool(server.url(), Some("pat-token"))
            .execute(json!({ "owner": "o", "repo": "r", "path": "docs/README.md" }))
            .await
        {
            Ok(_) => panic!("expected a rate limit error"),
            Err(error) => error,
        };

        assert_eq!(
            error.to_string(),
            "You have exceeded a secondary rate limit"
        );
        call.assert_async().await;
    }

    #[tokio::test(start_paused = true)]
    async fn repeated_rate_limit_preserves_the_last_github_error() {
        let mut server = mockito::Server::new_async().await;
        let limited = server
            .mock("GET", "/repos/o/r/contents/docs/README.md")
            .with_status(429)
            .with_header("retry-after", "1")
            .with_body("API rate limit exceeded")
            .expect(1)
            .create_async()
            .await;
        let retried = server
            .mock("GET", "/repos/o/r/contents/docs/README.md")
            .with_status(403)
            .with_body("You have exceeded a secondary rate limit")
            .expect(1)
            .create_async()
            .await;

        let error = match tool(server.url(), Some("pat-token"))
            .execute(json!({ "owner": "o", "repo": "r", "path": "docs/README.md" }))
            .await
        {
            Ok(_) => panic!("expected a repeated rate limit error"),
            Err(error) => error,
        };

        assert_eq!(
            error.to_string(),
            "You have exceeded a secondary rate limit"
        );
        limited.assert_async().await;
        retried.assert_async().await;
    }

    #[tokio::test]
    async fn authorization_failure_is_an_error() {
        let mut server = mockito::Server::new_async().await;
        let call = server
            .mock("GET", "/repos/o/r/contents/docs/README.md")
            .with_status(401)
            .with_body("{\"message\":\"Bad credentials\"}")
            .expect(1)
            .create_async()
            .await;

        let error = match tool(server.url(), Some("pat-token"))
            .execute(json!({ "owner": "o", "repo": "r", "path": "docs/README.md" }))
            .await
        {
            Ok(_) => panic!("expected an authorization error"),
            Err(error) => error,
        };

        assert_eq!(error.to_string(), "{\"message\":\"Bad credentials\"}");
        call.assert_async().await;
    }
}
