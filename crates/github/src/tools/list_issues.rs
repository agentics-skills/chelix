//! `github_list_issues` — list open repository issues through
//! `GET /search/issues` with server-side issue filtering.

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

/// Maximum `per_page` accepted by the GitHub issues API.
const MAX_PER_PAGE: u32 = 100;
/// JSON Schema pattern for one GitHub repository path component.
const REPOSITORY_COMPONENT_PATTERN: &str = "^[A-Za-z0-9._-]+$";

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ListIssuesInput {
    owner: String,
    repo: String,
    #[serde(default)]
    per_page: Option<u32>,
}

impl ListIssuesInput {
    fn validate(&self) -> Result<()> {
        for (name, value) in [("owner", &self.owner), ("repo", &self.repo)] {
            if value.trim().is_empty() {
                return Err(Error::message(format!(
                    "Missing required parameter: {name}"
                )));
            }
            if !is_valid_repository_component(value) {
                return Err(Error::message(format!(
                    "Invalid parameter: {name} must contain only ASCII letters, digits, hyphens, underscores, or periods"
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

fn is_valid_repository_component(value: &str) -> bool {
    value
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
}

#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
enum ListIssueLabel {
    Object { name: Option<String> },
    String(String),
}

impl ListIssueLabel {
    fn name(&self) -> Option<&str> {
        match self {
            Self::Object { name } => name.as_deref(),
            Self::String(name) => Some(name.as_str()),
        }
        .filter(|name| !name.is_empty())
    }
}

#[derive(Debug, Clone, Deserialize)]
struct ListIssueUser {
    login: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct ListIssueItem {
    number: u64,
    title: String,
    state: String,
    html_url: String,
    comments: u64,
    updated_at: String,
    user: Option<ListIssueUser>,
    labels: Option<Vec<ListIssueLabel>>,
}

#[derive(Debug, Clone, Deserialize)]
struct ListIssuesResponse {
    items: Vec<ListIssueItem>,
}

/// List issues for a GitHub repository.
pub struct GithubListIssuesTool {
    client: Arc<GitHubClient>,
}

impl GithubListIssuesTool {
    #[must_use]
    pub fn new(client: Arc<GitHubClient>) -> Self {
        Self { client }
    }

    async fn list_issues(&self, input: &ListIssuesInput) -> Result<Vec<ListIssueItem>> {
        let mut url = url::Url::parse(&format!("{}/search/issues", self.client.base_url()))?;
        {
            let mut query = url.query_pairs_mut();
            query.append_pair(
                "q",
                &format!(
                    "repo:{}/{} is:issue state:open",
                    input.owner.trim(),
                    input.repo.trim()
                ),
            );
            query.append_pair("sort", "created");
            query.append_pair("order", "desc");
            if let Some(per_page) = input.per_page {
                query.append_pair("per_page", &per_page.to_string());
            }
        }

        let response = get_with_rate_limit_retry(&self.client, &url, self.name()).await?;
        if !response.is_success() {
            return Err(Error::message(response.failure_message()));
        }
        Ok(response.json::<ListIssuesResponse>()?.items)
    }
}

fn format_issue(issue: &ListIssueItem) -> String {
    let mut lines = vec![
        format!("- Number: #{}", issue.number),
        format!("- Title: {}", issue.title),
        format!("- State: {}", issue.state),
    ];
    if let Some(author) = issue
        .user
        .as_ref()
        .and_then(|user| user.login.as_deref())
        .filter(|login| !login.is_empty())
    {
        lines.push(format!("- Author: {author}"));
    }
    lines.extend([
        format!("- Comments: {}", issue.comments),
        format!("- Updated: {}", issue.updated_at),
    ]);
    if let Some(labels) = &issue.labels {
        let names = labels
            .iter()
            .filter_map(ListIssueLabel::name)
            .collect::<Vec<_>>();
        if !names.is_empty() {
            lines.push(format!("- Labels: {}", names.join(", ")));
        }
    }
    lines.push(format!("- URL: {}", issue.html_url));
    lines.join("\n")
}

fn format_results(owner: &str, repo: &str, items: &[ListIssueItem]) -> String {
    if items.is_empty() {
        return format!("No issues found for {owner}/{repo}.");
    }
    let header = format!("GitHub Issues for {owner}/{repo} (showing {})", items.len());
    let items = items
        .iter()
        .map(format_issue)
        .collect::<Vec<_>>()
        .join("\n----------\n");
    format!("{header}\n\n{items}")
}

#[async_trait]
impl AgentTool for GithubListIssuesTool {
    fn name(&self) -> &str {
        "github_list_issues"
    }

    fn description(&self) -> &str {
        "List open issues for a repository via GitHub API."
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
                    "pattern": REPOSITORY_COMPONENT_PATTERN,
                    "description": "Repository owner (organization or user)."
                },
                "repo": {
                    "type": "string",
                    "minLength": 1,
                    "pattern": REPOSITORY_COMPONENT_PATTERN,
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

impl GithubListIssuesTool {
    async fn run(&self, params: Value) -> Result<String> {
        let input = parse_input(params)?;
        let owner = input.owner.trim();
        let repo = input.repo.trim();
        let items = self.list_issues(&input).await?;
        Ok(format_results(owner, repo, &items))
    }
}

fn parse_input(params: Value) -> Result<ListIssuesInput> {
    let input: ListIssuesInput = parse_params("github_list_issues", params)?;
    input.validate()?;
    Ok(input)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tool(base_url: String) -> GithubListIssuesTool {
        GithubListIssuesTool::new(Arc::new(GitHubClient::for_test(base_url, None)))
    }

    #[test]
    fn exposes_the_reference_description_and_a_strict_schema() {
        let tool = tool("http://127.0.0.1:1".into());

        assert_eq!(tool.name(), "github_list_issues");
        assert_eq!(
            tool.description(),
            "List open issues for a repository via GitHub API."
        );
        let schema = tool.parameters_schema();
        assert_eq!(schema["additionalProperties"], false);
        assert_eq!(schema["required"], json!(["owner", "repo"]));
        for (name, description) in [
            ("owner", "Repository owner (organization or user)."),
            ("repo", "Repository name."),
            ("perPage", "Number of items per page (max 100)."),
        ] {
            assert_eq!(schema["properties"][name]["description"], description);
        }
        assert_eq!(
            schema["properties"]["owner"]["pattern"],
            REPOSITORY_COMPONENT_PATTERN
        );
        assert_eq!(
            schema["properties"]["repo"]["pattern"],
            REPOSITORY_COMPONENT_PATTERN
        );
        assert_eq!(schema["properties"]["perPage"]["type"], "integer");
        assert_eq!(schema["properties"]["perPage"]["maximum"], 100);
    }

    fn parse_error(params: Value) -> String {
        parse_input(params)
            .err()
            .map(|error| error.to_string())
            .unwrap_or_default()
    }

    #[test]
    fn validates_required_fields_page_size_and_camel_case_parameters() {
        assert_eq!(
            parse_error(json!({ "owner": " ", "repo": "r" })),
            "Missing required parameter: owner"
        );
        assert_eq!(
            parse_error(json!({ "owner": "o", "repo": "" })),
            "Missing required parameter: repo"
        );
        assert_eq!(
            parse_error(json!({ "owner": "o", "repo": "rust state:closed" })),
            "Invalid parameter: repo must contain only ASCII letters, digits, hyphens, underscores, or periods"
        );
        assert_eq!(
            parse_error(json!({ "owner": "org:kubernetes", "repo": "r" })),
            "Invalid parameter: owner must contain only ASCII letters, digits, hyphens, underscores, or periods"
        );
        assert!(parse_input(json!({ "owner": "o", "repo": "r", "per_page": 5 })).is_err());
        assert!(parse_input(json!({ "owner": "o", "repo": "r", "perPage": 0 })).is_err());
        assert!(parse_input(json!({ "owner": "o", "repo": "r", "perPage": 101 })).is_err());
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
    async fn lists_open_issues_in_created_descending_order() {
        let mut server = mockito::Server::new_async().await;
        let call = server
            .mock("GET", "/search/issues")
            .match_query(mockito::Matcher::AllOf(vec![
                mockito::Matcher::UrlEncoded(
                    "q".into(),
                    "repo:owner/repo is:issue state:open".into(),
                ),
                mockito::Matcher::UrlEncoded("sort".into(), "created".into()),
                mockito::Matcher::UrlEncoded("order".into(), "desc".into()),
                mockito::Matcher::UrlEncoded("per_page".into(), "2".into()),
            ]))
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                json!({
                    "total_count": 2,
                    "incomplete_results": false,
                    "items": [
                        {
                            "number": 12,
                            "title": "First issue",
                            "state": "open",
                            "html_url": "https://github.com/owner/repo/issues/12",
                            "comments": 3,
                            "updated_at": "2026-08-02T00:00:00Z",
                            "user": { "login": "alice" },
                            "labels": [{ "name": "bug" }, "urgent", { "name": "" }]
                        },
                        {
                            "number": 13,
                            "title": "Second issue",
                            "state": "open",
                            "html_url": "https://github.com/owner/repo/issues/13",
                            "comments": 1,
                            "updated_at": "2026-08-03T00:00:00Z",
                            "user": null,
                            "labels": []
                        }
                    ]
                })
                .to_string(),
            )
            .expect(1)
            .create_async()
            .await;

        let result = tool(server.url())
            .execute(json!({ "owner": "owner", "repo": "repo", "perPage": 2 }))
            .await
            .unwrap_or_else(|error| panic!("execute failed: {error}"));

        assert_eq!(
            result,
            json!(
                "GitHub Issues for owner/repo (showing 2)\n\n\
                 - Number: #12\n- Title: First issue\n- State: open\n- Author: alice\n- Comments: 3\n- Updated: 2026-08-02T00:00:00Z\n- Labels: bug, urgent\n- URL: https://github.com/owner/repo/issues/12\n\
                 ----------\n\
                 - Number: #13\n- Title: Second issue\n- State: open\n- Comments: 1\n- Updated: 2026-08-03T00:00:00Z\n- URL: https://github.com/owner/repo/issues/13"
            )
        );
        call.assert_async().await;
    }

    #[tokio::test]
    async fn empty_search_results_use_the_reference_message() {
        let mut server = mockito::Server::new_async().await;
        let call = server
            .mock("GET", "/search/issues")
            .match_query(mockito::Matcher::AllOf(vec![
                mockito::Matcher::UrlEncoded("q".into(), "repo:o/r is:issue state:open".into()),
                mockito::Matcher::UrlEncoded("sort".into(), "created".into()),
                mockito::Matcher::UrlEncoded("order".into(), "desc".into()),
            ]))
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(json!({ "total_count": 0, "items": [] }).to_string())
            .expect(1)
            .create_async()
            .await;

        let result = tool(server.url())
            .execute(json!({ "owner": "o", "repo": "r" }))
            .await
            .unwrap_or_else(|error| panic!("execute failed: {error}"));

        assert_eq!(result, json!("No issues found for o/r."));
        call.assert_async().await;
    }

    #[tokio::test]
    async fn unsuccessful_response_propagates_the_github_error_body() {
        let mut server = mockito::Server::new_async().await;
        let call = server
            .mock("GET", "/search/issues")
            .match_query(mockito::Matcher::Any)
            .with_status(404)
            .with_body("{\"message\":\"Not Found\"}")
            .expect(1)
            .create_async()
            .await;

        let error = match tool(server.url())
            .execute(json!({ "owner": "o", "repo": "r" }))
            .await
        {
            Ok(_) => panic!("expected a retrieval error"),
            Err(error) => error,
        };

        assert_eq!(error.to_string(), "{\"message\":\"Not Found\"}");
        call.assert_async().await;
    }

    #[tokio::test]
    async fn rate_limit_without_timing_propagates_the_github_error() {
        let mut server = mockito::Server::new_async().await;
        let call = server
            .mock("GET", "/search/issues")
            .match_query(mockito::Matcher::Any)
            .with_status(429)
            .with_body("You have exceeded a secondary rate limit")
            .expect(1)
            .create_async()
            .await;

        let error = match tool(server.url())
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

    #[tokio::test(start_paused = true)]
    async fn rate_limited_response_is_retried_once_after_the_cooldown() {
        let mut server = mockito::Server::new_async().await;
        let limited = server
            .mock("GET", "/search/issues")
            .match_query(mockito::Matcher::Any)
            .with_status(429)
            .with_header("retry-after", "1")
            .with_body("API rate limit exceeded")
            .expect(1)
            .create_async()
            .await;
        let retried = server
            .mock("GET", "/search/issues")
            .match_query(mockito::Matcher::Any)
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(json!({ "total_count": 0, "items": [] }).to_string())
            .expect(1)
            .create_async()
            .await;

        let result = tool(server.url())
            .execute(json!({ "owner": "o", "repo": "r" }))
            .await
            .unwrap_or_else(|error| panic!("execute failed: {error}"));

        assert_eq!(result, json!("No issues found for o/r."));
        limited.assert_async().await;
        retried.assert_async().await;
    }
}
