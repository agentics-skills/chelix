//! `github_search_repositories` — search repositories via
//! `GET /search/repositories`.

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

/// Maximum `per_page` accepted by the GitHub search API.
const MAX_PER_PAGE: u32 = 100;

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SearchRepositoriesInput {
    query: String,
    #[serde(default)]
    per_page: Option<u32>,
}

impl SearchRepositoriesInput {
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
struct RepositoryItem {
    full_name: String,
    description: Option<String>,
    stargazers_count: u64,
    forks_count: u64,
    language: Option<String>,
    html_url: String,
}

#[derive(Debug, Clone, Deserialize)]
struct SearchRepositoriesResponse {
    total_count: u64,
    items: Vec<RepositoryItem>,
}

/// Search repositories through the GitHub REST API.
pub struct GithubSearchRepositoriesTool {
    client: Arc<GitHubClient>,
}

impl GithubSearchRepositoriesTool {
    #[must_use]
    pub fn new(client: Arc<GitHubClient>) -> Self {
        Self { client }
    }

    async fn search_repositories(
        &self,
        input: &SearchRepositoriesInput,
    ) -> Result<SearchRepositoriesResponse> {
        let mut url = url::Url::parse(&format!("{}/search/repositories", self.client.base_url()))?;
        {
            let mut query_pairs = url.query_pairs_mut();
            query_pairs.append_pair("q", input.query.trim());
            if let Some(per_page) = input.per_page {
                query_pairs.append_pair("per_page", &per_page.to_string());
            }
        }

        let response = get_with_rate_limit_retry(&self.client, &url, self.name()).await?;
        if !response.is_success() {
            return Err(Error::message(response.failure_message()));
        }
        response.json()
    }
}

fn format_repository(repository: &RepositoryItem) -> String {
    let mut lines = vec![format!("- Name: {}", repository.full_name)];
    if let Some(description) = repository
        .description
        .as_deref()
        .filter(|description| !description.is_empty())
    {
        lines.push(format!("- Description: {description}"));
    }
    lines.push(format!("- Stars: {}", repository.stargazers_count));
    lines.push(format!("- Forks: {}", repository.forks_count));
    if let Some(language) = repository
        .language
        .as_deref()
        .filter(|language| !language.is_empty())
    {
        lines.push(format!("- Language: {language}"));
    }
    lines.push(format!("- URL: {}", repository.html_url));
    lines.join("\n")
}

fn format_results(response: &SearchRepositoriesResponse) -> String {
    if response.items.is_empty() {
        return "No repositories found for this query.".to_string();
    }
    let header = format!(
        "GitHub Repository Search Results (showing {} of {})",
        response.items.len(),
        response.total_count
    );
    let repositories = response
        .items
        .iter()
        .map(format_repository)
        .collect::<Vec<_>>()
        .join("\n----------\n");
    format!("{header}\n\n{repositories}")
}

#[async_trait]
impl AgentTool for GithubSearchRepositoriesTool {
    fn name(&self) -> &str {
        "github_search_repositories"
    }

