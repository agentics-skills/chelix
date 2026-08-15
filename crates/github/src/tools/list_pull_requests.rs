//! `github_list_pull_requests` — list repository pull requests through
//! `GET /repos/{owner}/{repo}/pulls`.

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

/// Maximum `per_page` accepted by the GitHub pull requests API.
const MAX_PER_PAGE: u32 = 100;

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ListPullRequestsInput {
    owner: String,
    repo: String,
    #[serde(default)]
    state: Option<String>,
    #[serde(default)]
    head: Option<String>,
    #[serde(default)]
    base: Option<String>,
    #[serde(default)]
    sort: Option<String>,
    #[serde(default)]
    direction: Option<String>,
    #[serde(default)]
    per_page: Option<u32>,
    #[serde(default)]
    page: Option<u32>,
}

impl ListPullRequestsInput {
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
        if self.page == Some(0) {
            return Err(Error::message("Invalid parameter: page must be at least 1"));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Deserialize)]
struct PullRequestListUser {
    login: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct PullRequestListBranch {
    r#ref: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct PullRequestListItem {
    number: u64,
    title: String,
    state: String,
    draft: Option<bool>,
    html_url: String,
    updated_at: String,
    merged_at: Option<String>,
    user: Option<PullRequestListUser>,
    head: Option<PullRequestListBranch>,
    base: Option<PullRequestListBranch>,
}

/// List pull requests for a GitHub repository.
pub struct GithubListPullRequestsTool {
    client: Arc<GitHubClient>,
}

impl GithubListPullRequestsTool {
    #[must_use]
    pub fn new(client: Arc<GitHubClient>) -> Self {
        Self { client }
    }

