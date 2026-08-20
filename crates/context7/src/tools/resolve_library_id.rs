//! `context7_resolve_library_id` — resolve a package name to Context7 library IDs.

use std::sync::Arc;

use {
    async_trait::async_trait,
    chelix_agents::tool_registry::AgentTool,
    serde::Deserialize,
    serde_json::{Value, json},
};

use crate::{
    client::Context7Client,
    error::{Error, Result},
    metrics::record_execution,
    tools::{parse_params, request::get_with_rate_limit_retry},
};

const TOOL_DESCRIPTION: &str = r#"Resolves a package/product name to a Context7-compatible library ID and returns matching libraries.

You MUST call this function before 'context7_get_library_docs' tool to obtain a valid Context7-compatible library ID UNLESS the user explicitly provides a library ID in the format '/org/project' or '/org/project/version' in their query.

Each result includes:
- Library ID: Context7-compatible identifier (format: /org/project)
- Name: Library or package name
- Description: Short summary
- Code Snippets: Number of available code examples
- Source Reputation: Authority indicator (High, Medium, Low, or Unknown)
- Benchmark Score: Quality indicator (100 is the highest score)
- Versions: List of versions if available. Use one of those versions if the user provides a version in their query. The format of the version is /org/project/version.

For best results, select libraries based on name match, source reputation, snippet coverage, benchmark score, and relevance to your use case.

Selection Process:
1. Analyze the query to understand what library/package the user is looking for
2. Return the most relevant match based on:
- Name similarity to the query (exact matches prioritized)
- Description relevance to the query's intent
- Documentation coverage (prioritize libraries with higher Code Snippet counts)
- Source reputation (consider libraries with High or Medium reputation more authoritative)
- Benchmark Score: Quality indicator (100 is the highest score)

Response Format:
- Return the selected library ID in a clearly marked section
- Provide a brief explanation for why this library was chosen
- If multiple good matches exist, acknowledge this but proceed with the most relevant one
- If no good matches exist, clearly state this and suggest query refinements

For ambiguous queries, request clarification before proceeding with a best-guess match.

IMPORTANT: Do not call this tool more than 3 times per question. If you cannot find what you need after 3 calls, use the best result you have."#;
const QUERY_DESCRIPTION: &str = "What to look up in the library's documentation. This is used to rank library results by relevance to what the user is trying to accomplish. The query is sent to the Context7 API for processing. Do not include any sensitive or confidential information such as API keys, passwords, credentials, personal data, or proprietary code in your query.";
const LIBRARY_NAME_DESCRIPTION: &str = "Library name to search for and retrieve a Context7-compatible library ID. Use the official library name with proper punctuation — e.g., 'Next.js' instead of 'nextjs', 'Customer.io' instead of 'customerio', 'Three.js' instead of 'threejs'.";
const NO_LIBRARIES_FOUND: &str = "No libraries found matching the provided name.";
const FILTERED_RESULTS_NOTE: &str = "**Note:** Your results only include libraries matching your teamspace's library filters. To adjust quality thresholds or blocked libraries, update your filters at https://context7.com/dashboard?tab=policies";

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ResolveLibraryIdInput {
    query: Option<String>,
    library_name: Option<String>,
}

struct NormalizedInput {
    query: String,
    library_name: String,
}

