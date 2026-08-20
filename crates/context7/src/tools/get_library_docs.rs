//! `context7_get_library_docs` — query Context7 documentation.

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

const TOOL_DESCRIPTION: &str = r#"Retrieves and queries up-to-date documentation and code examples from Context7 for any programming library or framework.

You must call 'context7_resolve_library_id' tool first to obtain the exact Context7-compatible library ID required to use this tool, UNLESS the user explicitly provides a library ID in the format '/org/project' or '/org/project/version' in their query.

Do not call this tool more than 3 times per question."#;
const LIBRARY_ID_DESCRIPTION: &str = "Exact Context7-compatible library ID (e.g., '/mongodb/docs', '/vercel/next.js', '/supabase/supabase', '/vercel/next.js/v14.3.0-canary.87') retrieved from 'context7_resolve_library_id' or directly from user query in the format '/org/project' or '/org/project/version'.";
const QUERY_DESCRIPTION: &str = "What to look up in the library's documentation, scoped to a single concept. Be specific and include relevant details, but keep each query to one topic — if the user's question spans multiple distinct concepts, make a separate call per concept instead of combining them, unless the question is about how the concepts interact. Good: 'How to set up authentication with JWT in Express.js' or 'React useEffect cleanup function examples'. Bad (too vague): 'auth' or 'hooks'. Bad (too broad): 'routing and auth and caching in Next.js'. The query is sent to the Context7 API for processing. Do not include any sensitive or confidential information such as API keys, passwords, credentials, personal data, or proprietary code in your query.";
const DOCUMENTATION_NOT_FOUND: &str = "Documentation not found or not finalized for this library. This might have happened because you used an invalid Context7-compatible library ID. To get a valid Context7-compatible library ID, use the 'context7_resolve_library_id' with the package name you wish to retrieve documentation for.";

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct GetLibraryDocsInput {
    library_id: Option<String>,
    query: Option<String>,
}

struct NormalizedInput {
    library_id: String,
    query: String,
}

impl GetLibraryDocsInput {
    fn normalize(self) -> Result<NormalizedInput> {
        let library_id = self
            .library_id
            .as_deref()
            .map(str::trim)
            .filter(|library_id| !library_id.is_empty())
            .ok_or_else(|| Error::message("Missing required parameter: libraryId"))?;
        let query = self
            .query
            .as_deref()
            .map(str::trim)
            .filter(|query| !query.is_empty())
            .ok_or_else(|| Error::message("Missing required parameter: query"))?;
        Ok(NormalizedInput {
            library_id: library_id.to_string(),
            query: query.to_string(),
        })
    }
}

/// Fetch up-to-date documentation for one Context7-compatible library ID.
pub struct Context7GetLibraryDocsTool {
    client: Arc<Context7Client>,
}

impl Context7GetLibraryDocsTool {
    #[must_use]
    pub fn new(client: Arc<Context7Client>) -> Self {
        Self { client }
    }

    async fn fetch_docs(&self, input: &NormalizedInput) -> Result<Option<String>> {
        let mut url = url::Url::parse(&format!("{}/v2/context", self.client.base_url()))?;
        url.query_pairs_mut()
            .append_pair("query", &input.query)
            .append_pair("libraryId", &input.library_id);

        let response = get_with_rate_limit_retry(&self.client, &url, self.name()).await?;
        if response.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(None);
        }
        if !response.is_success() {
            return Err(Error::message(format!(
                "Context7 request failed with HTTP {}: {}",
                response.status().as_u16(),
                response.failure_message()
            )));
        }
        let text = response.body();
        if text.is_empty() {
            return Ok(None);
        }
        Ok(Some(text.to_string()))
    }

    async fn run(&self, params: Value) -> Result<String> {
        let input = parse_input(params)?.normalize()?;
        Ok(self
            .fetch_docs(&input)
            .await?
            .unwrap_or_else(|| DOCUMENTATION_NOT_FOUND.to_string()))
    }
}

#[async_trait]
impl AgentTool for Context7GetLibraryDocsTool {
    fn name(&self) -> &str {
        "context7_get_library_docs"
    }

    fn description(&self) -> &str {
        TOOL_DESCRIPTION
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "additionalProperties": false,
            "required": ["libraryId", "query"],
            "properties": {
                "libraryId": {
                    "type": "string",
                    "description": LIBRARY_ID_DESCRIPTION
                },
                "query": {
                    "type": "string",
                    "description": QUERY_DESCRIPTION
                }
            }
        })
    }

    async fn execute(&self, params: Value) -> anyhow::Result<Value> {
        let result = self.run(params).await;
        record_execution(self.name(), result.is_ok());
        match result {
            Ok(output) => Ok(Value::String(output)),
            Err(error) => Err(anyhow::anyhow!("context7_get_library_docs error: {error}")),
        }
    }
}