    async fn list_pull_requests(
        &self,
        input: &ListPullRequestsInput,
    ) -> Result<Vec<PullRequestListItem>> {
        let mut url = url::Url::parse(&format!("{}/repos/", self.client.base_url()))?;
        {
            let mut segments = url
                .path_segments_mut()
                .map_err(|()| Error::message("GitHub API base URL cannot have path segments"))?;
            segments.pop_if_empty();
            segments.push(input.owner.trim());
            segments.push(input.repo.trim());
            segments.push("pulls");
        }
        let filters = [
            ("state", input.state.as_deref()),
            ("head", input.head.as_deref()),
            ("base", input.base.as_deref()),
            ("sort", input.sort.as_deref()),
            ("direction", input.direction.as_deref()),
        ];
        let has_filters = filters
            .iter()
            .any(|(_, value)| value.is_some_and(|value| !value.is_empty()));
        if has_filters || input.per_page.is_some() || input.page.is_some() {
            let mut query = url.query_pairs_mut();
            for (name, value) in filters {
                if let Some(value) = value.filter(|value| !value.is_empty()) {
                    query.append_pair(name, value);
                }
            }
            if let Some(per_page) = input.per_page {
                query.append_pair("per_page", &per_page.to_string());
            }
            if let Some(page) = input.page {
                query.append_pair("page", &page.to_string());
            }
        }

        let response = get_with_rate_limit_retry(&self.client, &url, self.name()).await?;
        if !response.is_success() {
            return Err(Error::message(response.failure_message()));
        }
        response.json()
    }
}

fn format_pull_request(pull_request: &PullRequestListItem) -> String {
    let mut lines = vec![
        format!("- Number: #{}", pull_request.number),
        format!("- Title: {}", pull_request.title),
        format!(
            "- State: {}{}",
            pull_request.state,
            if pull_request.draft == Some(true) {
                " (draft)"
            } else {
                ""
            }
        ),
    ];
    if let Some(author) = pull_request
        .user
        .as_ref()
        .and_then(|user| user.login.as_deref())
        .filter(|login| !login.is_empty())
    {
        lines.push(format!("- Author: {author}"));
    }
    if let Some(base) = pull_request
        .base
        .as_ref()
        .and_then(|branch| branch.r#ref.as_deref())
        .filter(|reference| !reference.is_empty())
    {
        lines.push(format!("- Base: {base}"));
    }
    if let Some(head) = pull_request
        .head
        .as_ref()
        .and_then(|branch| branch.r#ref.as_deref())
        .filter(|reference| !reference.is_empty())
    {
        lines.push(format!("- Head: {head}"));
    }
    lines.push(format!("- Updated: {}", pull_request.updated_at));
    if let Some(merged_at) = pull_request
        .merged_at
        .as_deref()
        .filter(|merged_at| !merged_at.is_empty())
    {
        lines.push(format!("- Merged At: {merged_at}"));
    }
    lines.push(format!("- URL: {}", pull_request.html_url));
    lines.join("\n")
}

fn format_results(owner: &str, repo: &str, items: &[PullRequestListItem]) -> String {
    if items.is_empty() {
        return format!("No pull requests found for {owner}/{repo}.");
    }
    let header = format!(
        "GitHub Pull Requests for {owner}/{repo} (showing {})",
        items.len()
    );
    let items = items
        .iter()
        .map(format_pull_request)
        .collect::<Vec<_>>()
        .join("\n----------\n");
    format!("{header}\n\n{items}")
}

#[async_trait]
impl AgentTool for GithubListPullRequestsTool {
    fn name(&self) -> &str {
        "github_list_pull_requests"
    }

    fn description(&self) -> &str {
        "List pull requests for a repository via GitHub API."
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
                "state": {
                    "type": "string",
                    "description": "Filter by state (open, closed, all)."
                },
                "head": {
                    "type": "string",
                    "description": "Filter by head user/org and branch."
                },
                "base": {
                    "type": "string",
                    "description": "Filter by base branch."
                },
                "sort": {
                    "type": "string",
                    "description": "Sort by (created, updated, popularity, long-running)."
                },
                "direction": {
                    "type": "string",
                    "description": "Sort direction (asc, desc)."
                },
                "perPage": {
                    "type": "integer",
                    "minimum": 1,
                    "maximum": MAX_PER_PAGE,
                    "description": "Number of items per page (max 100)."
                },
                "page": {
                    "type": "integer",
                    "minimum": 1,
                    "description": "Page number (1-based)."
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

impl GithubListPullRequestsTool {
    async fn run(&self, params: Value) -> Result<String> {
        let input = parse_input(params)?;
        let owner = input.owner.trim();
        let repo = input.repo.trim();
        let items = self.list_pull_requests(&input).await?;
        Ok(format_results(owner, repo, &items))
    }
}

fn parse_input(params: Value) -> Result<ListPullRequestsInput> {
    let input: ListPullRequestsInput = parse_params("github_list_pull_requests", params)?;
    input.validate()?;
    Ok(input)
}

#[cfg(test)]
mod tests {
    use {super::*, secrecy::Secret};

    fn tool(base_url: String, token: Option<&str>) -> GithubListPullRequestsTool {
        GithubListPullRequestsTool::new(Arc::new(GitHubClient::for_test(
            base_url,
            token.map(|value| Secret::new(value.to_string())),
        )))
    }

    #[test]
    fn exposes_the_reference_description_and_a_strict_schema() {
        let tool = tool("http://127.0.0.1:1".into(), None);

        assert_eq!(tool.name(), "github_list_pull_requests");
        assert_eq!(
            tool.description(),
            "List pull requests for a repository via GitHub API."
        );
        let schema = tool.parameters_schema();
        assert_eq!(schema["additionalProperties"], false);
        assert_eq!(schema["required"], json!(["owner", "repo"]));
        for (name, description) in [
            ("owner", "Repository owner (organization or user)."),
            ("repo", "Repository name."),
            ("state", "Filter by state (open, closed, all)."),
            ("head", "Filter by head user/org and branch."),
            ("base", "Filter by base branch."),
            (
                "sort",
                "Sort by (created, updated, popularity, long-running).",
            ),
            ("direction", "Sort direction (asc, desc)."),
            ("perPage", "Number of items per page (max 100)."),
            ("page", "Page number (1-based)."),
        ] {
            assert_eq!(schema["properties"][name]["description"], description);
        }
        assert_eq!(schema["properties"]["perPage"]["type"], "integer");
        assert_eq!(schema["properties"]["perPage"]["maximum"], 100);
        assert_eq!(schema["properties"]["page"]["minimum"], 1);
    }

    fn parse_error(params: Value) -> String {
        parse_input(params)
            .err()
            .map(|error| error.to_string())
            .unwrap_or_default()
    }

    #[test]
    fn validates_required_fields_pages_and_camel_case_parameters() {
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
        assert!(parse_input(json!({ "owner": "o", "repo": "r", "page": 0 })).is_err());
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
    async fn sends_every_filter_and_formats_the_reference_layout() {
        let mut server = mockito::Server::new_async().await;
        let call = server
            .mock("GET", "/repos/owner/repo/pulls")
            .match_query(mockito::Matcher::AllOf(vec![
                mockito::Matcher::UrlEncoded("state".into(), "all".into()),
                mockito::Matcher::UrlEncoded("head".into(), "owner:feature".into()),
                mockito::Matcher::UrlEncoded("base".into(), "main".into()),
                mockito::Matcher::UrlEncoded("sort".into(), "updated".into()),
                mockito::Matcher::UrlEncoded("direction".into(), "desc".into()),
                mockito::Matcher::UrlEncoded("per_page".into(), "2".into()),
                mockito::Matcher::UrlEncoded("page".into(), "3".into()),
            ]))
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                json!([
                    {
                        "number": 12,
                        "title": "First pull request",
                        "state": "open",
                        "draft": true,
                        "html_url": "https://github.com/owner/repo/pull/12",
                        "created_at": "2026-08-01T00:00:00Z",
                        "updated_at": "2026-08-02T00:00:00Z",
                        "merged_at": null,
                        "user": { "login": "alice" },
                        "head": { "ref": "feature" },
                        "base": { "ref": "main" }
                    },
                    {
                        "number": 11,
                        "title": "Merged pull request",
                        "state": "closed",
                        "draft": false,
                        "html_url": "https://github.com/owner/repo/pull/11",
                        "created_at": "2026-07-01T00:00:00Z",
                        "updated_at": "2026-07-03T00:00:00Z",
                        "merged_at": "2026-07-02T00:00:00Z",
                        "user": null,
                        "head": null,
                        "base": null
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
                "state": "all",
                "head": "owner:feature",
                "base": "main",
                "sort": "updated",
                "direction": "desc",
                "perPage": 2,
                "page": 3
            }))
            .await
            .unwrap_or_else(|error| panic!("execute failed: {error}"));

        assert_eq!(
            result,
            json!(
                "GitHub Pull Requests for owner/repo (showing 2)\n\n\
                 - Number: #12\n- Title: First pull request\n- State: open (draft)\n- Author: alice\n- Base: main\n- Head: feature\n- Updated: 2026-08-02T00:00:00Z\n- URL: https://github.com/owner/repo/pull/12\n\
                 ----------\n\
                 - Number: #11\n- Title: Merged pull request\n- State: closed\n- Updated: 2026-07-03T00:00:00Z\n- Merged At: 2026-07-02T00:00:00Z\n- URL: https://github.com/owner/repo/pull/11"
            )
        );
        call.assert_async().await;
    }

    #[tokio::test]
    async fn empty_result_set_uses_the_reference_message() {
        let mut server = mockito::Server::new_async().await;
        let call = server
            .mock("GET", "/repos/o/r/pulls")
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

        assert_eq!(result, json!("No pull requests found for o/r."));
        call.assert_async().await;
    }

    #[tokio::test]
    async fn unsuccessful_response_propagates_the_github_error_body() {
        for (status, body) in [
            (404, "{\"message\":\"Not Found\"}"),
            (422, "{\"message\":\"Validation Failed\"}"),
        ] {
            let mut server = mockito::Server::new_async().await;
            let call = server
                .mock("GET", "/repos/o/r/pulls")
                .with_status(status)
                .with_body(body)
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

            assert_eq!(error.to_string(), body);
            call.assert_async().await;
        }
    }

    #[tokio::test(start_paused = true)]
    async fn rate_limited_response_is_retried_once_after_the_cooldown() {
        let mut server = mockito::Server::new_async().await;
        let limited = server
            .mock("GET", "/repos/o/r/pulls")
            .with_status(429)
            .with_header("retry-after", "1")
            .with_body("API rate limit exceeded")
            .expect(1)
            .create_async()
            .await;
        let retried = server
            .mock("GET", "/repos/o/r/pulls")
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

        assert_eq!(result, json!("No pull requests found for o/r."));
        limited.assert_async().await;
        retried.assert_async().await;
    }

    #[tokio::test]
    async fn rate_limit_without_timing_propagates_the_github_error() {
        let mut server = mockito::Server::new_async().await;
        let call = server
            .mock("GET", "/repos/o/r/pulls")
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
