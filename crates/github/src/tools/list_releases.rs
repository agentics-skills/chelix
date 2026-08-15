//! `github_list_releases` — list repository releases through
//! `GET /repos/{owner}/{repo}/releases`.

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

/// Maximum `per_page` accepted by the GitHub releases API.
const MAX_PER_PAGE: u32 = 100;

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ListReleasesInput {
    owner: String,
    repo: String,
    #[serde(default)]
    per_page: Option<u32>,
}

impl ListReleasesInput {
    fn validate(&self) -> Result<()> {
        for (name, value) in [("owner", &self.owner), ("repo", &self.repo)] {
            if value.trim().is_empty() {
                return Err(Error::message(format!(
                    "Missing required parameter: {name}"
                )));
            }
        }
        if let Some(per_page) = self.per_page
            && (per_page == 0 || per_page > MAX_PER_PAGE)
        {
            return Err(Error::message(format!(
                "Invalid parameter: perPage must be between 1 and {MAX_PER_PAGE}"
            )));
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

/// List releases for a GitHub repository.
pub struct GithubListReleasesTool {
    client: Arc<GitHubClient>,
}

impl GithubListReleasesTool {
    #[must_use]
    pub fn new(client: Arc<GitHubClient>) -> Self {
        Self { client }
    }

    async fn list_releases(&self, input: &ListReleasesInput) -> Result<Vec<ReleaseItem>> {
        let mut url = url::Url::parse(&format!("{}/repos/", self.client.base_url()))?;
        {
            let mut segments = url
                .path_segments_mut()
                .map_err(|()| Error::message("GitHub API base URL cannot have path segments"))?;
            segments.pop_if_empty();
            segments.push(input.owner.trim());
            segments.push(input.repo.trim());
            segments.push("releases");
        }
        if let Some(per_page) = input.per_page {
            url.query_pairs_mut()
                .append_pair("per_page", &per_page.to_string());
        }

        let response = get_with_rate_limit_retry(&self.client, &url, self.name()).await?;
        if !response.is_success() {
            return Err(Error::message(response.failure_message()));
        }
        response.json()
    }
}

fn format_release(release: &ReleaseItem) -> String {
    let mut lines = vec![format!("- Tag: {}", release.tag_name)];
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

fn format_results(owner: &str, repo: &str, items: &[ReleaseItem]) -> String {
    if items.is_empty() {
        return format!("No releases found for {owner}/{repo}.");
    }
    let header = format!(
        "GitHub Releases for {owner}/{repo} (showing {})",
        items.len()
    );
    let releases = items
        .iter()
        .map(format_release)
        .collect::<Vec<_>>()
        .join("\n----------\n");
    format!("{header}\n\n{releases}")
}

#[async_trait]
impl AgentTool for GithubListReleasesTool {
    fn name(&self) -> &str {
        "github_list_releases"
    }

    fn description(&self) -> &str {
        "List releases for a repository via GitHub API."
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
                "perPage": {
                    "type": "integer",
                    "minimum": 1,
                    "maximum": MAX_PER_PAGE,
                    "description": "Number of items per page (max 100)."
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

impl GithubListReleasesTool {
    async fn run(&self, params: Value) -> Result<String> {
        let input = parse_input(params)?;
        let owner = input.owner.trim();
        let repo = input.repo.trim();
        let items = self.list_releases(&input).await?;
        Ok(format_results(owner, repo, &items))
    }
}

fn parse_input(params: Value) -> Result<ListReleasesInput> {
    let input: ListReleasesInput = parse_params("github_list_releases", params)?;
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

    fn tool(base_url: String, token: Option<&str>) -> GithubListReleasesTool {
        GithubListReleasesTool::new(Arc::new(GitHubClient::for_test(
            base_url,
            token.map(|value| Secret::new(value.to_string())),
        )))
    }

    #[test]
    fn exposes_the_reference_description_and_a_strict_schema() {
        let tool = tool("http://127.0.0.1:1".into(), None);

        assert_eq!(tool.name(), "github_list_releases");
        assert_eq!(
            tool.description(),
            "List releases for a repository via GitHub API."
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
            schema["properties"]["perPage"]["description"],
            "Number of items per page (max 100)."
        );
        assert_eq!(schema["properties"]["perPage"]["type"], "integer");
        assert_eq!(schema["properties"]["perPage"]["minimum"], 1);
        assert_eq!(schema["properties"]["perPage"]["maximum"], 100);
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
    fn validates_required_fields_and_camel_case_page_size() {
        assert_eq!(
            parse_error(json!({ "owner": " ", "repo": "r" })),
            "Missing required parameter: owner"
        );
        assert_eq!(
            parse_error(json!({ "owner": "o", "repo": "" })),
            "Missing required parameter: repo"
        );
        assert!(parse_input(json!({ "owner": "o", "repo": "r", "per_page": 1 })).is_err());
        assert!(parse_input(json!({ "owner": "o", "repo": "r", "perPage": 0 })).is_err());
        assert!(parse_input(json!({ "owner": "o", "repo": "r", "perPage": 101 })).is_err());
        assert!(parse_input(json!({ "owner": "o", "repo": "r", "perPage": 1.5 })).is_err());
        assert!(parse_input(json!({ "owner": "o", "repo": "r", "unknown": true })).is_err());
    }

    #[test]
    fn ignores_enriched_internal_fields() {
        let input = parse_input(json!({
            "owner": "o",
            "repo": "r",
            "perPage": 5,
            "_session_key": "session:test",
            "_channel": { "surface": "web" }
        }))
        .unwrap_or_else(|error| panic!("parse failed: {error}"));

        assert_eq!(input.per_page, Some(5));
    }

    #[tokio::test]
    async fn sends_the_page_size_and_formats_the_reference_layout() {
        let mut server = mockito::Server::new_async().await;
        let call = server
            .mock("GET", "/repos/owner/repo/releases")
            .match_query(mockito::Matcher::UrlEncoded("per_page".into(), "2".into()))
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                json!([
                    {
                        "tag_name": "v2.0.0",
                        "name": "Version 2.0",
                        "draft": false,
                        "prerelease": false,
                        "created_at": "2026-08-01T00:00:00Z",
                        "published_at": "2026-08-02T00:00:00Z",
                        "html_url": "https://github.com/owner/repo/releases/tag/v2.0.0"
                    },
                    {
                        "tag_name": "v2.0.0-rc1",
                        "name": null,
                        "draft": true,
                        "prerelease": true,
                        "created_at": "2026-07-01T00:00:00Z",
                        "published_at": null,
                        "html_url": "https://github.com/owner/repo/releases/tag/v2.0.0-rc1"
                    }
                ])
                .to_string(),
            )
            .expect(1)
            .create_async()
            .await;

        let result = tool(server.url(), None)
            .execute(json!({
                "owner": " owner ",
                "repo": " repo ",
                "perPage": 2
            }))
            .await
            .unwrap_or_else(|error| panic!("execute failed: {error}"));

        assert_eq!(
            result,
            json!(
                "GitHub Releases for owner/repo (showing 2)\n\n\
                 - Tag: v2.0.0\n- Name: Version 2.0\n- Draft: false\n- Pre-release: false\n- Published: 2026-08-02T00:00:00Z\n- URL: https://github.com/owner/repo/releases/tag/v2.0.0\n\
                 ----------\n\
                 - Tag: v2.0.0-rc1\n- Draft: true\n- Pre-release: true\n- Published: N/A\n- URL: https://github.com/owner/repo/releases/tag/v2.0.0-rc1"
            )
        );
        call.assert_async().await;
    }

    #[tokio::test]
    async fn empty_result_set_uses_the_reference_message() {
        let mut server = mockito::Server::new_async().await;
        let call = server
            .mock("GET", "/repos/o/r/releases")
            .match_query(mockito::Matcher::Missing)
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

        assert_eq!(result, json!("No releases found for o/r."));
        call.assert_async().await;
    }

    #[tokio::test]
    async fn unsuccessful_response_propagates_the_github_error_body() {
        let mut server = mockito::Server::new_async().await;
        let call = server
            .mock("GET", "/repos/o/r/releases")
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
            .mock("GET", "/repos/o/r/releases")
            .with_status(429)
            .with_header("retry-after", "1")
            .with_body("API rate limit exceeded")
            .expect(1)
            .create_async()
            .await;
        let retried = server
            .mock("GET", "/repos/o/r/releases")
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

        assert_eq!(result, json!("No releases found for o/r."));
        limited.assert_async().await;
        retried.assert_async().await;
    }

    #[tokio::test]
    async fn rate_limit_without_timing_propagates_the_github_error() {
        let mut server = mockito::Server::new_async().await;
        let call = server
            .mock("GET", "/repos/o/r/releases")
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