fn parse_input(params: Value) -> Result<GetLibraryDocsInput> {
    parse_params("context7_get_library_docs", params)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tool(base_url: String) -> Context7GetLibraryDocsTool {
        Context7GetLibraryDocsTool::new(Arc::new(Context7Client::for_test(base_url, None)))
    }

    #[test]
    fn exposes_the_current_description_and_schema() {
        let tool = tool("http://127.0.0.1:1".into());

        assert_eq!(tool.name(), "context7_get_library_docs");
        assert_eq!(tool.description(), TOOL_DESCRIPTION);
        let schema = tool.parameters_schema();
        assert_eq!(schema["additionalProperties"], false);
        assert_eq!(schema["required"], json!(["libraryId", "query"]));
        assert_eq!(schema["properties"]["libraryId"]["type"], "string");
        assert_eq!(
            schema["properties"]["libraryId"]["description"],
            LIBRARY_ID_DESCRIPTION
        );
        assert_eq!(schema["properties"]["query"]["type"], "string");
        assert_eq!(
            schema["properties"]["query"]["description"],
            QUERY_DESCRIPTION
        );
    }

    #[tokio::test]
    async fn returns_context_text_unchanged() {
        let mut server = mockito::Server::new_async().await;
        let call = server
            .mock("GET", "/v2/context")
            .match_query(mockito::Matcher::AllOf(vec![
                mockito::Matcher::UrlEncoded("query".into(), "React hooks cleanup".into()),
                mockito::Matcher::UrlEncoded("libraryId".into(), "/vercel/next.js".into()),
            ]))
            .with_status(200)
            .with_body("# Hooks\n\nDocumentation text")
            .expect(1)
            .create_async()
            .await;

        let value = tool(server.url())
            .execute(json!({
                "libraryId": " /vercel/next.js ",
                "query": " React hooks cleanup "
            }))
            .await
            .unwrap_or_else(|error| panic!("tool execution failed: {error}"));

        assert_eq!(value, "# Hooks\n\nDocumentation text");
        call.assert_async().await;
    }

    #[tokio::test]
    async fn returns_the_current_not_found_message() {
        let mut server = mockito::Server::new_async().await;
        let call = server
            .mock("GET", "/v2/context")
            .match_query(mockito::Matcher::AllOf(vec![
                mockito::Matcher::UrlEncoded("query".into(), "routing".into()),
                mockito::Matcher::UrlEncoded("libraryId".into(), "/unknown/library".into()),
            ]))
            .with_status(404)
            .expect(1)
            .create_async()
            .await;

        let value = tool(server.url())
            .execute(json!({"libraryId": "/unknown/library", "query": "routing"}))
            .await
            .unwrap_or_else(|error| panic!("tool execution failed: {error}"));

        assert_eq!(value, DOCUMENTATION_NOT_FOUND);
        call.assert_async().await;
    }

    #[tokio::test]
    async fn empty_context_returns_the_current_not_found_message() {
        let mut server = mockito::Server::new_async().await;
        let call = server
            .mock("GET", "/v2/context")
            .match_query(mockito::Matcher::AllOf(vec![
                mockito::Matcher::UrlEncoded("query".into(), "routing".into()),
                mockito::Matcher::UrlEncoded("libraryId".into(), "/unknown/library".into()),
            ]))
            .with_status(200)
            .with_body("")
            .expect(1)
            .create_async()
            .await;

        let value = tool(server.url())
            .execute(json!({"libraryId": "/unknown/library", "query": "routing"}))
            .await
            .unwrap_or_else(|error| panic!("tool execution failed: {error}"));

        assert_eq!(value, DOCUMENTATION_NOT_FOUND);
        call.assert_async().await;
    }

    #[tokio::test(start_paused = true)]
    async fn exhausted_rate_limit_is_returned_as_a_tool_error() {
        let mut server = mockito::Server::new_async().await;
        let query_matcher = mockito::Matcher::AllOf(vec![
            mockito::Matcher::UrlEncoded("query".into(), "routing".into()),
            mockito::Matcher::UrlEncoded("libraryId".into(), "/vercel/next.js".into()),
        ]);
        let limited = server
            .mock("GET", "/v2/context")
            .match_query(query_matcher.clone())
            .with_status(429)
            .with_header("retry-after", "1")
            .with_body("rate limited")
            .expect(1)
            .create_async()
            .await;
        let exhausted = server
            .mock("GET", "/v2/context")
            .match_query(query_matcher)
            .with_status(429)
            .with_body("quota exhausted")
            .expect(1)
            .create_async()
            .await;

        let error = match tool(server.url())
            .execute(json!({"libraryId": "/vercel/next.js", "query": "routing"}))
            .await
        {
            Ok(value) => panic!("expected tool error, got: {value}"),
            Err(error) => error,
        };

        assert_eq!(
            error.to_string(),
            "context7_get_library_docs error: Context7 request failed with HTTP 429: quota exhausted"
        );
        limited.assert_async().await;
        exhausted.assert_async().await;
    }

    #[tokio::test]
    async fn server_errors_are_returned_as_tool_errors() {
        for status in [500, 502, 503] {
            let mut server = mockito::Server::new_async().await;
            let body = format!("server error {status}");
            let call = server
                .mock("GET", "/v2/context")
                .match_query(mockito::Matcher::AllOf(vec![
                    mockito::Matcher::UrlEncoded("query".into(), "routing".into()),
                    mockito::Matcher::UrlEncoded("libraryId".into(), "/vercel/next.js".into()),
                ]))
                .with_status(status)
                .with_body(body.clone())
                .expect(1)
                .create_async()
                .await;

            let error = match tool(server.url())
                .execute(json!({"libraryId": "/vercel/next.js", "query": "routing"}))
                .await
            {
                Ok(value) => panic!("expected tool error, got: {value}"),
                Err(error) => error,
            };

            assert_eq!(
                error.to_string(),
                format!(
                    "context7_get_library_docs error: Context7 request failed with HTTP {status}: {body}"
                )
            );
            call.assert_async().await;
        }
    }

    #[tokio::test]
    async fn preserves_the_registered_error_prefix() {
        let error = match tool("http://127.0.0.1:1".into()).execute(json!({})).await {
            Ok(value) => panic!("expected tool error, got: {value}"),
            Err(error) => error,
        };

        assert_eq!(
            error.to_string(),
            "context7_get_library_docs error: Missing required parameter: libraryId"
        );
    }
}
