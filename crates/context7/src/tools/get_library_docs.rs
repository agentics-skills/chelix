//! `context7_get_library_docs` — fetch Context7 documentation text.

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

const DEFAULT_MINIMUM_TOKENS: f64 = 6_000.0;
const DOCUMENTATION_NOT_FOUND: &str = "Documentation not found or not finalized for this library. This might have happened because you used an invalid Context7-compatible library ID. To get a valid ID, call context7_resolve_library_id first.";

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct GetLibraryDocsInput {
    #[serde(rename = "context7CompatibleLibraryID")]
    context7_compatible_library_id: Option<String>,
    #[serde(default)]
    topic: Option<String>,
    #[serde(default)]
    tokens: Option<f64>,
}

struct NormalizedInput {
    library_id: String,
    topic: Option<String>,
    tokens: Option<f64>,
}

impl GetLibraryDocsInput {
    fn normalize(self) -> Result<NormalizedInput> {
        let library_id = self
            .context7_compatible_library_id
            .as_deref()
            .map(str::trim)
            .filter(|library_id| !library_id.is_empty())
            .ok_or_else(|| {
                Error::message("Missing required parameter: context7CompatibleLibraryID")
            })?;
        let topic = self
            .topic
            .map(|topic| topic.trim().to_string())
            .filter(|topic| !topic.is_empty());
        let tokens = self.tokens.and_then(|tokens| {
            (tokens.is_finite() && tokens > 0.0).then(|| tokens.max(DEFAULT_MINIMUM_TOKENS).trunc())
        });
        Ok(NormalizedInput {
            library_id: library_id.to_string(),
            topic,
            tokens,
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
        let clean_id = input
            .library_id
            .strip_prefix('/')
            .unwrap_or(&input.library_id);
        let mut url = url::Url::parse(&format!("{}/{clean_id}", self.client.base_url()))?;
        {
            let mut query = url.query_pairs_mut();
            if let Some(tokens) = input.tokens {
                query.append_pair("tokens", &tokens.to_string());
            }
            if let Some(topic) = &input.topic {
                query.append_pair("topic", topic);
            }
            query.append_pair("type", "txt");
        }

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
        if text.is_empty() || text == "No content available" || text == "No context data available"
        {
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
        "Fetches up-to-date documentation for a library given a Context7-compatible library ID (obtained via 'context7_resolve_library_id' or directly provided by user)."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "additionalProperties": false,
            "required": ["context7CompatibleLibraryID"],
            "properties": {
                "context7CompatibleLibraryID": {
                    "type": "string",
                    "description": "Exact Context7-compatible library ID (e.g., '/mongodb/docs', '/vercel/next.js', '/supabase/supabase', '/vercel/next.js/v14.3.0-canary.87')."
                },
                "topic": {
                    "type": "string",
                    "description": "Topic to focus documentation on (e.g., 'hooks', 'routing')."
                },
                "tokens": {
                    "type": "number",
                    "description": "Maximum number of tokens of documentation to retrieve (default: 6000). Higher values provide more context but consume more tokens."
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
    fn exposes_the_reference_description_and_schema() {
        let tool = tool("http://127.0.0.1:1".into());

        assert_eq!(tool.name(), "context7_get_library_docs");
        assert_eq!(
            tool.description(),
            "Fetches up-to-date documentation for a library given a Context7-compatible library ID (obtained via 'context7_resolve_library_id' or directly provided by user)."
        );
        let schema = tool.parameters_schema();
        assert_eq!(schema["additionalProperties"], false);
        assert_eq!(schema["required"], json!(["context7CompatibleLibraryID"]));
        assert_eq!(
            schema["properties"]["context7CompatibleLibraryID"]["description"],
            "Exact Context7-compatible library ID (e.g., '/mongodb/docs', '/vercel/next.js', '/supabase/supabase', '/vercel/next.js/v14.3.0-canary.87')."
        );
        assert_eq!(
            schema["properties"]["topic"]["description"],
            "Topic to focus documentation on (e.g., 'hooks', 'routing')."
        );
        assert_eq!(schema["properties"]["tokens"]["type"], "number");
        assert_eq!(
            schema["properties"]["tokens"]["description"],
            "Maximum number of tokens of documentation to retrieve (default: 6000). Higher values provide more context but consume more tokens."
        );
    }

    #[tokio::test]
    async fn returns_reference_text_and_normalizes_query_parameters() {
        let mut server = mockito::Server::new_async().await;
        let call = server
            .mock("GET", "/vercel/next.js")
            .match_query(mockito::Matcher::AllOf(vec![
                mockito::Matcher::UrlEncoded("tokens".into(), "6000".into()),
                mockito::Matcher::UrlEncoded("topic".into(), "hooks".into()),
                mockito::Matcher::UrlEncoded("type".into(), "txt".into()),
            ]))
            .with_status(200)
            .with_body("# Hooks\n\nDocumentation text")
            .expect(1)
            .create_async()
            .await;

        let value = tool(server.url())
            .execute(json!({
                "context7CompatibleLibraryID": " /vercel/next.js ",
                "topic": " hooks ",
                "tokens": 100
            }))
            .await
            .unwrap_or_else(|error| panic!("tool execution failed: {error}"));

        assert_eq!(value, "# Hooks\n\nDocumentation text");
        call.assert_async().await;
    }

    #[tokio::test]
    async fn returns_the_reference_not_found_message() {
        let mut server = mockito::Server::new_async().await;
        let call = server
            .mock("GET", "/unknown/library")
            .match_query(mockito::Matcher::UrlEncoded("type".into(), "txt".into()))
            .with_status(404)
            .expect(1)
            .create_async()
            .await;

        let value = tool(server.url())
            .execute(json!({"context7CompatibleLibraryID": "/unknown/library"}))
            .await
            .unwrap_or_else(|error| panic!("tool execution failed: {error}"));

        assert_eq!(value, DOCUMENTATION_NOT_FOUND);
        call.assert_async().await;
    }

    #[tokio::test(start_paused = true)]
    async fn exhausted_rate_limit_is_returned_as_a_tool_error() {
        let mut server = mockito::Server::new_async().await;
        let limited = server
            .mock("GET", "/vercel/next.js")
            .match_query(mockito::Matcher::UrlEncoded("type".into(), "txt".into()))
            .with_status(429)
            .with_header("retry-after", "1")
            .with_body("rate limited")
            .expect(1)
            .create_async()
            .await;
        let exhausted = server
            .mock("GET", "/vercel/next.js")
            .match_query(mockito::Matcher::UrlEncoded("type".into(), "txt".into()))
            .with_status(429)
            .with_body("quota exhausted")
            .expect(1)
            .create_async()
            .await;

        let error = match tool(server.url())
            .execute(json!({"context7CompatibleLibraryID": "/vercel/next.js"}))
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
                .mock("GET", "/vercel/next.js")
                .match_query(mockito::Matcher::UrlEncoded("type".into(), "txt".into()))
                .with_status(status)
                .with_body(body.clone())
                .expect(1)
                .create_async()
                .await;

            let error = match tool(server.url())
                .execute(json!({"context7CompatibleLibraryID": "/vercel/next.js"}))
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
            "context7_get_library_docs error: Missing required parameter: context7CompatibleLibraryID"
        );
    }
}
