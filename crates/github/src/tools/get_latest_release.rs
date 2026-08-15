//! `github_get_latest_release` — retrieve the latest repository release through
//! `GET /repos/{owner}/{repo}/releases/latest`.

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
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct GetLatestReleaseInput {
    owner: String,
    repo: String,
}

impl GetLatestReleaseInput {
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
}

#[derive(Debug, Clone, Deserialize)]
struct ReleaseItem {
    tag_name: String,
    name: Option<String>,
    draft: bool,
    prerelease: bool,
    published_at: Option<String>,
    html_url: String,
}

/// Retrieve the latest release for a GitHub repository.
pub struct GithubGetLatestReleaseTool {
    client: Arc<GitHubClient>,
}

impl GithubGetLatestReleaseTool {
    #[must_use]
    pub fn new(client: Arc<GitHubClient>) -> Self {
        Self { client }
    }

    async fn get_latest_release(&self, input: &GetLatestReleaseInput) -> Result<ReleaseItem> {
        let mut url = url::Url::parse(&format!("{}/repos/", self.client.base_url()))?;
        {
            let mut segments = url
                .path_segments_mut()
                .map_err(|()| Error::message("GitHub API base URL cannot have path segments"))?;
            segments.pop_if_empty();
            segments.push(input.owner.trim());
            segments.push(input.repo.trim());
            segments.push("releases");
            segments.push("latest");
        }

        let response = get_with_rate_limit_retry(&self.client, &url, self.name()).await?;
        if !response.is_success() {
            return Err(Error::message(response.failure_message()));
        }
        response.json()
    }
}

fn format_release(owner: &str, repo: &str, release: &ReleaseItem) -> String {
    let mut lines = vec![
        format!("Latest GitHub Release for {owner}/{repo}"),
        String::new(),
    ];
    lines.push(format!("- Tag: {}", release.tag_name));
    if let Some(name) = release.name.as_deref().filter(|name| !name.is_empty()) {
        lines.push(format!("- Name: {name}"));
    }
    lines.push(format!("- Draft: {}", release.draft));
    lines.push(format!("- Pre-release: {}", release.prerelease));
    lines.push(format!(
        "- Published: {}",
        release.published_at.as_deref().unwrap_or("N/A")
    ));
    lines.push(format!("- URL: {}", release.html_url));
    lines.join("\n")
}

#[async_trait]
impl AgentTool for GithubGetLatestReleaseTool {
    fn name(&self) -> &str {
        "github_get_latest_release"
    }