impl ResolveLibraryIdInput {
    fn normalize(self) -> Result<NormalizedInput> {
        let query = self
            .query
            .as_deref()
            .map(str::trim)
            .filter(|query| !query.is_empty())
            .ok_or_else(|| Error::message("Missing required parameter: query"))?;
        let library_name = self
            .library_name
            .as_deref()
            .map(str::trim)
            .filter(|library_name| !library_name.is_empty())
            .ok_or_else(|| Error::message("Missing required parameter: libraryName"))?;
        Ok(NormalizedInput {
            query: query.to_string(),
            library_name: library_name.to_string(),
        })
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Context7SearchResult {
    id: String,
    title: String,
    description: String,
    total_snippets: Option<i64>,
    trust_score: Option<f64>,
    benchmark_score: Option<f64>,
    versions: Option<Vec<String>>,
    source: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Context7SearchResponse {
    results: Vec<Context7SearchResult>,
    #[serde(default)]
    search_filter_applied: bool,
}

/// Resolve a package or product name to Context7-compatible library IDs.
pub struct Context7ResolveLibraryIdTool {
    client: Arc<Context7Client>,
}

impl Context7ResolveLibraryIdTool {
    #[must_use]
    pub fn new(client: Arc<Context7Client>) -> Self {
        Self { client }
    }

    async fn search_libraries(&self, input: &NormalizedInput) -> Result<Context7SearchResponse> {
        let mut url = url::Url::parse(&format!("{}/v2/libs/search", self.client.base_url()))?;
        url.query_pairs_mut()
            .append_pair("query", &input.query)
            .append_pair("libraryName", &input.library_name);

        let response = get_with_rate_limit_retry(&self.client, &url, self.name()).await?;
        if !response.is_success() {
            return Err(Error::message(format!(
                "Context7 request failed with HTTP {}: {}",
                response.status().as_u16(),
                response.failure_message()
            )));
        }
        response.json()
    }

    async fn run(&self, params: Value) -> Result<String> {
        let input = parse_input(params)?.normalize()?;
        let response = self.search_libraries(&input).await?;
        if response.results.is_empty() {
            return Ok(NO_LIBRARIES_FOUND.to_string());
        }
        Ok(format!(
            "Available Libraries:\n\n{}",
            format_results(&response)
        ))
    }
}

fn source_reputation(trust_score: Option<f64>) -> &'static str {
    match trust_score {
        Some(score) if score >= 7.0 => "High",
        Some(score) if score >= 4.0 => "Medium",
        Some(score) if score >= 0.0 => "Low",
        _ => "Unknown",
    }
}

fn format_result(result: &Context7SearchResult) -> String {
    let mut lines = vec![
        format!("- Title: {}", result.title),
        format!("- Context7-compatible library ID: {}", result.id),
        format!("- Description: {}", result.description),
    ];
    if let Some(total_snippets) = result.total_snippets
        && total_snippets != -1
    {
        lines.push(format!("- Code Snippets: {total_snippets}"));
    }
    lines.push(format!(
        "- Source Reputation: {}",
        source_reputation(result.trust_score)
    ));
    if let Some(benchmark_score) = result.benchmark_score
        && benchmark_score > 0.0
    {
        lines.push(format!("- Benchmark Score: {benchmark_score}"));
    }
    if let Some(versions) = result
        .versions
        .as_ref()
        .filter(|versions| !versions.is_empty())
    {
        lines.push(format!("- Versions: {}", versions.join(", ")));
    }
    if let Some(source) = result.source.as_deref().filter(|source| !source.is_empty()) {
        lines.push(format!("- Source: {source}"));
    }
    lines.join("\n")
}

fn format_results(response: &Context7SearchResponse) -> String {
    let formatted_results = response
        .results
        .iter()
        .map(format_result)
        .collect::<Vec<_>>()
        .join("\n----------\n");
    if response.search_filter_applied {
        format!("{FILTERED_RESULTS_NOTE}\n\n{formatted_results}")
    } else {
        formatted_results
    }
}

#[async_trait]
impl AgentTool for Context7ResolveLibraryIdTool {
    fn name(&self) -> &str {
        "context7_resolve_library_id"
    }

    fn description(&self) -> &str {
        TOOL_DESCRIPTION
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "additionalProperties": false,
            "required": ["query", "libraryName"],
            "properties": {
                "query": {
                    "type": "string",
                    "description": QUERY_DESCRIPTION
                },
                "libraryName": {
                    "type": "string",
                    "description": LIBRARY_NAME_DESCRIPTION
                }
            }
        })
    }

    async fn execute(&self, params: Value) -> anyhow::Result<Value> {
        let result = self.run(params).await;
        record_execution(self.name(), result.is_ok());
        match result {
            Ok(output) => Ok(Value::String(output)),
            Err(error) => Err(anyhow::anyhow!(
                "context7_resolve_library_id error: {error}"
            )),
        }
    }
}

