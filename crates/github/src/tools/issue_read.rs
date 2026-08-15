//! `github_issue_read` — read one issue through
//! `GET /repos/{owner}/{repo}/issues/{issue_number}`.

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
struct IssueReadInput {
    owner: String,
    repo: String,
    issue_number: u64,
}

impl IssueReadInput {
    fn validate(&self) -> Result<()> {
        for (name, value) in [("owner", &self.owner), ("repo", &self.repo)] {
            if value.trim().is_empty() {
                return Err(Error::message(format!(
                    "Missing required parameter: {name}"
                )));
            }
        }
        if self.issue_number == 0 {
            return Err(Error::message(
                "Missing or invalid required parameter: issueNumber",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Deserialize)]
struct IssueUser {
    login: String,
    id: u64,
    node_id: String,
    r#type: String,
    site_admin: bool,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
enum IssueLabel {
    Object { name: String },
    String(String),
}

impl IssueLabel {
    fn name(&self) -> &str {
        match self {
            Self::Object { name } | Self::String(name) => name,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
struct IssueMilestone {
    number: u64,
    state: String,
    title: String,
    open_issues: u64,
    closed_issues: u64,
    due_on: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct IssueReactions {
    total_count: u64,
    #[serde(rename = "+1")]
    plus_one: u64,
    #[serde(rename = "-1")]
    minus_one: u64,
    laugh: u64,
    confused: u64,
    heart: u64,
    hooray: u64,
    rocket: u64,
    eyes: u64,
}

#[derive(Debug, Clone, Deserialize)]
struct Issue {
    id: u64,
    number: u64,
    title: String,
    state: String,
    state_reason: Option<String>,
    locked: Option<bool>,
    author_association: Option<String>,
    html_url: String,
    url: String,
    comments_url: String,
    events_url: String,
    labels_url: String,
    repository_url: Option<String>,
    node_id: String,
    user: Option<IssueUser>,
    body: Option<String>,
    closed_at: Option<String>,
    created_at: String,
    updated_at: String,
    closed_by: Option<IssueUser>,
    comments: u64,
    labels: Option<Vec<IssueLabel>>,
    milestone: Option<IssueMilestone>,
    reactions: Option<IssueReactions>,
}

/// Read a single issue from a GitHub repository.
pub struct GithubIssueReadTool {
    client: Arc<GitHubClient>,
}

impl GithubIssueReadTool {
    #[must_use]
    pub fn new(client: Arc<GitHubClient>) -> Self {
        Self { client }
    }

    async fn get_issue(&self, input: &IssueReadInput) -> Result<Issue> {
        let mut url = url::Url::parse(&format!("{}/repos/", self.client.base_url()))?;
        let issue_number = input.issue_number.to_string();
        {
            let mut segments = url
                .path_segments_mut()
                .map_err(|()| Error::message("GitHub API base URL cannot have path segments"))?;
            segments.pop_if_empty();
            segments.push(input.owner.trim());
            segments.push(input.repo.trim());
            segments.push("issues");
            segments.push(&issue_number);
        }

        let response = get_with_rate_limit_retry(&self.client, &url, self.name()).await?;
        if !response.is_success() {
            return Err(Error::message(response.failure_message()));
        }
        response.json()
    }
}

fn format_issue(issue: &Issue) -> String {
    let mut lines = vec![
        format!("- id: {}", issue.id),
        format!("- number: {}", issue.number),
        format!("- title: {}", issue.title),
        format!("- state: {}", issue.state),
    ];
    if let Some(state_reason) = issue
        .state_reason
        .as_deref()
        .filter(|state_reason| !state_reason.is_empty())
    {
        lines.push(format!("- state_reason: {state_reason}"));
    }
    if let Some(locked) = issue.locked {
        lines.push(format!("- locked: {locked}"));
    }
    if let Some(author_association) = issue
        .author_association
        .as_deref()
        .filter(|association| !association.is_empty())
    {
        lines.push(format!("- author_association: {author_association}"));
    }
    lines.extend([
        format!("- html_url: {}", issue.html_url),
        format!("- url: {}", issue.url),
        format!("- comments_url: {}", issue.comments_url),
        format!("- events_url: {}", issue.events_url),
        format!("- labels_url: {}", issue.labels_url),
    ]);
    if let Some(repository_url) = issue
        .repository_url
        .as_deref()
        .filter(|repository_url| !repository_url.is_empty())
    {
        lines.push(format!("- repository_url: {repository_url}"));
    }
    lines.extend([
        format!("- node_id: {}", issue.node_id),
        format!("- comments: {}", issue.comments),
        format!("- created_at: {}", issue.created_at),
        format!("- updated_at: {}", issue.updated_at),
    ]);
    if let Some(closed_at) = issue
        .closed_at
        .as_deref()
        .filter(|closed_at| !closed_at.is_empty())
    {
        lines.push(format!("- closed_at: {closed_at}"));
    }
    if let Some(user) = &issue.user {
        lines.extend([
            format!("- user.login: {}", user.login),
            format!("- user.id: {}", user.id),
            format!("- user.node_id: {}", user.node_id),
            format!("- user.type: {}", user.r#type),
            format!("- user.site_admin: {}", user.site_admin),
        ]);
    }
    if let Some(closed_by) = &issue.closed_by {
        lines.extend([
            format!("- closed_by.login: {}", closed_by.login),
            format!("- closed_by.id: {}", closed_by.id),
            format!("- closed_by.node_id: {}", closed_by.node_id),
        ]);
    }
    if let Some(milestone) = &issue.milestone {
        lines.extend([
            format!("- milestone.title: {}", milestone.title),
            format!("- milestone.state: {}", milestone.state),
            format!("- milestone.number: {}", milestone.number),
            format!("- milestone.open_issues: {}", milestone.open_issues),
            format!("- milestone.closed_issues: {}", milestone.closed_issues),
        ]);
        if let Some(due_on) = milestone
            .due_on
            .as_deref()
            .filter(|due_on| !due_on.is_empty())
        {
            lines.push(format!("- milestone.due_on: {due_on}"));
        }
    }
    if let Some(labels) = &issue.labels {
        let names = labels
            .iter()
            .map(IssueLabel::name)
            .filter(|name| !name.is_empty())
            .collect::<Vec<_>>();
        if !names.is_empty() {
            lines.push(format!("- labels: {}", names.join(", ")));
        }
    }
    if let Some(reactions) = &issue.reactions {
        lines.extend([
            format!("- reactions.total_count: {}", reactions.total_count),
            format!("- reactions.+1: {}", reactions.plus_one),
            format!("- reactions.-1: {}", reactions.minus_one),
            format!("- reactions.laugh: {}", reactions.laugh),
            format!("- reactions.confused: {}", reactions.confused),
            format!("- reactions.heart: {}", reactions.heart),
            format!("- reactions.hooray: {}", reactions.hooray),
            format!("- reactions.rocket: {}", reactions.rocket),
            format!("- reactions.eyes: {}", reactions.eyes),
        ]);
    }
    lines.push("\nBody:\n".to_string());
    lines.push(
        issue
            .body
            .as_deref()
            .filter(|body| !body.trim().is_empty())
            .unwrap_or("(empty)")
            .to_string(),
    );
    lines.join("\n")
}

fn format_result(owner: &str, repo: &str, issue: &Issue) -> String {
    format!(
        "GitHub Issue (full) {owner}/{repo} #{}\n\n{}",
        issue.number,
        format_issue(issue)
    )
}

#[async_trait]
impl AgentTool for GithubIssueReadTool {
    fn name(&self) -> &str {
        "github_issue_read"
    }

    fn description(&self) -> &str {
        "Read a single issue from a GitHub repository via GitHub API."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "additionalProperties": false,
            "required": ["owner", "repo", "issueNumber"],
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
                "issueNumber": {
                    "type": "integer",
                    "minimum": 1,
                    "description": "Issue number."
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

impl GithubIssueReadTool {
    async fn run(&self, params: Value) -> Result<String> {
        let input = parse_input(params)?;
        let owner = input.owner.trim();
        let repo = input.repo.trim();
        let issue = self.get_issue(&input).await?;
        Ok(format_result(owner, repo, &issue))
    }
}

fn parse_input(params: Value) -> Result<IssueReadInput> {
    let input: IssueReadInput = parse_params("github_issue_read", params)?;
    input.validate()?;
    Ok(input)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tool(base_url: String) -> GithubIssueReadTool {
        GithubIssueReadTool::new(Arc::new(GitHubClient::for_test(base_url, None)))
    }

    #[test]
    fn exposes_the_reference_description_and_a_strict_schema() {
        let tool = tool("http://127.0.0.1:1".into());

        assert_eq!(tool.name(), "github_issue_read");
        assert_eq!(
            tool.description(),
            "Read a single issue from a GitHub repository via GitHub API."
        );
        let schema = tool.parameters_schema();
        assert_eq!(schema["additionalProperties"], false);
        assert_eq!(schema["required"], json!(["owner", "repo", "issueNumber"]));
        for (name, description) in [
            ("owner", "Repository owner (organization or user)."),
            ("repo", "Repository name."),
            ("issueNumber", "Issue number."),
        ] {
            assert_eq!(schema["properties"][name]["description"], description);
        }
        assert_eq!(schema["properties"]["issueNumber"]["type"], "integer");
        assert_eq!(schema["properties"]["issueNumber"]["minimum"], 1);
    }

    fn parse_error(params: Value) -> String {
        parse_input(params)
            .err()
            .map(|error| error.to_string())
            .unwrap_or_default()
    }

    #[test]
    fn validates_required_fields_number_and_camel_case_parameters() {
        assert_eq!(
            parse_error(json!({ "owner": " ", "repo": "r", "issueNumber": 1 })),
            "Missing required parameter: owner"
        );
        assert_eq!(
            parse_error(json!({ "owner": "o", "repo": "", "issueNumber": 1 })),
            "Missing required parameter: repo"
        );
        assert_eq!(
            parse_error(json!({ "owner": "o", "repo": "r", "issueNumber": 0 })),
            "Missing or invalid required parameter: issueNumber"
        );
        assert!(parse_input(json!({ "owner": "o", "repo": "r", "issue_number": 1 })).is_err());
        assert!(
            parse_input(json!({
                "owner": "o",
                "repo": "r",
                "issueNumber": 1,
                "unknown": true
            }))
            .is_err()
        );
    }

    #[test]
    fn ignores_enriched_internal_fields() {
        let input = parse_input(json!({
            "owner": "o",
            "repo": "r",
            "issueNumber": 7,
            "_session_key": "session:test",
            "_channel": { "surface": "web" }
        }))
        .unwrap_or_else(|error| panic!("parse failed: {error}"));

        assert_eq!(input.issue_number, 7);
    }

    fn full_issue_response() -> Value {
        json!({
            "id": 100,
            "number": 7,
            "title": "Issue title",
            "state": "closed",
            "state_reason": "completed",
            "locked": false,
            "author_association": "MEMBER",
            "html_url": "https://github.com/owner/repo/issues/7",
            "url": "https://api.github.com/repos/owner/repo/issues/7",
            "comments_url": "https://api.github.com/repos/owner/repo/issues/7/comments",
            "events_url": "https://api.github.com/repos/owner/repo/issues/7/events",
            "labels_url": "https://api.github.com/repos/owner/repo/issues/7/labels{/name}",
            "repository_url": "https://api.github.com/repos/owner/repo",
            "node_id": "ISSUE_NODE",
            "comments": 3,
            "created_at": "2026-08-01T00:00:00Z",
            "updated_at": "2026-08-02T00:00:00Z",
            "closed_at": "2026-08-03T00:00:00Z",
            "user": {
                "login": "alice",
                "id": 10,
                "node_id": "USER_NODE",
                "type": "User",
                "site_admin": false
            },
            "closed_by": {
                "login": "bob",
                "id": 11,
                "node_id": "CLOSER_NODE",
                "type": "User",
                "site_admin": false
            },
            "milestone": {
                "title": "v1",
                "state": "open",
                "number": 2,
                "open_issues": 4,
                "closed_issues": 5,
                "due_on": "2026-09-01T00:00:00Z"
            },
            "labels": [{ "name": "bug" }, "urgent", { "name": "" }],
            "reactions": {
                "total_count": 8,
                "+1": 1,
                "-1": 2,
                "laugh": 3,
                "confused": 4,
                "heart": 5,
                "hooray": 6,
                "rocket": 7,
                "eyes": 8
            },
            "body": "First line\n\nSecond line"
        })
    }

    #[tokio::test]
    async fn reads_and_formats_the_full_reference_layout() {
        let mut server = mockito::Server::new_async().await;
        let call = server
            .mock("GET", "/repos/owner/repo/issues/7")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(full_issue_response().to_string())
            .expect(1)
            .create_async()
            .await;

        let result = tool(server.url())
            .execute(json!({
                "owner": " owner ",
                "repo": " repo ",
                "issueNumber": 7
            }))
            .await
            .unwrap_or_else(|error| panic!("execute failed: {error}"));

        assert_eq!(
            result,
            json!(
                "GitHub Issue (full) owner/repo #7\n\n\
                 - id: 100\n- number: 7\n- title: Issue title\n- state: closed\n- state_reason: completed\n- locked: false\n- author_association: MEMBER\n\
                 - html_url: https://github.com/owner/repo/issues/7\n- url: https://api.github.com/repos/owner/repo/issues/7\n\
                 - comments_url: https://api.github.com/repos/owner/repo/issues/7/comments\n- events_url: https://api.github.com/repos/owner/repo/issues/7/events\n\
                 - labels_url: https://api.github.com/repos/owner/repo/issues/7/labels{/name}\n- repository_url: https://api.github.com/repos/owner/repo\n\
                 - node_id: ISSUE_NODE\n- comments: 3\n- created_at: 2026-08-01T00:00:00Z\n- updated_at: 2026-08-02T00:00:00Z\n- closed_at: 2026-08-03T00:00:00Z\n\
                 - user.login: alice\n- user.id: 10\n- user.node_id: USER_NODE\n- user.type: User\n- user.site_admin: false\n\
                 - closed_by.login: bob\n- closed_by.id: 11\n- closed_by.node_id: CLOSER_NODE\n\
                 - milestone.title: v1\n- milestone.state: open\n- milestone.number: 2\n- milestone.open_issues: 4\n- milestone.closed_issues: 5\n- milestone.due_on: 2026-09-01T00:00:00Z\n\
                 - labels: bug, urgent\n- reactions.total_count: 8\n- reactions.+1: 1\n- reactions.-1: 2\n- reactions.laugh: 3\n- reactions.confused: 4\n- reactions.heart: 5\n- reactions.hooray: 6\n- reactions.rocket: 7\n- reactions.eyes: 8\n\nBody:\n\nFirst line\n\nSecond line"
            )
        );
        call.assert_async().await;
    }

    #[tokio::test]
    async fn blank_body_uses_the_reference_empty_marker() {
        let mut server = mockito::Server::new_async().await;
        let call = server
            .mock("GET", "/repos/o/r/issues/1")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                json!({
                    "id": 1,
                    "number": 1,
                    "title": "Empty",
                    "state": "open",
                    "html_url": "https://github.com/o/r/issues/1",
                    "url": "https://api.github.com/repos/o/r/issues/1",
                    "comments_url": "https://api.github.com/repos/o/r/issues/1/comments",
                    "events_url": "https://api.github.com/repos/o/r/issues/1/events",
                    "labels_url": "https://api.github.com/repos/o/r/issues/1/labels{/name}",
                    "node_id": "NODE",
                    "comments": 0,
                    "created_at": "2026-08-01T00:00:00Z",
                    "updated_at": "2026-08-01T00:00:00Z",
                    "body": "  \n "
                })
                .to_string(),
            )
            .expect(1)
            .create_async()
            .await;

        let result = tool(server.url())
            .execute(json!({ "owner": "o", "repo": "r", "issueNumber": 1 }))
            .await
            .unwrap_or_else(|error| panic!("execute failed: {error}"));

        assert_eq!(
            result,
            json!(
                "GitHub Issue (full) o/r #1\n\n\
                 - id: 1\n- number: 1\n- title: Empty\n- state: open\n\
                 - html_url: https://github.com/o/r/issues/1\n- url: https://api.github.com/repos/o/r/issues/1\n\
                 - comments_url: https://api.github.com/repos/o/r/issues/1/comments\n- events_url: https://api.github.com/repos/o/r/issues/1/events\n\
                 - labels_url: https://api.github.com/repos/o/r/issues/1/labels{/name}\n- node_id: NODE\n- comments: 0\n\
                 - created_at: 2026-08-01T00:00:00Z\n- updated_at: 2026-08-01T00:00:00Z\n\nBody:\n\n(empty)"
            )
        );
        call.assert_async().await;
    }

    #[tokio::test]
    async fn unsuccessful_response_propagates_the_github_error_body() {
        let mut server = mockito::Server::new_async().await;
        let call = server
            .mock("GET", "/repos/o/r/issues/1")
            .with_status(404)
            .with_body("{\"message\":\"Not Found\"}")
            .expect(1)
            .create_async()
            .await;

        let error = match tool(server.url())
            .execute(json!({ "owner": "o", "repo": "r", "issueNumber": 1 }))
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
            .mock("GET", "/repos/o/r/issues/1")
            .with_status(429)
            .with_body("You have exceeded a secondary rate limit")
            .expect(1)
            .create_async()
            .await;

        let error = match tool(server.url())
            .execute(json!({ "owner": "o", "repo": "r", "issueNumber": 1 }))
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
            .mock("GET", "/repos/o/r/issues/7")
            .with_status(429)
            .with_header("retry-after", "1")
            .with_body("API rate limit exceeded")
            .expect(1)
            .create_async()
            .await;
        let mut response = full_issue_response();
        response["number"] = json!(7);
        let retried = server
            .mock("GET", "/repos/o/r/issues/7")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(response.to_string())
            .expect(1)
            .create_async()
            .await;

        let result = tool(server.url())
            .execute(json!({ "owner": "o", "repo": "r", "issueNumber": 7 }))
            .await
            .unwrap_or_else(|error| panic!("execute failed: {error}"));

        assert!(
            result
                .as_str()
                .is_some_and(|text| text.starts_with("GitHub Issue (full) o/r #7\n\n- id: 100"))
        );
        limited.assert_async().await;
        retried.assert_async().await;
    }
}
