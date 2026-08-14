//! `github_search_code` — search code across GitHub via `GET /search/code`.

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
    tools::parse_params,
};

/// Maximum `per_page` accepted by the GitHub search API.
const MAX_PER_PAGE: u32 = 100;

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SearchCodeInput {
    query: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    per_page: Option<u32>,
}

impl SearchCodeInput {
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
struct SearchCodeItemRepository {
    full_name: String,
}

#[derive(Debug, Clone, Deserialize)]
struct SearchCodeItem {
    name: String,
    path: String,
    sha: String,
    html_url: String,
    repository: SearchCodeItemRepository,
}

#[derive(Debug, Clone, Deserialize)]
struct SearchCodeResponse {
    total_count: u64,
    items: Vec<SearchCodeItem>,
}

/// Search code across GitHub with the authenticated search API.
pub struct GithubSearchCodeTool {
    client: Arc<GitHubClient>,
}

impl GithubSearchCodeTool {
    #[must_use]
    pub fn new(client: Arc<GitHubClient>) -> Self {
        Self { client }
    }

    async fn search_code(&self, input: &SearchCodeInput) -> Result<SearchCodeResponse> {
        let mut url = url::Url::parse(&format!("{}/search/code", self.client.base_url()))?;
        {
            let mut query_pairs = url.query_pairs_mut();
            query_pairs.append_pair("q", input.query.trim());
            if let Some(per_page) = input.per_page {
                query_pairs.append_pair("per_page", &per_page.to_string());
            }
        }

        let response = self
            .client
            .get(&url, RequestOptions {
                return_rate_limit_response: true,
            })
            .await?;
        // One cooldown-driven retry when the response carries usable timing.
        let response = match response
            .is_rate_limited()
            .then(|| response.rate_limit_cooldown_ms())
            .flatten()
        {
            Some(cooldown_ms) => {
                #[cfg(feature = "tracing")]
                tracing::warn!(
                    cooldown_ms,
                    "github_search_code hit a GitHub rate limit; retrying once after the cooldown"
                );
                tokio::time::sleep(std::time::Duration::from_millis(cooldown_ms)).await;
                self.client.get(&url, RequestOptions::default()).await?
            },
            None => response,
        };

        if !response.is_success() {
            return Err(Error::message(response.failure_message()));
        }
        response.json()
    }
}

fn format_item(item: &SearchCodeItem) -> String {
    format!(
        "- Repo: {}\n- File: {}\n- Name: {}\n- SHA: {}\n- URL: {}",
        item.repository.full_name, item.path, item.name, item.sha, item.html_url
    )
}

fn format_results(response: &SearchCodeResponse) -> String {
    if response.items.is_empty() {
        return "No code results found for this query.".to_string();
    }
    let header = format!(
        "GitHub Code Search Results (showing {} of {})",
        response.items.len(),
        response.total_count
    );
    let items = response
        .items
        .iter()
        .map(format_item)
        .collect::<Vec<_>>()
        .join("\n----------\n");
    format!("{header}\n\n{items}")
}

#[async_trait]
impl AgentTool for GithubSearchCodeTool {
    fn name(&self) -> &str {
        "github_search_code"
    }

