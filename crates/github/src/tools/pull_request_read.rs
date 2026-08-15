//! `github_pull_request_read` — read pull request details and related data
//! through the GitHub REST API.

use std::sync::Arc;

use {
    async_trait::async_trait,
    chelix_agents::tool_registry::AgentTool,
    serde::Deserialize,
    serde_json::{Value, json},
};

use crate::{
    client::{GitHubClient, RequestOptions},
    error::{Error, Result},
    metrics::record_execution,
    tools::{
        parse_params,
        request::{get_with_rate_limit_retry, get_with_rate_limit_retry_and_options},
    },
};

/// Maximum `per_page` accepted by GitHub list endpoints.
const MAX_PER_PAGE: u32 = 100;
/// Media type used by the reference implementation for pull request diffs.
const DIFF_ACCEPT: &str = "application/vnd.github.v3.diff";
/// Maximum body excerpt length used by the reference implementation.
const BODY_EXCERPT_CHARS: usize = 400;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
enum PullRequestReadMethod {
    Get,
    GetDiff,
    GetStatus,
    GetFiles,
    GetReviewComments,
    GetReviews,
    GetComments,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct PullRequestReadInput {
    method: PullRequestReadMethod,
    owner: String,
    repo: String,
    pull_number: u64,
    #[serde(default)]
    per_page: Option<u32>,
    #[serde(default)]
    page: Option<u32>,
}

impl PullRequestReadInput {
    fn validate(&self) -> Result<()> {
        for (name, value) in [("owner", &self.owner), ("repo", &self.repo)] {
            if value.trim().is_empty() {
                return Err(Error::message(format!(
                    "Missing required parameter: {name}"
                )));
            }
        }
        if self.pull_number == 0 {
            return Err(Error::message(
                "Missing or invalid required parameter: pull_number",
            ));
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
struct PullRequestUser {
    login: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct PullRequestBranch {
    r#ref: Option<String>,
    sha: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct PullRequest {
    number: u64,
    title: String,
    state: String,
    draft: Option<bool>,
    created_at: String,
    updated_at: String,
    merged_at: Option<String>,
    body: Option<String>,
    user: Option<PullRequestUser>,
    head: Option<PullRequestBranch>,
    base: Option<PullRequestBranch>,
}

#[derive(Debug, Clone, Deserialize)]
struct CombinedStatusItem {
    context: Option<String>,
    state: Option<String>,
    description: Option<String>,
    target_url: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct CombinedStatus {
    state: String,
    sha: String,
    total_count: u64,
    statuses: Vec<CombinedStatusItem>,
}

#[derive(Debug, Clone, Deserialize)]
struct PullRequestFile {
    filename: String,
    status: String,
    additions: u64,
    deletions: u64,
    changes: u64,
    blob_url: Option<String>,
    raw_url: Option<String>,
    patch: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct PullRequestReviewComment {
    id: u64,
    user: Option<PullRequestUser>,
    path: Option<String>,
    diff_hunk: Option<String>,
    body: Option<String>,
    commit_id: Option<String>,
    html_url: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct PullRequestReview {
    id: u64,
    user: Option<PullRequestUser>,
    state: Option<String>,
    body: Option<String>,
    submitted_at: Option<String>,
    commit_id: Option<String>,
    html_url: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct IssueComment {
    id: u64,
    user: Option<PullRequestUser>,
    body: Option<String>,
    created_at: Option<String>,
    updated_at: Option<String>,
    html_url: Option<String>,
}

/// Read pull request details or related data from GitHub.
pub struct GithubPullRequestReadTool {
    client: Arc<GitHubClient>,
}

impl GithubPullRequestReadTool {
    #[must_use]
    pub fn new(client: Arc<GitHubClient>) -> Self {
        Self { client }
    }

    fn pull_request_url(&self, input: &PullRequestReadInput) -> Result<url::Url> {
        self.repository_url(input, &["pulls", &input.pull_number.to_string()])
    }

    fn repository_url(&self, input: &PullRequestReadInput, suffix: &[&str]) -> Result<url::Url> {
        let mut url = url::Url::parse(&format!("{}/repos/", self.client.base_url()))?;
        {
            let mut segments = url
                .path_segments_mut()
                .map_err(|()| Error::message("GitHub API base URL cannot have path segments"))?;
            segments.pop_if_empty();
            segments.push(input.owner.trim());
            segments.push(input.repo.trim());
            segments.extend(suffix.iter().copied());
        }
        Ok(url)
    }

    async fn fetch_json<T: serde::de::DeserializeOwned>(&self, url: &url::Url) -> Result<T> {
        let response = get_with_rate_limit_retry(&self.client, url, self.name()).await?;
        if !response.is_success() {
            return Err(Error::message(response.failure_message()));
        }
        response.json()
    }

    async fn fetch_diff(&self, url: &url::Url) -> Result<String> {
        let response =
            get_with_rate_limit_retry_and_options(&self.client, url, self.name(), RequestOptions {
                accept: Some(DIFF_ACCEPT),
                ..RequestOptions::default()
            })
            .await?;
        if !response.is_success() {
            return Err(Error::message(response.failure_message()));
        }
        if response.body().is_empty() {
            return Err(Error::message(
                "Failed to retrieve pull request diff from GitHub",
            ));
        }
        Ok(response.body().to_string())
    }

    async fn read(&self, input: &PullRequestReadInput) -> Result<String> {
        let pull_request_url = self.pull_request_url(input)?;
        let owner = input.owner.trim();
        let repo = input.repo.trim();
        let pull_number = input.pull_number;

        match input.method {
            PullRequestReadMethod::Get => {
                let pull_request = self.fetch_json::<PullRequest>(&pull_request_url).await?;
                Ok(format!(
                    "GitHub Pull Request {owner}/{repo} #{}\n\n{}",
                    pull_request.number,
                    format_pull_request(&pull_request)
                ))
            },
            PullRequestReadMethod::GetDiff => {
                let diff = self.fetch_diff(&pull_request_url).await?;
                let header = format!("# Pull Request Diff {owner}/{repo} #{pull_number}");
                Ok(format!("{header}\n\n\n\u{2063}\n\n\n```diff\n{diff}\n```"))
            },
            PullRequestReadMethod::GetStatus => {
                let pull_request = self.fetch_json::<PullRequest>(&pull_request_url).await?;
                let head_sha = pull_request
                    .head
                    .as_ref()
                    .and_then(|head| head.sha.as_deref())
                    .filter(|sha| !sha.is_empty())
                    .ok_or_else(|| {
                        Error::message("Failed to retrieve pull request (head sha missing)")
                    })?;
                let status_url = self.repository_url(input, &["commits", head_sha, "status"])?;
                let status = self.fetch_json::<CombinedStatus>(&status_url).await?;
                Ok(format!(
                    "GitHub Pull Request Status {owner}/{repo} #{pull_number}\n\n{}",
                    format_combined_status(&status)
                ))
            },
            PullRequestReadMethod::GetFiles => {
                let mut files_url = pull_request_url;
                append_path_segment(&mut files_url, "files")?;
                append_pagination(&mut files_url, input);
                let files = self.fetch_json::<Vec<PullRequestFile>>(&files_url).await?;
                Ok(format!(
                    "GitHub Pull Request Files {owner}/{repo} #{pull_number}\n\n{}",
                    format_files(&files)
                ))
            },
            PullRequestReadMethod::GetReviewComments => {
                let mut comments_url = pull_request_url;
                append_path_segment(&mut comments_url, "comments")?;
                append_pagination(&mut comments_url, input);
                let comments = self
                    .fetch_json::<Vec<PullRequestReviewComment>>(&comments_url)
                    .await?;
                Ok(format!(
                    "GitHub Pull Request Review Comments {owner}/{repo} #{pull_number}\n\n{}",
                    format_review_comments(&comments)
                ))
            },
            PullRequestReadMethod::GetReviews => {
                let mut reviews_url = pull_request_url;
                append_path_segment(&mut reviews_url, "reviews")?;
                append_pagination(&mut reviews_url, input);
                let reviews = self
                    .fetch_json::<Vec<PullRequestReview>>(&reviews_url)
                    .await?;
                Ok(format!(
                    "GitHub Pull Request Reviews {owner}/{repo} #{pull_number}\n\n{}",
                    format_reviews(&reviews)
                ))
            },
            PullRequestReadMethod::GetComments => {
                let mut comments_url =
                    self.repository_url(input, &["issues", &pull_number.to_string(), "comments"])?;
                append_pagination(&mut comments_url, input);
                let comments = self.fetch_json::<Vec<IssueComment>>(&comments_url).await?;
                Ok(format!(
                    "GitHub Pull Request Issue Comments {owner}/{repo} #{pull_number}\n\n{}",
                    format_issue_comments(&comments)
                ))
            },
        }
    }
}

fn append_path_segment(url: &mut url::Url, segment: &str) -> Result<()> {
    url.path_segments_mut()
        .map_err(|()| Error::message("GitHub API URL cannot have path segments"))?
        .push(segment);
    Ok(())
}

fn append_pagination(url: &mut url::Url, input: &PullRequestReadInput) {
    if input.per_page.is_none() && input.page.is_none() {
        return;
    }
    let mut query = url.query_pairs_mut();
    if let Some(per_page) = input.per_page {
        query.append_pair("per_page", &per_page.to_string());
    }
    if let Some(page) = input.page {
        query.append_pair("page", &page.to_string());
    }
}

fn format_pull_request(pull_request: &PullRequest) -> String {
    let mut lines = vec![
        format!("- number: {}", pull_request.number),
        format!("- title: {}", pull_request.title),
        format!(
            "- state: {}{}",
            pull_request.state,
            if pull_request.draft == Some(true) {
                " (draft)"
            } else {
                ""
            }
        ),
    ];
    if let Some(login) = pull_request
        .user
        .as_ref()
        .and_then(|user| user.login.as_deref())
        .filter(|login| !login.is_empty())
    {
        lines.push(format!("- user.login: {login}"));
    }
    if let Some(reference) = pull_request
        .base
        .as_ref()
        .and_then(|base| base.r#ref.as_deref())
        .filter(|reference| !reference.is_empty())
    {
        lines.push(format!("- base.ref: {reference}"));
    }
    if let Some(reference) = pull_request
        .head
        .as_ref()
        .and_then(|head| head.r#ref.as_deref())
        .filter(|reference| !reference.is_empty())
    {
        lines.push(format!("- head.ref: {reference}"));
    }
    lines.push(format!("- created_at: {}", pull_request.created_at));
    lines.push(format!("- updated_at: {}", pull_request.updated_at));
    if let Some(merged_at) = pull_request
        .merged_at
        .as_deref()
        .filter(|merged_at| !merged_at.is_empty())
    {
        lines.push(format!("- merged_at: {merged_at}"));
    }
    lines.push("\nBody:\n".to_string());
    lines.push(
        pull_request
            .body
            .as_deref()
            .filter(|body| !body.trim().is_empty())
            .unwrap_or("(empty)")
            .to_string(),
    );
    lines.join("\n")
}

fn format_combined_status(status: &CombinedStatus) -> String {
    let mut lines = vec![
        format!("Combined Status for {}", status.sha),
        format!("State: {}", status.state),
        format!("Checks: {}", status.total_count),
    ];
    if !status.statuses.is_empty() {
        lines.push("\nIndividual Statuses:".to_string());
        for item in &status.statuses {
            let mut parts = Vec::new();
            if let Some(context) = item.context.as_deref().filter(|value| !value.is_empty()) {
                parts.push(format!("context={context}"));
            }
            if let Some(state) = item.state.as_deref().filter(|value| !value.is_empty()) {
                parts.push(format!("state={state}"));
            }
            if let Some(description) = item
                .description
                .as_deref()
                .filter(|value| !value.is_empty())
            {
                parts.push(format!("desc={description}"));
            }
            if let Some(target_url) = item.target_url.as_deref().filter(|value| !value.is_empty()) {
                parts.push(format!("url={target_url}"));
            }
            lines.push(format!("- {}", parts.join(" | ")));
        }
    }
    lines.join("\n")
}

fn indented_block(value: &str) -> String {
    value
        .split('\n')
        .map(|line| format!("    {line}"))
        .collect::<Vec<_>>()
        .join("\n")
}

fn format_files(files: &[PullRequestFile]) -> String {
    if files.is_empty() {
        return "No files.".to_string();
    }
    files
        .iter()
        .map(|file| {
            let mut lines = vec![
                format!("- filename: {}", file.filename),
                format!("  status: {}", file.status),
                format!("  additions: {}", file.additions),
                format!("  deletions: {}", file.deletions),
                format!("  changes: {}", file.changes),
            ];
            if let Some(patch) = file.patch.as_deref().filter(|patch| !patch.is_empty()) {
                lines.push(format!("  patch: |\n{}", indented_block(patch)));
            }
            if let Some(blob_url) = file
                .blob_url
                .as_deref()
                .filter(|blob_url| !blob_url.is_empty())
            {
                lines.push(format!("  blob_url: {blob_url}"));
            }
            if let Some(raw_url) = file
                .raw_url
                .as_deref()
                .filter(|raw_url| !raw_url.is_empty())
            {
                lines.push(format!("  raw_url: {raw_url}"));
            }
            lines.join("\n")
        })
        .collect::<Vec<_>>()
        .join("\n----------\n")
}

fn excerpt(body: Option<&str>) -> String {
    let Some(body) = body.filter(|body| !body.is_empty()) else {
        return "(empty)".to_string();
    };
    let trimmed = body.trim();
    if trimmed.chars().count() <= BODY_EXCERPT_CHARS {
        return trimmed.to_string();
    }
    format!(
        "{}…",
        trimmed.chars().take(BODY_EXCERPT_CHARS).collect::<String>()
    )
}

fn format_review_comments(comments: &[PullRequestReviewComment]) -> String {
    if comments.is_empty() {
        return "No review comments.".to_string();
    }
    comments
        .iter()
        .map(|comment| {
            let mut lines = vec![format!("- id: {}", comment.id)];
            if let Some(login) = comment
                .user
                .as_ref()
                .and_then(|user| user.login.as_deref())
                .filter(|login| !login.is_empty())
            {
                lines.push(format!("  user: {login}"));
            }
            if let Some(path) = comment.path.as_deref().filter(|path| !path.is_empty()) {
                lines.push(format!("  path: {path}"));
            }
            if let Some(diff_hunk) = comment
                .diff_hunk
                .as_deref()
                .filter(|diff_hunk| !diff_hunk.is_empty())
            {
                lines.push(format!("  diff_hunk: |\n{}", indented_block(diff_hunk)));
            }
            lines.push(format!(
                "  body: |\n{}",
                indented_block(&excerpt(comment.body.as_deref()))
            ));
            if let Some(commit_id) = comment
                .commit_id
                .as_deref()
                .filter(|commit_id| !commit_id.is_empty())
            {
                lines.push(format!("  commit_id: {commit_id}"));
            }
            if let Some(html_url) = comment
                .html_url
                .as_deref()
                .filter(|html_url| !html_url.is_empty())
            {
                lines.push(format!("  url: {html_url}"));
            }
            lines.join("\n")
        })
        .collect::<Vec<_>>()
        .join("\n----------\n")
}

fn format_reviews(reviews: &[PullRequestReview]) -> String {
    if reviews.is_empty() {
        return "No reviews.".to_string();
    }
    reviews
        .iter()
        .map(|review| {
            let mut lines = vec![format!("- id: {}", review.id)];
            if let Some(login) = review
                .user
                .as_ref()
                .and_then(|user| user.login.as_deref())
                .filter(|login| !login.is_empty())
            {
                lines.push(format!("  user: {login}"));
            }
            if let Some(state) = review.state.as_deref().filter(|state| !state.is_empty()) {
                lines.push(format!("  state: {state}"));
            }
            if let Some(submitted_at) = review
                .submitted_at
                .as_deref()
                .filter(|submitted_at| !submitted_at.is_empty())
            {
                lines.push(format!("  submitted_at: {submitted_at}"));
            }
            if let Some(commit_id) = review
                .commit_id
                .as_deref()
                .filter(|commit_id| !commit_id.is_empty())
            {
                lines.push(format!("  commit_id: {commit_id}"));
            }
            if let Some(body) = review.body.as_deref().filter(|body| !body.is_empty()) {
                lines.push(format!(
                    "  body: |\n{}",
                    indented_block(&excerpt(Some(body)))
                ));
            }
            if let Some(html_url) = review
                .html_url
                .as_deref()
                .filter(|html_url| !html_url.is_empty())
            {
                lines.push(format!("  url: {html_url}"));
            }
            lines.join("\n")
        })
        .collect::<Vec<_>>()
        .join("\n----------\n")
}

fn format_issue_comments(comments: &[IssueComment]) -> String {
    if comments.is_empty() {
        return "No issue comments.".to_string();
    }
    comments
        .iter()
        .map(|comment| {
            let mut lines = vec![format!("- id: {}", comment.id)];
            if let Some(login) = comment
                .user
                .as_ref()
                .and_then(|user| user.login.as_deref())
                .filter(|login| !login.is_empty())
            {
                lines.push(format!("  user: {login}"));
            }
            lines.push(format!(
                "  body: |\n{}",
                indented_block(&excerpt(comment.body.as_deref()))
            ));
            if let Some(created_at) = comment
                .created_at
                .as_deref()
                .filter(|created_at| !created_at.is_empty())
            {
                lines.push(format!("  created_at: {created_at}"));
            }
            if let Some(updated_at) = comment
                .updated_at
                .as_deref()
                .filter(|updated_at| !updated_at.is_empty())
            {
                lines.push(format!("  updated_at: {updated_at}"));
            }
            if let Some(html_url) = comment
                .html_url
                .as_deref()
                .filter(|html_url| !html_url.is_empty())
            {
                lines.push(format!("  url: {html_url}"));
            }
            lines.join("\n")
        })
        .collect::<Vec<_>>()
        .join("\n----------\n")
}

#[async_trait]
impl AgentTool for GithubPullRequestReadTool {
    fn name(&self) -> &str {
        "github_pull_request_read"
    }

    fn description(&self) -> &str {
        "Read a pull request details or related data via GitHub API."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "additionalProperties": false,
            "required": ["method", "owner", "repo", "pullNumber"],
            "properties": {
                "method": {
                    "type": "string",
                    "enum": [
                        "get",
                        "get_diff",
                        "get_status",
                        "get_files",
                        "get_review_comments",
                        "get_reviews",
                        "get_comments"
                    ],
                    "description": "Action to retrieve PR data: get | get_diff | get_status | get_files | get_review_comments | get_reviews | get_comments"
                },
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
                "pullNumber": {
                    "type": "integer",
                    "minimum": 1,
                    "description": "Pull request number."
                },
                "perPage": {
                    "type": "integer",
                    "minimum": 1,
                    "maximum": MAX_PER_PAGE,
                    "description": "Number of items per page (for list endpoints, max 100)."
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

impl GithubPullRequestReadTool {
    async fn run(&self, params: Value) -> Result<String> {
        let input = parse_input(params)?;
        self.read(&input).await
    }
}

fn parse_input(params: Value) -> Result<PullRequestReadInput> {
    let input: PullRequestReadInput = parse_params("github_pull_request_read", params)?;
    input.validate()?;
    Ok(input)
}

#[cfg(test)]
mod tests {
    use {super::*, secrecy::Secret};

    fn tool(base_url: String, token: Option<&str>) -> GithubPullRequestReadTool {
        GithubPullRequestReadTool::new(Arc::new(GitHubClient::for_test(
            base_url,
            token.map(|value| Secret::new(value.to_string())),
        )))
    }

    fn pull_request_body(body: Value, head_sha: Option<&str>) -> String {
        json!({
            "number": 42,
            "title": "Reference pull request",
            "state": "open",
            "draft": true,
            "html_url": "https://github.com/o/r/pull/42",
            "created_at": "2026-08-01T00:00:00Z",
            "updated_at": "2026-08-02T00:00:00Z",
            "merged_at": null,
            "body": body,
            "user": { "login": "alice" },
            "head": { "ref": "feature", "sha": head_sha },
            "base": { "ref": "main", "sha": "base-sha" }
        })
        .to_string()
    }

    fn input(method: &str) -> Value {
        json!({
            "method": method,
            "owner": "o",
            "repo": "r",
            "pullNumber": 42
        })
    }

    #[test]
    fn exposes_the_reference_description_and_a_strict_schema() {
        let tool = tool("http://127.0.0.1:1".into(), None);

        assert_eq!(tool.name(), "github_pull_request_read");
        assert_eq!(
            tool.description(),
            "Read a pull request details or related data via GitHub API."
        );
        let schema = tool.parameters_schema();
        assert_eq!(schema["additionalProperties"], false);
        assert_eq!(
            schema["required"],
            json!(["method", "owner", "repo", "pullNumber"])
        );
        assert_eq!(
            schema["properties"]["method"]["description"],
            "Action to retrieve PR data: get | get_diff | get_status | get_files | get_review_comments | get_reviews | get_comments"
        );
        assert_eq!(
            schema["properties"]["method"]["enum"],
            json!([
                "get",
                "get_diff",
                "get_status",
                "get_files",
                "get_review_comments",
                "get_reviews",
                "get_comments"
            ])
        );
        for (name, description) in [
            ("owner", "Repository owner (organization or user)."),
            ("repo", "Repository name."),
            ("pullNumber", "Pull request number."),
            (
                "perPage",
                "Number of items per page (for list endpoints, max 100).",
            ),
            ("page", "Page number (1-based)."),
        ] {
            assert_eq!(schema["properties"][name]["description"], description);
        }
        assert_eq!(schema["properties"]["pullNumber"]["type"], "integer");
        assert_eq!(schema["properties"]["perPage"]["maximum"], 100);
    }

    #[test]
    fn validates_method_required_fields_numbers_and_camel_case_parameters() {
        assert!(parse_input(input("unknown")).is_err());
        assert!(
            parse_input(json!({
                "method": "get",
                "owner": " ",
                "repo": "r",
                "pullNumber": 1
            }))
            .is_err()
        );
        assert!(
            parse_input(json!({
                "method": "get",
                "owner": "o",
                "repo": "r",
                "pullNumber": 0
            }))
            .is_err()
        );
        assert!(
            parse_input(json!({
                "method": "get_files",
                "owner": "o",
                "repo": "r",
                "pull_number": 1
            }))
            .is_err()
        );
        assert!(
            parse_input(json!({
                "method": "get_files",
                "owner": "o",
                "repo": "r",
                "pullNumber": 1,
                "perPage": 101
            }))
            .is_err()
        );
        assert!(
            parse_input(json!({
                "method": "get_files",
                "owner": "o",
                "repo": "r",
                "pullNumber": 1,
                "page": 0
            }))
            .is_err()
        );
    }

    #[test]
    fn ignores_enriched_internal_fields() {
        let parsed = parse_input(json!({
            "method": "get_files",
            "owner": "o",
            "repo": "r",
            "pullNumber": 1,
            "perPage": 5,
            "_session_key": "session:test"
        }))
        .unwrap_or_else(|error| panic!("parse failed: {error}"));

        assert_eq!(parsed.method, PullRequestReadMethod::GetFiles);
        assert_eq!(parsed.per_page, Some(5));
    }

    #[tokio::test]
    async fn get_formats_pull_request_in_the_reference_layout() {
        let mut server = mockito::Server::new_async().await;
        let call = server
            .mock("GET", "/repos/o/r/pulls/42")
            .match_query(mockito::Matcher::Missing)
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(pull_request_body(
                json!("Pull request body\nsecond line"),
                Some("head-sha"),
            ))
            .expect(1)
            .create_async()
            .await;

        let result = tool(server.url(), None)
            .execute(input("get"))
            .await
            .unwrap_or_else(|error| panic!("execute failed: {error}"));

        assert_eq!(
            result,
            json!(
                "GitHub Pull Request o/r #42\n\n\
                 - number: 42\n- title: Reference pull request\n- state: open (draft)\n- user.login: alice\n- base.ref: main\n- head.ref: feature\n- created_at: 2026-08-01T00:00:00Z\n- updated_at: 2026-08-02T00:00:00Z\n\nBody:\n\nPull request body\nsecond line"
            )
        );
        call.assert_async().await;
    }

    #[tokio::test]
    async fn get_renders_an_empty_body_exactly() {
        let mut server = mockito::Server::new_async().await;
        let call = server
            .mock("GET", "/repos/o/r/pulls/42")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(pull_request_body(Value::Null, Some("head-sha")))
            .expect(1)
            .create_async()
            .await;

        let result = tool(server.url(), None)
            .execute(input("get"))
            .await
            .unwrap_or_else(|error| panic!("execute failed: {error}"));
        let text = result
            .as_str()
            .unwrap_or_else(|| panic!("tool result is not a string"));

        assert!(text.ends_with("\n\nBody:\n\n(empty)"));
        call.assert_async().await;
    }

    #[tokio::test]
    async fn get_diff_uses_the_reference_media_type_and_markdown_layout() {
        let mut server = mockito::Server::new_async().await;
        let call = server
            .mock("GET", "/repos/o/r/pulls/42")
            .match_header("accept", DIFF_ACCEPT)
            .with_status(200)
            .with_body("diff --git a/a.rs b/a.rs\n+added")
            .expect(1)
            .create_async()
            .await;

        let result = tool(server.url(), None)
            .execute(input("get_diff"))
            .await
            .unwrap_or_else(|error| panic!("execute failed: {error}"));

        assert_eq!(
            result,
            json!(
                "# Pull Request Diff o/r #42\n\n\n\u{2063}\n\n\n```diff\ndiff --git a/a.rs b/a.rs\n+added\n```"
            )
        );
        call.assert_async().await;
    }

    #[tokio::test]
    async fn get_status_reads_the_head_sha_and_formats_combined_status() {
        let mut server = mockito::Server::new_async().await;
        let pull = server
            .mock("GET", "/repos/o/r/pulls/42")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(pull_request_body(Value::Null, Some("head-sha")))
            .expect(1)
            .create_async()
            .await;
        let status = server
            .mock("GET", "/repos/o/r/commits/head-sha/status")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                json!({
                    "state": "pending",
                    "sha": "head-sha",
                    "total_count": 2,
                    "statuses": [
                        {
                            "context": "ci/test",
                            "state": "success",
                            "description": "Tests passed",
                            "target_url": "https://ci.example/test"
                        },
                        {
                            "context": "ci/lint",
                            "state": "pending",
                            "description": null,
                            "target_url": null
                        }
                    ]
                })
                .to_string(),
            )
            .expect(1)
            .create_async()
            .await;

        let result = tool(server.url(), None)
            .execute(input("get_status"))
            .await
            .unwrap_or_else(|error| panic!("execute failed: {error}"));

        assert_eq!(
            result,
            json!(
                "GitHub Pull Request Status o/r #42\n\nCombined Status for head-sha\nState: pending\nChecks: 2\n\nIndividual Statuses:\n- context=ci/test | state=success | desc=Tests passed | url=https://ci.example/test\n- context=ci/lint | state=pending"
            )
        );
        pull.assert_async().await;
        status.assert_async().await;
    }

    #[tokio::test]
    async fn get_files_sends_pagination_and_formats_patches() {
        let mut server = mockito::Server::new_async().await;
        let call = server
            .mock("GET", "/repos/o/r/pulls/42/files")
            .match_query(mockito::Matcher::AllOf(vec![
                mockito::Matcher::UrlEncoded("per_page".into(), "10".into()),
                mockito::Matcher::UrlEncoded("page".into(), "2".into()),
            ]))
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                json!([
                    {
                        "filename": "src/lib.rs",
                        "status": "modified",
                        "additions": 3,
                        "deletions": 1,
                        "changes": 4,
                        "patch": "@@ -1 +1 @@\n-old\n+new",
                        "blob_url": "https://github.com/o/r/blob/sha/src/lib.rs",
                        "raw_url": "https://github.com/o/r/raw/sha/src/lib.rs"
                    },
                    {
                        "filename": "README.md",
                        "status": "added",
                        "additions": 1,
                        "deletions": 0,
                        "changes": 1,
                        "patch": null,
                        "blob_url": null,
                        "raw_url": null
                    }
                ])
                .to_string(),
            )
            .expect(1)
            .create_async()
            .await;
        let mut params = input("get_files");
        params["perPage"] = json!(10);
        params["page"] = json!(2);

        let result = tool(server.url(), None)
            .execute(params)
            .await
            .unwrap_or_else(|error| panic!("execute failed: {error}"));

        assert_eq!(
            result,
            json!(
                "GitHub Pull Request Files o/r #42\n\n\
                 - filename: src/lib.rs\n  status: modified\n  additions: 3\n  deletions: 1\n  changes: 4\n  patch: |\n    @@ -1 +1 @@\n    -old\n    +new\n  blob_url: https://github.com/o/r/blob/sha/src/lib.rs\n  raw_url: https://github.com/o/r/raw/sha/src/lib.rs\n\
                 ----------\n\
                 - filename: README.md\n  status: added\n  additions: 1\n  deletions: 0\n  changes: 1"
            )
        );
        call.assert_async().await;
    }

    #[tokio::test]
    async fn get_review_comments_formats_blocks_and_reference_excerpts() {
        let mut server = mockito::Server::new_async().await;
        let long_body = "x".repeat(BODY_EXCERPT_CHARS + 1);
        let call = server
            .mock("GET", "/repos/o/r/pulls/42/comments")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                json!([{
                    "id": 7,
                    "user": { "login": "reviewer" },
                    "path": "src/lib.rs",
                    "diff_hunk": "@@ -1 +1 @@\n-old\n+new",
                    "body": long_body,
                    "commit_id": "commit-sha",
                    "html_url": "https://github.com/o/r/pull/42#discussion_r7"
                }])
                .to_string(),
            )
            .expect(1)
            .create_async()
            .await;

        let result = tool(server.url(), None)
            .execute(input("get_review_comments"))
            .await
            .unwrap_or_else(|error| panic!("execute failed: {error}"));
        let expected_excerpt = format!("{}…", "x".repeat(BODY_EXCERPT_CHARS));

        assert_eq!(
            result,
            json!(format!(
                "GitHub Pull Request Review Comments o/r #42\n\n\
                 - id: 7\n  user: reviewer\n  path: src/lib.rs\n  diff_hunk: |\n    @@ -1 +1 @@\n    -old\n    +new\n  body: |\n    {expected_excerpt}\n  commit_id: commit-sha\n  url: https://github.com/o/r/pull/42#discussion_r7"
            ))
        );
        call.assert_async().await;
    }

    #[tokio::test]
    async fn get_reviews_formats_the_reference_layout() {
        let mut server = mockito::Server::new_async().await;
        let call = server
            .mock("GET", "/repos/o/r/pulls/42/reviews")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                json!([{
                    "id": 8,
                    "user": { "login": "reviewer" },
                    "state": "APPROVED",
                    "body": "Looks good\nShip it",
                    "submitted_at": "2026-08-03T00:00:00Z",
                    "commit_id": "commit-sha",
                    "html_url": "https://github.com/o/r/pull/42#pullrequestreview-8"
                }])
                .to_string(),
            )
            .expect(1)
            .create_async()
            .await;

        let result = tool(server.url(), None)
            .execute(input("get_reviews"))
            .await
            .unwrap_or_else(|error| panic!("execute failed: {error}"));

        assert_eq!(
            result,
            json!(
                "GitHub Pull Request Reviews o/r #42\n\n\
                 - id: 8\n  user: reviewer\n  state: APPROVED\n  submitted_at: 2026-08-03T00:00:00Z\n  commit_id: commit-sha\n  body: |\n    Looks good\n    Ship it\n  url: https://github.com/o/r/pull/42#pullrequestreview-8"
            )
        );
        call.assert_async().await;
    }

    #[tokio::test]
    async fn get_comments_uses_the_issue_endpoint_and_formats_the_reference_layout() {
        let mut server = mockito::Server::new_async().await;
        let call = server
            .mock("GET", "/repos/o/r/issues/42/comments")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                json!([{
                    "id": 9,
                    "user": { "login": "commenter" },
                    "body": "Issue comment",
                    "created_at": "2026-08-04T00:00:00Z",
                    "updated_at": "2026-08-05T00:00:00Z",
                    "html_url": "https://github.com/o/r/pull/42#issuecomment-9"
                }])
                .to_string(),
            )
            .expect(1)
            .create_async()
            .await;

        let result = tool(server.url(), None)
            .execute(input("get_comments"))
            .await
            .unwrap_or_else(|error| panic!("execute failed: {error}"));

        assert_eq!(
            result,
            json!(
                "GitHub Pull Request Issue Comments o/r #42\n\n\
                 - id: 9\n  user: commenter\n  body: |\n    Issue comment\n  created_at: 2026-08-04T00:00:00Z\n  updated_at: 2026-08-05T00:00:00Z\n  url: https://github.com/o/r/pull/42#issuecomment-9"
            )
        );
        call.assert_async().await;
    }

    #[test]
    fn empty_related_lists_use_the_reference_messages() {
        assert_eq!(format_files(&[]), "No files.");
        assert_eq!(format_review_comments(&[]), "No review comments.");
        assert_eq!(format_reviews(&[]), "No reviews.");
        assert_eq!(format_issue_comments(&[]), "No issue comments.");
    }

    #[tokio::test]
    async fn each_unsuccessful_endpoint_propagates_the_github_error_body() {
        for (method, path) in [
            ("get", "/repos/o/r/pulls/42"),
            ("get_diff", "/repos/o/r/pulls/42"),
            ("get_status", "/repos/o/r/pulls/42"),
            ("get_files", "/repos/o/r/pulls/42/files"),
            ("get_review_comments", "/repos/o/r/pulls/42/comments"),
            ("get_reviews", "/repos/o/r/pulls/42/reviews"),
            ("get_comments", "/repos/o/r/issues/42/comments"),
        ] {
            let mut server = mockito::Server::new_async().await;
            let call = server
                .mock("GET", path)
                .with_status(404)
                .with_body("{\"message\":\"Not Found\"}")
                .expect(1)
                .create_async()
                .await;

            let error = match tool(server.url(), None).execute(input(method)).await {
                Ok(_) => panic!("expected a retrieval error for {method}"),
                Err(error) => error,
            };

            assert_eq!(error.to_string(), "{\"message\":\"Not Found\"}");
            call.assert_async().await;
        }
    }

    #[tokio::test]
    async fn missing_head_sha_uses_the_reference_error() {
        let mut server = mockito::Server::new_async().await;
        let call = server
            .mock("GET", "/repos/o/r/pulls/42")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(pull_request_body(Value::Null, None))
            .expect(1)
            .create_async()
            .await;

        let error = match tool(server.url(), None).execute(input("get_status")).await {
            Ok(_) => panic!("expected a missing head SHA error"),
            Err(error) => error,
        };

        assert_eq!(
            error.to_string(),
            "Failed to retrieve pull request (head sha missing)"
        );
        call.assert_async().await;
    }

    #[tokio::test(start_paused = true)]
    async fn diff_rate_limit_retry_preserves_the_specialized_media_type() {
        let mut server = mockito::Server::new_async().await;
        let limited = server
            .mock("GET", "/repos/o/r/pulls/42")
            .match_header("accept", DIFF_ACCEPT)
            .with_status(429)
            .with_header("retry-after", "1")
            .with_body("API rate limit exceeded")
            .expect(1)
            .create_async()
            .await;
        let retried = server
            .mock("GET", "/repos/o/r/pulls/42")
            .match_header("accept", DIFF_ACCEPT)
            .with_status(200)
            .with_body("diff body")
            .expect(1)
            .create_async()
            .await;

        let result = tool(server.url(), None)
            .execute(input("get_diff"))
            .await
            .unwrap_or_else(|error| panic!("execute failed: {error}"));

        assert_eq!(
            result,
            json!("# Pull Request Diff o/r #42\n\n\n\u{2063}\n\n\n```diff\ndiff body\n```")
        );
        limited.assert_async().await;
        retried.assert_async().await;
    }

    #[tokio::test]
    async fn diff_rate_limit_without_timing_propagates_the_github_error() {
        let mut server = mockito::Server::new_async().await;
        let call = server
            .mock("GET", "/repos/o/r/pulls/42")
            .match_header("accept", DIFF_ACCEPT)
            .with_status(429)
            .with_body("You have exceeded a secondary rate limit")
            .expect(1)
            .create_async()
            .await;

        let error = match tool(server.url(), None).execute(input("get_diff")).await {
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