    fn description(&self) -> &str {
        "Retrieve latest release info for a repository via GitHub API."
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

impl GithubGetLatestReleaseTool {
    async fn run(&self, params: Value) -> Result<String> {
        let input = parse_input(params)?;
        let owner = input.owner.trim();
        let repo = input.repo.trim();
        let release = self.get_latest_release(&input).await?;
        Ok(format_release(owner, repo, &release))
    }
}

fn parse_input(params: Value) -> Result<GetLatestReleaseInput> {
    let input: GetLatestReleaseInput = parse_params("github_get_latest_release", params)?;
    input.validate()?;
    Ok(input)
}

#[cfg(test)]
mod tests {
    use {
        super::*,
        chelix_agents::tool_registry::{ToolResultPersistence, Truncation},
        secrecy::Secret,
    };

    fn tool(base_url: String, token: Option<&str>) -> GithubGetLatestReleaseTool {
        GithubGetLatestReleaseTool::new(Arc::new(GitHubClient::for_test(
            base_url,
            token.map(|value| Secret::new(value.to_string())),
        )))
    }

    #[test]
    fn exposes_the_reference_description_and_a_strict_schema() {
        let tool = tool("http://127.0.0.1:1".into(), None);

        assert_eq!(tool.name(), "github_get_latest_release");
        assert_eq!(
            tool.description(),
            "Retrieve latest release info for a repository via GitHub API."
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
        assert_eq!(tool.truncation(&json!({})), Truncation::Standard);
        assert_eq!(
            tool.result_persistence(&json!({})),
            ToolResultPersistence::On
        );
    }

    fn parse_error(params: Value) -> String {
        parse_input(params)
            .err()
            .map(|error| error.to_string())
            .unwrap_or_default()
    }

    #[test]
    fn validates_required_fields_and_rejects_unknown_fields() {
        assert_eq!(
            parse_error(json!({ "owner": " ", "repo": "r" })),
            "Missing required parameter: owner"
        );
        assert_eq!(
            parse_error(json!({ "owner": "o", "repo": "" })),
            "Missing required parameter: repo"
        );
        assert!(parse_input(json!({ "owner": "o", "repo": "r", "unknown": true })).is_err());
    }

    #[test]
    fn ignores_enriched_internal_fields() {
        let input = parse_input(json!({
            "owner": "o",
            "repo": "r",
            "_session_key": "session:test",
            "_channel": { "surface": "web" }
        }))
        .unwrap_or_else(|error| panic!("parse failed: {error}"));

        assert_eq!(input.owner, "o");
        assert_eq!(input.repo, "r");
    }

    #[tokio::test]
    async fn requests_latest_and_formats_the_reference_layout() {
        let mut server = mockito::Server::new_async().await;
        let call = server
            .mock("GET", "/repos/owner/repo/releases/latest")
            .match_query(mockito::Matcher::Missing)
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                json!({
                    "tag_name": "v2.0.0",
                    "name": "Version 2.0",
                    "draft": false,
                    "prerelease": false,
                    "created_at": "2026-08-01T00:00:00Z",
                    "published_at": "2026-08-02T00:00:00Z",
                    "html_url": "https://github.com/owner/repo/releases/tag/v2.0.0"
                })
                .to_string(),
            )
            .expect(1)
            .create_async()
            .await;

        let result = tool(server.url(), None)
            .execute(json!({ "owner": " owner ", "repo": " repo " }))
            .await
            .unwrap_or_else(|error| panic!("execute failed: {error}"));

        assert_eq!(
            result,
            json!(
                "Latest GitHub Release for owner/repo\n\n- Tag: v2.0.0\n- Name: Version 2.0\n- Draft: false\n- Pre-release: false\n- Published: 2026-08-02T00:00:00Z\n- URL: https://github.com/owner/repo/releases/tag/v2.0.0"
            )
        );
        call.assert_async().await;
    }

    #[test]
    fn null_name_and_published_date_match_the_reference_layout() {
        let release = ReleaseItem {
            tag_name: "v2.0.0-rc1".to_string(),
            name: None,
            draft: true,
            prerelease: true,
            published_at: None,
            html_url: "https://github.com/o/r/releases/tag/v2.0.0-rc1".to_string(),
        };

        assert_eq!(
            format_release("o", "r", &release),
            "Latest GitHub Release for o/r\n\n- Tag: v2.0.0-rc1\n- Draft: true\n- Pre-release: true\n- Published: N/A\n- URL: https://github.com/o/r/releases/tag/v2.0.0-rc1"
        );
    }

    #[tokio::test]
    async fn unsuccessful_response_propagates_the_github_error_body() {
        let mut server = mockito::Server::new_async().await;
        let call = server
            .mock("GET", "/repos/o/r/releases/latest")
            .with_status(404)
            .with_body("{\"message\":\"Not Found\"}")
            .expect(1)
            .create_async()
            .await;

        let error = match tool(server.url(), None)
            .execute(json!({ "owner": "o", "repo": "r" }))
            .await
        {
            Ok(_) => panic!("expected a retrieval error"),
            Err(error) => error,
        };

        assert_eq!(error.to_string(), "{\"message\":\"Not Found\"}");
        call.assert_async().await;
    }

    #[tokio::test(start_paused = true)]
    async fn rate_limited_response_is_retried_once_after_the_cooldown() {
        let mut server = mockito::Server::new_async().await;
        let limited = server
            .mock("GET", "/repos/o/r/releases/latest")
            .with_status(429)
            .with_header("retry-after", "1")
            .with_body("API rate limit exceeded")
            .expect(1)
            .create_async()
            .await;
        let retried = server
            .mock("GET", "/repos/o/r/releases/latest")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                json!({
                    "tag_name": "v1.0.0",
                    "name": null,
                    "draft": false,
                    "prerelease": false,
                    "created_at": "2026-08-01T00:00:00Z",
                    "published_at": null,
                    "html_url": "https://github.com/o/r/releases/tag/v1.0.0"
                })
                .to_string(),
            )
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
                "Latest GitHub Release for o/r\n\n- Tag: v1.0.0\n- Draft: false\n- Pre-release: false\n- Published: N/A\n- URL: https://github.com/o/r/releases/tag/v1.0.0"
            )
        );
        limited.assert_async().await;
        retried.assert_async().await;
    }

    #[tokio::test]
    async fn rate_limit_without_timing_propagates_the_github_error() {
        let mut server = mockito::Server::new_async().await;
        let call = server
            .mock("GET", "/repos/o/r/releases/latest")
            .with_status(429)
            .with_body("You have exceeded a secondary rate limit")
            .expect(1)
            .create_async()
            .await;

        let error = match tool(server.url(), None)
            .execute(json!({ "owner": "o", "repo": "r" }))
            .await
        {
            Ok(_) => panic!("expected a rate-limit error"),
            Err(error) => error,
        };

        assert_eq!(
            error.to_string(),
            "You have exceeded a secondary rate limit"
        );
        call.assert_async().await;
    }
}
