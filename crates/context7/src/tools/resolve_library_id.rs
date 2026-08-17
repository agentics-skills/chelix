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

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ResolveLibraryIdInput {
    library_name: Option<String>,
}

impl ResolveLibraryIdInput {
    fn normalize(self) -> Result<String> {
        let library_name = self
            .library_name
            .as_deref()
            .map(str::trim)
            .filter(|library_name| !library_name.is_empty())
            .ok_or_else(|| Error::message("Missing required parameter: libraryName"))?;
        Ok(library_name.to_string())
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
    versions: Option<Vec<String>>,
}

#[derive(Debug, Clone, Deserialize)]
struct Context7SearchResponse {
    results: Vec<Context7SearchResult>,
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

    async fn search_libraries(&self, query: &str) -> Result<Context7SearchResponse> {
        let mut url = url::Url::parse(&format!("{}/search", self.client.base_url()))?;
        url.query_pairs_mut().append_pair("query", query);

        let response = get_with_rate_limit_retry(&self.client, &url, self.name()).await?;
        if !response.is_success() {
            return Err(Error::message(
                "Failed to retrieve library documentation data from Context7",
            ));
        }
        response.json()
    }

    async fn run(&self, params: Value) -> Result<String> {
        let query = parse_input(params)?.normalize()?;
        let response = self.search_libraries(&query).await?;
        let body = format_results(&response);
        Ok(format!(
            "Available Libraries (top matches):\n\nEach result includes:\n- Library ID: Context7-compatible identifier (format: /org/project)\n- Name: Library or package name\n- Description: Short summary\n- Code Snippets: Number of available code examples\n- Trust Score: Authority indicator\n- Versions: List of versions if available. Use one of those versions if and only if the user explicitly provides a version in their query.\n\nFor best results, select libraries based on name match, trust score, snippet coverage, and relevance to your use case.\n\n----------\n\n{body}"
        ))
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
    if let Some(trust_score) = result.trust_score
        && trust_score >= 0.0
    {
        let rounded = (trust_score * 10.0).round() / 10.0;
        lines.push(format!("- Trust Score: {rounded:.1}"));
    }
    if let Some(versions) = result
        .versions
        .as_ref()
        .filter(|versions| !versions.is_empty())
    {
        lines.push(format!("- Versions: {}", versions.join(", ")));
    }
    lines.join("\n")
}

fn format_results(response: &Context7SearchResponse) -> String {
    if response.results.is_empty() {
        return "No documentation libraries found matching your query.".to_string();
    }
    response
        .results
        .iter()
        .map(format_result)
        .collect::<Vec<_>>()
        .join("\n----------\n")
}

#[async_trait]
impl AgentTool for Context7ResolveLibraryIdTool {
    fn name(&self) -> &str {
        "context7_resolve_library_id"
    }

