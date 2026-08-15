//! `github_search_issues` — search issues through `GET /search/issues`.

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

/// Maximum `per_page` accepted by the GitHub issue search API.
const MAX_PER_PAGE: u32 = 100;

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SearchIssuesInput {
    query: String,
    #[serde(default)]
    per_page: Option<u32>,
}

impl SearchIssuesInput {
    fn validate(&self) -> Result<()> {
        if self.query.trim().is_empty() {
            return Err(Error::message("Missing required parameter: query"));
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
#[serde(untagged)]
enum SearchIssueLabel {
    Object { name: Option<String> },
    String(String),
}

impl SearchIssueLabel {
    fn name(&self) -> Option<&str> {
        match self {
            Self::Object { name } => name.as_deref(),
            Self::String(name) => Some(name.as_str()),
        }
        .filter(|name| !name.is_empty())
    }
}

#[derive(Debug, Clone, Deserialize)]
struct SearchIssueUser {
    login: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct PullRequestMarker {}

#[derive(Debug, Clone, Deserialize)]
struct SearchIssueItem {
    number: u64,
    title: String,
    state: String,
    html_url: String,
    comments: u64,
    created_at: String,
    updated_at: String,
    user: Option<SearchIssueUser>,
    labels: Option<Vec<SearchIssueLabel>>,
    repository_url: Option<String>,
    pull_request: Option<PullRequestMarker>,
}

#[derive(Debug, Clone, Deserialize)]
struct SearchIssuesResponse {
    total_count: u64,
    items: Vec<SearchIssueItem>,
}

/// Search issues across GitHub.
pub struct GithubSearchIssuesTool {
    client: Arc<GitHubClient>,
}

impl GithubSearchIssuesTool {
    #[must_use]
    pub fn new(client: Arc<GitHubClient>) -> Self {
        Self { client }
    }

    async fn search_issues(&self, input: &SearchIssuesInput) -> Result<SearchIssuesResponse> {
        let mut url = url::Url::parse(&format!("{}/search/issues", self.client.base_url()))?;
        {
            let mut query = url.query_pairs_mut();
            query.append_pair("q", &ensure_issue_qualifier(input.query.trim()));
            if let Some(per_page) = input.per_page {
                query.append_pair("per_page", &per_page.to_string());
            }
        }

        let response = get_with_rate_limit_retry(&self.client, &url, self.name()).await?;
        if !response.is_success() {
            return Err(Error::message(response.failure_message()));
        }
        response.json()
    }
}

fn ensure_issue_qualifier(query: &str) -> String {
    if contains_issue_qualifier(query) {
        query.to_string()
    } else {
        format!("is:issue {query}")
    }
}

fn contains_issue_qualifier(query: &str) -> bool {
    const QUALIFIER: &str = "is:issue";

    let query = query.to_ascii_lowercase();
    query.match_indices(QUALIFIER).any(|(start, qualifier)| {
        let before = start
            .checked_sub(1)
            .and_then(|index| query.as_bytes().get(index))
            .is_none_or(|byte| !is_word_byte(*byte));
        let after = query
            .as_bytes()
            .get(start + qualifier.len())
            .is_none_or(|byte| !is_word_byte(*byte));
        before && after
    })
}

const fn is_word_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_'
}

fn repository_full_name(repository_url: Option<&str>) -> Option<&str> {
    repository_url
        .and_then(|url| {
            url.find("/repos/")
                .map(|index| &url[index + "/repos/".len()..])
        })
        .filter(|name| !name.is_empty())
}

fn format_issue(issue: &SearchIssueItem) -> String {
    let mut lines = Vec::new();
    if let Some(repo) = repository_full_name(issue.repository_url.as_deref()) {
        lines.push(format!("- Repo: {repo}"));
    }
    lines.extend([
        format!("- Number: #{}", issue.number),
        format!("- Title: {}", issue.title),
        format!("- State: {}", issue.state),
    ]);
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
        format!("- Created: {}", issue.created_at),
        format!("- Updated: {}", issue.updated_at),
    ]);
    if let Some(labels) = &issue.labels {
        let names = labels
            .iter()
            .filter_map(SearchIssueLabel::name)
            .collect::<Vec<_>>();
        if !names.is_empty() {
            lines.push(format!("- Labels: {}", names.join(", ")));
        }
    }
    lines.push(format!("- URL: {}", issue.html_url));
    lines.join("\n")
}

fn format_results(response: &SearchIssuesResponse) -> String {
    let issues = response
        .items
        .iter()
        .filter(|item| item.pull_request.is_none())
        .collect::<Vec<_>>();
    if issues.is_empty() {
        return "No issues found for this query.".to_string();
    }
    let header = format!(
        "GitHub Issue Search Results (showing {} of {})",
        issues.len(),
        response.total_count
    );
    let items = issues
        .into_iter()
        .map(format_issue)
        .collect::<Vec<_>>()
        .join("\n----------\n");
    format!("{header}\n\n{items}")
}

#[async_trait]
impl AgentTool for GithubSearchIssuesTool {
    fn name(&self) -> &str {
        "github_search_issues"
    }