    fn description(&self) -> &str {
        "Search repositories via GitHub API."
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
                    "description": "Search query string (e.g., 'language:TypeScript stars:>1000')."
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

impl GithubSearchRepositoriesTool {
    async fn run(&self, params: Value) -> Result<String> {
        let input = parse_input(params)?;
        let response = self.search_repositories(&input).await?;
        Ok(format_results(&response))
    }
}

fn parse_input(params: Value) -> Result<SearchRepositoriesInput> {
    let input: SearchRepositoriesInput = parse_params("github_search_repositories", params)?;
    input.validate()?;
    Ok(input)
}

#[cfg(test)]
mod tests {
    use {super::*, secrecy::Secret};

    fn tool(base_url: String, token: Option<&str>) -> GithubSearchRepositoriesTool {
        GithubSearchRepositoriesTool::new(Arc::new(GitHubClient::for_test(
            base_url,
            token.map(|value| Secret::new(value.to_string())),
        )))
    }

    #[test]
    fn exposes_the_documented_description_and_a_strict_schema() {
        let tool = tool("http://127.0.0.1:1".into(), None);

        assert_eq!(tool.name(), "github_search_repositories");
        assert_eq!(tool.description(), "Search repositories via GitHub API.");
        let schema = tool.parameters_schema();
        assert_eq!(schema["additionalProperties"], false);
        assert_eq!(schema["required"], json!(["query"]));
        assert_eq!(
            schema["properties"]["query"]["description"],
            "Search query string (e.g., 'language:TypeScript stars:>1000')."
        );
        assert_eq!(
            schema["properties"]["perPage"]["description"],
            "Number of items per page (max 100)."
        );
        assert_eq!(schema["properties"]["perPage"]["type"], "integer");
        assert_eq!(schema["properties"]["perPage"]["maximum"], 100);
    }

    #[test]
    fn rejects_blank_queries_unknown_fields_and_out_of_range_pages() {
        assert_eq!(
            parse_input(json!({ "query": "   " }))
                .err()
                .map(|error| error.to_string())
                .unwrap_or_default(),
            "Missing required parameter: query"
        );
        assert!(parse_input(json!({ "query": "needle", "per_page": 10 })).is_err());
        assert!(parse_input(json!({ "query": "needle", "perPage": 0 })).is_err());
        assert!(parse_input(json!({ "query": "needle", "perPage": 101 })).is_err());
    }

    #[test]
    fn ignores_enriched_internal_fields() {
        let input = parse_input(json!({
            "query": "needle",
            "perPage": 5,
            "_session_key": "session:test",
            "_channel": { "surface": "web" }
        }))
        .unwrap_or_else(|error| panic!("parse failed: {error}"));

        assert_eq!(input.query, "needle");
        assert_eq!(input.per_page, Some(5));
    }

    #[tokio::test]
    async fn formats_results_in_the_reference_layout_without_a_token() {
        let mut server = mockito::Server::new_async().await;
        let call = server
            .mock("GET", "/search/repositories")
            .match_query(mockito::Matcher::AllOf(vec![
                mockito::Matcher::UrlEncoded("q".into(), "language:Rust stars:>100".into()),
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
                            "full_name": "owner/alpha",
                            "description": "Alpha repository",
                            "stargazers_count": 120,
                            "forks_count": 12,
                            "language": "Rust",
                            "html_url": "https://github.com/owner/alpha"
                        },
                        {
                            "full_name": "owner/beta",
                            "description": null,
                            "stargazers_count": 110,
                            "forks_count": 8,
                            "language": null,
                            "html_url": "https://github.com/owner/beta"
                        }
                    ]
                })
                .to_string(),
            )
            .expect(1)
            .create_async()
            .await;

        let result = tool(server.url(), None)
            .execute(json!({
                "query": "language:Rust stars:>100",
                "perPage": 2
            }))
            .await
            .unwrap_or_else(|error| panic!("execute failed: {error}"));

        assert_eq!(
            result,
            json!(
                "GitHub Repository Search Results (showing 2 of 42)\n\n\
                 - Name: owner/alpha\n- Description: Alpha repository\n- Stars: 120\n- Forks: 12\n- Language: Rust\n- URL: https://github.com/owner/alpha\n\
                 ----------\n\
                 - Name: owner/beta\n- Stars: 110\n- Forks: 8\n- URL: https://github.com/owner/beta"
            )
        );
        call.assert_async().await;
    }

    #[tokio::test]
    async fn empty_result_set_uses_the_reference_message() {
        let mut server = mockito::Server::new_async().await;
        let call = server
            .mock("GET", "/search/repositories")
            .match_query(mockito::Matcher::UrlEncoded("q".into(), "needle".into()))
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(json!({ "total_count": 0, "items": [] }).to_string())
            .expect(1)
            .create_async()
            .await;

        let result = tool(server.url(), None)
            .execute(json!({ "query": "needle" }))
            .await
            .unwrap_or_else(|error| panic!("execute failed: {error}"));

        assert_eq!(result, json!("No repositories found for this query."));
        call.assert_async().await;
    }