    fn description(&self) -> &str {
        "Resolves a package/product name to a Context7-compatible library ID and returns a list of matching libraries. Call before 'context7_get_library_docs' unless user explicitly provides an ID."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "additionalProperties": false,
            "required": ["libraryName"],
            "properties": {
                "libraryName": {
                    "type": "string",
                    "description": "Library name to search for and retrieve a Context7-compatible library ID."
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
    fn exposes_the_reference_description_and_schema() {
        let tool = tool("http://127.0.0.1:1".into());

        assert_eq!(tool.name(), "context7_resolve_library_id");
        assert_eq!(
            tool.description(),
            "Resolves a package/product name to a Context7-compatible library ID and returns a list of matching libraries. Call before 'context7_get_library_docs' unless user explicitly provides an ID."
        );
        let schema = tool.parameters_schema();
        assert_eq!(schema["additionalProperties"], false);
        assert_eq!(schema["required"], json!(["libraryName"]));
        assert_eq!(schema["properties"]["libraryName"]["type"], "string");
        assert_eq!(
            schema["properties"]["libraryName"]["description"],
            "Library name to search for and retrieve a Context7-compatible library ID."
        );
    }

    #[tokio::test]
    async fn formats_the_reference_markdown_result() {
        let mut server = mockito::Server::new_async().await;
        let call = server
            .mock("GET", "/search")
            .match_query(mockito::Matcher::UrlEncoded(
                "query".into(),
                "Next.js".into(),
            ))
            .with_status(200)
            .with_body(
                r#"{"results":[{"id":"/vercel/next.js","title":"Next.js","description":"The React Framework","totalSnippets":42,"trustScore":9.25,"versions":["v15","v14"]}]}"#,
            )
            .expect(1)
            .create_async()
            .await;

        let value = tool(server.url())
            .execute(json!({"libraryName": " Next.js "}))
            .await
            .unwrap_or_else(|error| panic!("tool execution failed: {error}"));
        let output = value
            .as_str()
            .unwrap_or_else(|| panic!("tool result is not text"));

        assert_eq!(
            output,
            "Available Libraries (top matches):\n\nEach result includes:\n- Library ID: Context7-compatible identifier (format: /org/project)\n- Name: Library or package name\n- Description: Short summary\n- Code Snippets: Number of available code examples\n- Trust Score: Authority indicator\n- Versions: List of versions if available. Use one of those versions if and only if the user explicitly provides a version in their query.\n\nFor best results, select libraries based on name match, trust score, snippet coverage, and relevance to your use case.\n\n----------\n\n- Title: Next.js\n- Context7-compatible library ID: /vercel/next.js\n- Description: The React Framework\n- Code Snippets: 42\n- Trust Score: 9.3\n- Versions: v15, v14"
        );
        call.assert_async().await;
    }

    #[tokio::test]
    async fn unknown_snippet_count_sentinel_does_not_reject_the_search_response() {
        let mut server = mockito::Server::new_async().await;
        let call = server
            .mock("GET", "/search")
            .match_query(mockito::Matcher::UrlEncoded(
                "query".into(),
                "packages".into(),
            ))
            .with_status(200)
            .with_body(
                r#"{"results":[{"id":"/unknown/snippets","title":"Unknown","description":"Unknown snippet count","totalSnippets":-1},{"id":"/known/snippets","title":"Known","description":"Known snippet count","totalSnippets":7}]}"#,
            )
            .expect(1)
            .create_async()
            .await;

        let value = tool(server.url())
            .execute(json!({"libraryName": "packages"}))
            .await
            .unwrap_or_else(|error| panic!("tool execution failed: {error}"));
        let output = value
            .as_str()
            .unwrap_or_else(|| panic!("tool result is not text"));

        assert!(output.contains("- Context7-compatible library ID: /unknown/snippets"));
        assert!(output.contains("- Context7-compatible library ID: /known/snippets"));
        assert!(!output.contains("- Code Snippets: -1"));
        assert!(output.contains("- Code Snippets: 7"));
        call.assert_async().await;
    }

    #[tokio::test(start_paused = true)]
    async fn rate_limited_response_is_retried_once_after_the_shared_cooldown() {
        let mut server = mockito::Server::new_async().await;
        let limited = server
            .mock("GET", "/search")
            .match_query(mockito::Matcher::UrlEncoded(
                "query".into(),
                "Next.js".into(),
            ))
            .with_status(429)
            .with_header("retry-after", "1")
            .with_body("rate limited")
            .expect(1)
            .create_async()
            .await;
        let retried = server
            .mock("GET", "/search")
            .match_query(mockito::Matcher::UrlEncoded(
                "query".into(),
                "Next.js".into(),
            ))
            .with_status(200)
            .with_body(r#"{"results":[]}"#)
            .expect(1)
            .create_async()
            .await;

        let value = tool(server.url())
            .execute(json!({"libraryName": "Next.js"}))
            .await
            .unwrap_or_else(|error| panic!("tool execution failed: {error}"));
        let output = value
            .as_str()
            .unwrap_or_else(|| panic!("tool result is not text"));

        assert!(output.ends_with("No documentation libraries found matching your query."));
        limited.assert_async().await;
        retried.assert_async().await;
    }

    #[tokio::test]
    async fn rate_limit_without_timing_is_not_retried() {
        let mut server = mockito::Server::new_async().await;
        let call = server
            .mock("GET", "/search")
            .match_query(mockito::Matcher::UrlEncoded(
                "query".into(),
                "Next.js".into(),
            ))
            .with_status(429)
            .with_body("rate limited")
            .expect(1)
            .create_async()
            .await;

        let error = match tool(server.url())
            .execute(json!({"libraryName": "Next.js"}))
            .await
        {
            Ok(value) => panic!("expected tool error, got: {value}"),
            Err(error) => error,
        };

        assert_eq!(
            error.to_string(),
            "context7_resolve_library_id error: Failed to retrieve library documentation data from Context7"
        );
        call.assert_async().await;
    }

    #[tokio::test]
    async fn preserves_the_reference_error_prefix() {
        let error = match tool("http://127.0.0.1:1".into()).execute(json!({})).await {
            Ok(value) => panic!("expected tool error, got: {value}"),
            Err(error) => error,
        };

        assert_eq!(
            error.to_string(),
            "context7_resolve_library_id error: Missing required parameter: libraryName"
        );
    }
}