    fn description(&self) -> &str {
        "Search issues via GitHub API."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "additionalProperties": false,
            "required": ["query"],
            "properties": {
                "query": {
                    "type": "string",
                    "minLength": 1,
                    "description": "Search issues query string (e.g., 'repo:owner/name bug label:bug')."
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

impl GithubSearchIssuesTool {
    async fn run(&self, params: Value) -> Result<String> {
        let input = parse_input(params)?;
        let response = self.search_issues(&input).await?;
        Ok(format_results(&response))
    }
}

fn parse_input(params: Value) -> Result<SearchIssuesInput> {
    let input: SearchIssuesInput = parse_params("github_search_issues", params)?;
    input.validate()?;
    Ok(input)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tool(base_url: String) -> GithubSearchIssuesTool {
        GithubSearchIssuesTool::new(Arc::new(GitHubClient::for_test(base_url, None)))
    }

    #[test]
    fn exposes_the_reference_description_and_a_strict_schema() {
        let tool = tool("http://127.0.0.1:1".into());

        assert_eq!(tool.name(), "github_search_issues");
        assert_eq!(tool.description(), "Search issues via GitHub API.");
        let schema = tool.parameters_schema();
        assert_eq!(schema["additionalProperties"], false);
        assert_eq!(schema["required"], json!(["query"]));
        assert_eq!(
            schema["properties"]["query"]["description"],
            "Search issues query string (e.g., 'repo:owner/name bug label:bug')."
        );
        assert_eq!(
            schema["properties"]["perPage"]["description"],
            "Number of items per page (max 100)."
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
    fn validates_query_page_size_and_camel_case_parameters() {
        assert_eq!(
            parse_error(json!({ "query": "   " })),
            "Missing required parameter: query"
        );
        assert!(parse_input(json!({ "query": "bug", "per_page": 5 })).is_err());
        assert!(parse_input(json!({ "query": "bug", "perPage": 0 })).is_err());
        assert!(parse_input(json!({ "query": "bug", "perPage": 101 })).is_err());
        assert!(parse_input(json!({ "query": "bug", "unknown": true })).is_err());
    }

    #[test]
    fn ignores_enriched_internal_fields() {
        let input = parse_input(json!({
            "query": "bug",
            "perPage": 5,
            "_session_key": "session:test",
            "_channel": { "surface": "web" }
        }))
        .unwrap_or_else(|error| panic!("parse failed: {error}"));

        assert_eq!(input.query, "bug");
        assert_eq!(input.per_page, Some(5));
    }

    #[test]
    fn adds_the_issue_qualifier_only_when_missing() {
        assert_eq!(
            ensure_issue_qualifier("repo:o/r bug"),
            "is:issue repo:o/r bug"
        );
        assert_eq!(
            ensure_issue_qualifier("repo:o/r IS:ISSUE bug"),
            "repo:o/r IS:ISSUE bug"
        );
        assert_eq!(
            ensure_issue_qualifier("repo:o/r (is:issue) bug"),
            "repo:o/r (is:issue) bug"
        );
        assert_eq!(
            ensure_issue_qualifier("repo:o/r this:is:issueish"),
            "is:issue repo:o/r this:is:issueish"
        );
    }

    #[tokio::test]
    async fn sends_the_qualified_query_and_formats_only_issues() {
        let mut server = mockito::Server::new_async().await;
        let call = server
            .mock("GET", "/search/issues")
            .match_query(mockito::Matcher::AllOf(vec![
                mockito::Matcher::UrlEncoded("q".into(), "is:issue repo:o/r bug".into()),
                mockito::Matcher::UrlEncoded("per_page".into(), "2".into()),
            ]))
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                json!({
                    "total_count": 42,
                    "incomplete_results": false,
                    "items": [
                        {
                            "number": 12,
                            "title": "Issue title",
                            "state": "open",
                            "html_url": "https://github.com/o/r/issues/12",
                            "comments": 3,
                            "created_at": "2026-08-01T00:00:00Z",
                            "updated_at": "2026-08-02T00:00:00Z",
                            "user": { "login": "alice" },
                            "labels": [{ "name": "bug" }, "urgent", { "name": "" }],
                            "repository_url": "https://api.github.com/repos/o/r"
                        },
                        {
                            "number": 13,
                            "title": "Pull request",
                            "state": "open",
                            "html_url": "https://github.com/o/r/pull/13",
                            "comments": 1,
                            "created_at": "2026-08-01T00:00:00Z",
                            "updated_at": "2026-08-02T00:00:00Z",
                            "user": { "login": "bob" },
                            "labels": [],
                            "repository_url": "https://api.github.com/repos/o/r",
                            "pull_request": { "url": "https://api.github.com/repos/o/r/pulls/13" }
                        }
                    ]
                })
                .to_string(),
            )
            .expect(1)
            .create_async()
            .await;

        let result = tool(server.url())
            .execute(json!({ "query": " repo:o/r bug ", "perPage": 2 }))
            .await
            .unwrap_or_else(|error| panic!("execute failed: {error}"));

        assert_eq!(
            result,
            json!(
                "GitHub Issue Search Results (showing 1 of 42)\n\n\
                 - Repo: o/r\n- Number: #12\n- Title: Issue title\n- State: open\n- Author: alice\n- Comments: 3\n- Created: 2026-08-01T00:00:00Z\n- Updated: 2026-08-02T00:00:00Z\n- Labels: bug, urgent\n- URL: https://github.com/o/r/issues/12"
            )
        );
        call.assert_async().await;
    }

    #[tokio::test]
    async fn empty_or_pull_request_only_results_use_the_reference_message() {
        let mut server = mockito::Server::new_async().await;
        let call = server
            .mock("GET", "/search/issues")
            .match_query(mockito::Matcher::Any)
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                json!({
                    "total_count": 1,
                    "items": [{
                        "number": 1,
                        "title": "Pull request",
                        "state": "open",
                        "html_url": "https://github.com/o/r/pull/1",
                        "comments": 0,
                        "created_at": "2026-08-01T00:00:00Z",
                        "updated_at": "2026-08-01T00:00:00Z",
                        "pull_request": {}
                    }]
                })
                .to_string(),
            )
            .expect(1)
            .create_async()
            .await;

        let result = tool(server.url())
            .execute(json!({ "query": "is:issue repo:o/r" }))
            .await
            .unwrap_or_else(|error| panic!("execute failed: {error}"));

        assert_eq!(result, json!("No issues found for this query."));
        call.assert_async().await;
    }

    #[tokio::test]
    async fn unsuccessful_response_propagates_the_github_error_body() {
        let mut server = mockito::Server::new_async().await;
        let call = server
            .mock("GET", "/search/issues")
            .match_query(mockito::Matcher::Any)
            .with_status(422)
            .with_body("{\"message\":\"Validation Failed\"}")
            .expect(1)
            .create_async()
            .await;

        let error = match tool(server.url())
            .execute(json!({ "query": "is:issue invalid" }))
            .await
        {
            Ok(_) => panic!("expected a retrieval error"),
            Err(error) => error,
        };

        assert_eq!(error.to_string(), "{\"message\":\"Validation Failed\"}");
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

        let error = match tool(server.url()).execute(json!({ "query": "bug" })).await {
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
            .execute(json!({ "query": "bug" }))
            .await
            .unwrap_or_else(|error| panic!("execute failed: {error}"));

        assert_eq!(result, json!("No issues found for this query."));
        limited.assert_async().await;
        retried.assert_async().await;
    }
}