    #[tokio::test]
    async fn validation_failure_preserves_the_github_error_body() {
        let mut server = mockito::Server::new_async().await;
        let call = server
            .mock("GET", "/search/repositories")
            .match_query(mockito::Matcher::Any)
            .with_status(422)
            .with_body("{\"message\":\"Validation Failed\"}")
            .expect(1)
            .create_async()
            .await;

        let error = match tool(server.url(), None)
            .execute(json!({ "query": "invalid" }))
            .await
        {
            Ok(_) => panic!("expected a validation error"),
            Err(error) => error,
        };

        assert_eq!(error.to_string(), "{\"message\":\"Validation Failed\"}");
        call.assert_async().await;
    }

    #[tokio::test]
    async fn unauthenticated_access_denial_reports_the_missing_token() {
        let mut server = mockito::Server::new_async().await;
        let call = server
            .mock("GET", "/search/repositories")
            .match_query(mockito::Matcher::Any)
            .with_status(401)
            .with_body("{\"message\":\"Requires authentication\"}")
            .expect(1)
            .create_async()
            .await;

        let error = match tool(server.url(), None)
            .execute(json!({ "query": "private" }))
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
            .mock("GET", "/search/repositories")
            .match_query(mockito::Matcher::Any)
            .with_status(401)
            .with_body("{\"message\":\"Bad credentials\"}")
            .expect(1)
            .create_async()
            .await;

        let error = match tool(server.url(), Some("pat-token"))
            .execute(json!({ "query": "needle" }))
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
            .mock("GET", "/search/repositories")
            .match_query(mockito::Matcher::Any)
            .with_status(429)
            .with_header("retry-after", "1")
            .with_body("API rate limit exceeded")
            .expect(1)
            .create_async()
            .await;
        let retried = server
            .mock("GET", "/search/repositories")
            .match_query(mockito::Matcher::Any)
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(json!({ "total_count": 0, "items": [] }).to_string())
            .expect(1)
            .create_async()
            .await;

        let result = tool(server.url(), None)
            .execute(json!({ "query": "needle" }))
            .await
            .unwrap_or_else(|error| panic!("execute failed: {error}"));

        assert_eq!(result, json!("No repositories found for this query."));
        limited.assert_async().await;
        retried.assert_async().await;
    }

    #[tokio::test(start_paused = true)]
    async fn repeated_rate_limit_is_not_reclassified_as_a_missing_token_error() {
        let mut server = mockito::Server::new_async().await;
        let limited = server
            .mock("GET", "/search/repositories")
            .match_query(mockito::Matcher::Any)
            .with_status(429)
            .with_header("retry-after", "1")
            .with_body("API rate limit exceeded")
            .expect(1)
            .create_async()
            .await;
        let retried = server
            .mock("GET", "/search/repositories")
            .match_query(mockito::Matcher::Any)
            .with_status(403)
            .with_body("You have exceeded a secondary rate limit")
            .expect(1)
            .create_async()
            .await;

        let error = match tool(server.url(), None)
            .execute(json!({ "query": "needle" }))
            .await
        {
            Ok(_) => panic!("expected a rate limit error"),
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
    async fn rate_limited_response_without_timing_is_not_retried() {
        let mut server = mockito::Server::new_async().await;
        let call = server
            .mock("GET", "/search/repositories")
            .match_query(mockito::Matcher::Any)
            .with_status(429)
            .with_body("You have exceeded a secondary rate limit")
            .expect(1)
            .create_async()
            .await;

        let error = match tool(server.url(), None)
            .execute(json!({ "query": "needle" }))
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
}