fn parse_input(params: Value) -> Result<ResolveLibraryIdInput> {
    parse_params("context7_resolve_library_id", params)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tool(base_url: String) -> Context7ResolveLibraryIdTool {
        Context7ResolveLibraryIdTool::new(Arc::new(Context7Client::for_test(base_url, None)))
    }

    #[test]
    fn exposes_the_current_description_and_schema() {
        let tool = tool("http://127.0.0.1:1".into());

        assert_eq!(tool.name(), "context7_resolve_library_id");
        assert_eq!(tool.description(), TOOL_DESCRIPTION);
        let schema = tool.parameters_schema();
        assert_eq!(schema["additionalProperties"], false);
        assert_eq!(schema["required"], json!(["query", "libraryName"]));
        assert_eq!(schema["properties"]["query"]["type"], "string");
        assert_eq!(
            schema["properties"]["query"]["description"],
            QUERY_DESCRIPTION
        );
        assert_eq!(schema["properties"]["libraryName"]["type"], "string");
        assert_eq!(
            schema["properties"]["libraryName"]["description"],
            LIBRARY_NAME_DESCRIPTION
        );
    }

    #[tokio::test]
    async fn formats_the_current_markdown_result() {
        let mut server = mockito::Server::new_async().await;
        let call = server
            .mock("GET", "/v2/libs/search")
            .match_query(mockito::Matcher::AllOf(vec![
                mockito::Matcher::UrlEncoded(
                    "query".into(),
                    "React framework routing".into(),
                ),
                mockito::Matcher::UrlEncoded("libraryName".into(), "Next.js".into()),
            ]))
            .with_status(200)
            .with_body(
                r#"{"results":[{"id":"/vercel/next.js","title":"Next.js","description":"The React Framework","totalSnippets":42,"trustScore":9.25,"benchmarkScore":95.5,"versions":["v15","v14"],"source":"github"}],"searchFilterApplied":true}"#,
            )
            .expect(1)
            .create_async()
            .await;

        let value = tool(server.url())
            .execute(json!({
                "query": " React framework routing ",
                "libraryName": " Next.js "
            }))
            .await
            .unwrap_or_else(|error| panic!("tool execution failed: {error}"));
        let output = value
            .as_str()
            .unwrap_or_else(|| panic!("tool result is not text"));

        assert_eq!(
            output,
            "Available Libraries:\n\n**Note:** Your results only include libraries matching your teamspace's library filters. To adjust quality thresholds or blocked libraries, update your filters at https://context7.com/dashboard?tab=policies\n\n- Title: Next.js\n- Context7-compatible library ID: /vercel/next.js\n- Description: The React Framework\n- Code Snippets: 42\n- Source Reputation: High\n- Benchmark Score: 95.5\n- Versions: v15, v14\n- Source: github"
        );
        call.assert_async().await;
    }

    #[tokio::test]
    async fn unknown_snippet_count_and_reputation_are_formatted_like_the_mcp_result() {
        let mut server = mockito::Server::new_async().await;
        let call = server
            .mock("GET", "/v2/libs/search")
            .match_query(mockito::Matcher::AllOf(vec![
                mockito::Matcher::UrlEncoded("query".into(), "packages".into()),
                mockito::Matcher::UrlEncoded("libraryName".into(), "packages".into()),
            ]))
            .with_status(200)
            .with_body(
                r#"{"results":[{"id":"/unknown/snippets","title":"Unknown","description":"Unknown snippet count","totalSnippets":-1},{"id":"/known/snippets","title":"Known","description":"Known snippet count","totalSnippets":7,"trustScore":4}] }"#,
            )
            .expect(1)
            .create_async()
            .await;

        let value = tool(server.url())
            .execute(json!({"query": "packages", "libraryName": "packages"}))
            .await
            .unwrap_or_else(|error| panic!("tool execution failed: {error}"));
        let output = value
            .as_str()
            .unwrap_or_else(|| panic!("tool result is not text"));

        assert!(output.contains("- Context7-compatible library ID: /unknown/snippets"));
        assert!(output.contains("- Source Reputation: Unknown"));
        assert!(output.contains("- Context7-compatible library ID: /known/snippets"));
        assert!(output.contains("- Source Reputation: Medium"));
        assert!(!output.contains("- Code Snippets: -1"));
        assert!(output.contains("- Code Snippets: 7"));
        call.assert_async().await;
    }

    #[tokio::test]
    async fn missing_snippet_count_does_not_reject_the_search_response() {
        let mut server = mockito::Server::new_async().await;
        let call = server
            .mock("GET", "/v2/libs/search")
            .match_query(mockito::Matcher::AllOf(vec![
                mockito::Matcher::UrlEncoded("query".into(), "package".into()),
                mockito::Matcher::UrlEncoded("libraryName".into(), "A".into()),
            ]))
            .with_status(200)
            .with_body(r#"{"results":[{"id":"/a/b","title":"A","description":"D"}]}"#)
            .expect(1)
            .create_async()
            .await;

        let value = tool(server.url())
            .execute(json!({"query": "package", "libraryName": "A"}))
            .await
            .unwrap_or_else(|error| panic!("tool execution failed: {error}"));

        assert_eq!(
            value,
            "Available Libraries:\n\n- Title: A\n- Context7-compatible library ID: /a/b\n- Description: D\n- Source Reputation: Unknown"
        );
        call.assert_async().await;
    }

    #[tokio::test(start_paused = true)]
    async fn rate_limited_response_is_retried_once_after_the_shared_cooldown() {
        let mut server = mockito::Server::new_async().await;
        let query_matcher = mockito::Matcher::AllOf(vec![
            mockito::Matcher::UrlEncoded("query".into(), "routing".into()),
            mockito::Matcher::UrlEncoded("libraryName".into(), "Next.js".into()),
        ]);
        let limited = server
            .mock("GET", "/v2/libs/search")
            .match_query(query_matcher.clone())
            .with_status(429)
            .with_header("retry-after", "1")
            .with_body("rate limited")
            .expect(1)
            .create_async()
            .await;
        let retried = server
            .mock("GET", "/v2/libs/search")
            .match_query(query_matcher)
            .with_status(200)
            .with_body(r#"{"results":[]}"#)
            .expect(1)
            .create_async()
            .await;

        let value = tool(server.url())
            .execute(json!({"query": "routing", "libraryName": "Next.js"}))
            .await
            .unwrap_or_else(|error| panic!("tool execution failed: {error}"));

        assert_eq!(value, NO_LIBRARIES_FOUND);
        limited.assert_async().await;
        retried.assert_async().await;
    }

    #[tokio::test]
    async fn rate_limit_without_timing_is_not_retried() {
        let mut server = mockito::Server::new_async().await;
        let call = server
            .mock("GET", "/v2/libs/search")
            .match_query(mockito::Matcher::AllOf(vec![
                mockito::Matcher::UrlEncoded("query".into(), "routing".into()),
                mockito::Matcher::UrlEncoded("libraryName".into(), "Next.js".into()),
            ]))
            .with_status(429)
            .with_body("rate limited")
            .expect(1)
            .create_async()
            .await;

        let error = match tool(server.url())
            .execute(json!({"query": "routing", "libraryName": "Next.js"}))
            .await
        {
            Ok(value) => panic!("expected tool error, got: {value}"),
            Err(error) => error,
        };

        assert_eq!(
            error.to_string(),
            "context7_resolve_library_id error: Context7 request failed with HTTP 429: rate limited"
        );
        call.assert_async().await;
    }

    #[tokio::test]
    async fn preserves_the_registered_error_prefix() {
        let error = match tool("http://127.0.0.1:1".into()).execute(json!({})).await {
            Ok(value) => panic!("expected tool error, got: {value}"),
            Err(error) => error,
        };

        assert_eq!(
            error.to_string(),
            "context7_resolve_library_id error: Missing required parameter: query"
        );
    }
}