    fn description(&self) -> &str {
        "Search code via GitHub API."
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
                    "description": "Search code query string (e.g., 'repo:owner/name path:/src language:TypeScript')."
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

impl GithubSearchCodeTool {
    async fn run(&self, params: Value) -> Result<String> {
        let input = parse_input(params)?;
        // GitHub requires authentication for the code search endpoint.
        self.client.require_token()?;
        let response = self
            .search_code(&input)
            .await
            .map_err(|error| match error {
                Error::MissingToken => error,
                other => Error::message(format!("GitHub code search API error: {other}")),
            })?;
        Ok(format_results(&response))
    }
}

fn parse_input(params: Value) -> Result<SearchCodeInput> {
    let input: SearchCodeInput = parse_params("github_search_code", params)?;
    input.validate()?;
    Ok(input)
}

#[cfg(test)]
mod tests {
    use {super::*, secrecy::Secret};

    fn tool(base_url: String, token: Option<&str>) -> GithubSearchCodeTool {
        GithubSearchCodeTool::new(Arc::new(GitHubClient::for_test(
            base_url,
            token.map(|value| Secret::new(value.to_string())),
        )))
    }

    #[test]
    fn exposes_the_documented_description_and_a_strict_schema() {
        let tool = tool("http://127.0.0.1:1".into(), None);

        assert_eq!(tool.name(), "github_search_code");
        assert_eq!(tool.description(), "Search code via GitHub API.");
        let schema = tool.parameters_schema();
        assert_eq!(schema["additionalProperties"], false);
        assert_eq!(schema["required"], json!(["query"]));
        assert_eq!(
            schema["properties"]["query"]["description"],
            "Search code query string (e.g., 'repo:owner/name path:/src language:TypeScript')."
        );
        assert_eq!(
            schema["properties"]["perPage"]["description"],
            "Number of items per page (max 100)."
        );
        assert_eq!(schema["properties"]["perPage"]["maximum"], 100);
    }

    fn parse_error(params: Value) -> String {
        match parse_input(params) {
            Ok(input) => panic!("expected a validation error, parsed {input:?}"),
            Err(error) => error.to_string(),
        }
    }

    #[test]
    fn rejects_blank_queries_unknown_fields_and_out_of_range_pages() {
        assert_eq!(
            parse_error(json!({ "query": "   " })),
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
    async fn missing_token_fails_before_any_request() {
        let mut server = mockito::Server::new_async().await;
        let call = server
            .mock("GET", mockito::Matcher::Any)
            .expect(0)
            .create_async()
            .await;

        let error = match tool(server.url(), None)
            .execute(json!({ "query": "needle" }))
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
    async fn formats_results_in_the_documented_layout() {
        let mut server = mockito::Server::new_async().await;
        let call = server
            .mock("GET", "/search/code")
            .match_query(mockito::Matcher::AllOf(vec![
                mockito::Matcher::UrlEncoded("q".into(), "repo:o/r needle".into()),
                mockito::Matcher::UrlEncoded("per_page".into(), "2".into()),
            ]))
            .match_header("authorization", "Bearer pat-token")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                json!({
                    "total_count": 42,
                    "incomplete_results": false,
                    "items": [
                        {
                            "name": "a.rs",
                            "path": "src/a.rs",
                            "sha": "sha-a",
                            "html_url": "https://github.com/o/r/blob/main/src/a.rs",
                            "repository": { "full_name": "o/r" }
                        },
                        {
                            "name": "b.rs",
                            "path": "src/b.rs",
                            "sha": "sha-b",
                            "html_url": "https://github.com/o/r/blob/main/src/b.rs",
                            "repository": { "full_name": "o/r" }
                        }
                    ]
                })
                .to_string(),
            )
            .expect(1)
            .create_async()
            .await;

        let result = tool(server.url(), Some("pat-token"))
            .execute(json!({ "query": "repo:o/r needle", "perPage": 2 }))
            .await
            .unwrap_or_else(|error| panic!("execute failed: {error}"));

        assert_eq!(
            result,
            json!(
                "GitHub Code Search Results (showing 2 of 42)\n\n\
                 - Repo: o/r\n- File: src/a.rs\n- Name: a.rs\n- SHA: sha-a\n- URL: https://github.com/o/r/blob/main/src/a.rs\n\
                 ----------\n\
                 - Repo: o/r\n- File: src/b.rs\n- Name: b.rs\n- SHA: sha-b\n- URL: https://github.com/o/r/blob/main/src/b.rs"
            )
        );
        call.assert_async().await;
    }

    #[tokio::test]
    async fn empty_result_set_uses_the_documented_message() {
        let mut server = mockito::Server::new_async().await;
        let call = server
            .mock("GET", "/search/code")
            .match_query(mockito::Matcher::UrlEncoded("q".into(), "needle".into()))
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(json!({ "total_count": 0, "items": [] }).to_string())
            .expect(1)
            .create_async()
            .await;

        let result = tool(server.url(), Some("pat-token"))
            .execute(json!({ "query": "needle" }))
            .await
            .unwrap_or_else(|error| panic!("execute failed: {error}"));

        assert_eq!(result, json!("No code results found for this query."));
        call.assert_async().await;
    }

    #[tokio::test]
    async fn authorization_failure_is_reported_as_an_api_error() {
        let mut server = mockito::Server::new_async().await;
        let call = server
            .mock("GET", "/search/code")
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

        assert_eq!(
            error.to_string(),
            "GitHub code search API error: {\"message\":\"Bad credentials\"}"
        );
        call.assert_async().await;
    }

    #[tokio::test(start_paused = true)]
    async fn rate_limited_response_is_retried_once_after_the_cooldown() {
        let mut server = mockito::Server::new_async().await;
        let limited = server
            .mock("GET", "/search/code")
            .match_query(mockito::Matcher::Any)
            .with_status(429)
            .with_header("retry-after", "1")
            .with_body("API rate limit exceeded")
            .expect(1)
            .create_async()
            .await;
        let retried = server
            .mock("GET", "/search/code")
            .match_query(mockito::Matcher::Any)
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(json!({ "total_count": 0, "items": [] }).to_string())
            .expect(1)
            .create_async()
            .await;

        let result = tool(server.url(), Some("pat-token"))
            .execute(json!({ "query": "needle" }))
            .await
            .unwrap_or_else(|error| panic!("execute failed: {error}"));

        assert_eq!(result, json!("No code results found for this query."));
        limited.assert_async().await;
        retried.assert_async().await;
    }

    #[tokio::test]
    async fn rate_limited_response_without_timing_is_not_retried() {
        let mut server = mockito::Server::new_async().await;
        let call = server
            .mock("GET", "/search/code")
            .match_query(mockito::Matcher::Any)
            .with_status(429)
            .with_body("You have exceeded a secondary rate limit")
            .expect(1)
            .create_async()
            .await;

        let error = match tool(server.url(), Some("pat-token"))
            .execute(json!({ "query": "needle" }))
            .await
        {
            Ok(_) => panic!("expected a rate limit error"),
            Err(error) => error,
        };

        assert_eq!(
            error.to_string(),
            "GitHub code search API error: You have exceeded a secondary rate limit"
        );
        call.assert_async().await;
    }
}
