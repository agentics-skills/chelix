//! `duckduckgo_search` agent tool.

use std::sync::Arc;

use {
    async_trait::async_trait,
    chelix_agents::tool_registry::AgentTool,
    serde::Deserialize,
    serde_json::{Value, json},
};

use crate::{
    client::DuckDuckGoClient,
    error::{Error, Result},
    metrics::record_execution,
    parser::format_results,
};

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct DuckDuckGoSearchInput {
    query: Option<String>,
    #[serde(default)]
    page: Option<u32>,
    #[serde(default)]
    num_results: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct NormalizedInput {
    query: String,
    page: u32,
    num_results: u32,
}

impl DuckDuckGoSearchInput {
    fn normalize(self) -> Result<NormalizedInput> {
        let query = self
            .query
            .as_deref()
            .map(str::trim)
            .filter(|query| !query.is_empty())
            .ok_or_else(|| Error::message("Missing required parameter: query"))?
            .to_string();
        let page = self.page.unwrap_or(1).max(1);
        let num_results = self.num_results.unwrap_or(10).max(1);
        if num_results > 20 {
            return Err(Error::message("numResults cannot exceed 20"));
        }
        Ok(NormalizedInput {
            query,
            page,
            num_results,
        })
    }
}

/// Search the web through DuckDuckGo HTML results.
pub(crate) struct DuckDuckGoSearchTool {
    client: Arc<DuckDuckGoClient>,
}

impl DuckDuckGoSearchTool {
    #[must_use]
    pub(crate) fn new(client: Arc<DuckDuckGoClient>) -> Self {
        Self { client }
    }

    async fn run(&self, params: Value) -> Result<String> {
        let input = parse_input(params)?.normalize()?;
        let items = self
            .client
            .search(&input.query, input.page, input.num_results)
            .await?;
        Ok(format_results(&input.query, input.page, &items))
    }
}

#[async_trait]
impl AgentTool for DuckDuckGoSearchTool {
    fn name(&self) -> &str {
        "duckduckgo_search"
    }

    fn description(&self) -> &str {
        "Search the web using DuckDuckGo"
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "additionalProperties": false,
            "properties": {
                "query": {
                    "type": "string",
                    "description": "The search query"
                },
                "page": {
                    "type": "integer",
                    "description": "Page number (starts at 1)",
                    "default": 1,
                    "minimum": 1
                },
                "numResults": {
                    "type": "integer",
                    "description": "Number of results to return (default: 10, max: 20)",
                    "default": 10,
                    "minimum": 1,
                    "maximum": 20
                }
            },
            "required": ["query"]
        })
    }

    fn validate(&self, params: &Value) -> anyhow::Result<()> {
        parse_input(params.clone())?.normalize()?;
        Ok(())
    }

    async fn execute(&self, params: Value) -> anyhow::Result<Value> {
        let result = self.run(params).await;
        record_execution(self.name(), result.is_ok());
        Ok(Value::String(result?))
    }
}

fn parse_input(mut params: Value) -> Result<DuckDuckGoSearchInput> {
    let map = params
        .as_object_mut()
        .ok_or_else(|| Error::message("duckduckgo_search parameters must be an object"))?;
    map.retain(|key, _| !key.starts_with('_'));
    serde_json::from_value(params)
        .map_err(|error| Error::message(format!("invalid duckduckgo_search parameters: {error}")))
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use {async_trait::async_trait, reqwest::StatusCode, url::Url};

    use crate::transport::{HttpResponse, HttpTransport};

    use super::*;

    struct EmptyTransport;

    #[async_trait]
    impl HttpTransport for EmptyTransport {
        async fn get(&self, _url: &Url) -> Result<HttpResponse> {
            Ok(HttpResponse {
                status: StatusCode::OK,
                retry_after: None,
                body: " ".repeat(1_000),
            })
        }
    }

    fn tool() -> DuckDuckGoSearchTool {
        DuckDuckGoSearchTool::new(Arc::new(DuckDuckGoClient::for_test(
            Arc::new(EmptyTransport),
            "https://duckduckgo.test/html/".to_string(),
            Duration::from_secs(30),
        )))
    }

    #[test]
    fn exposes_expected_name_description_and_schema() {
        let tool = tool();
        assert_eq!(tool.name(), "duckduckgo_search");
        assert_eq!(tool.description(), "Search the web using DuckDuckGo");
        assert_eq!(
            tool.parameters_schema(),
            json!({
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "query": {
                        "type": "string",
                        "description": "The search query"
                    },
                    "page": {
                        "type": "integer",
                        "description": "Page number (starts at 1)",
                        "default": 1,
                        "minimum": 1
                    },
                    "numResults": {
                        "type": "integer",
                        "description": "Number of results to return (default: 10, max: 20)",
                        "default": 10,
                        "minimum": 1,
                        "maximum": 20
                    }
                },
                "required": ["query"]
            })
        );
    }

    #[test]
    fn accepts_runner_internal_metadata_during_validation() {
        let tool = tool();
        tool.validate(&json!({
            "query": "latest NASA Artemis II mission update",
            "page": 1,
            "numResults": 8,
            "_session_key": "session:123",
            "_conn_id": "connection:456"
        }))
        .unwrap_or_else(|error| panic!("internal metadata was not removed: {error}"));
    }

    #[test]
    fn rejects_unknown_public_parameter_during_validation() {
        let error = match tool().validate(&json!({
            "query": "rust",
            "unexpected": true,
            "_session_key": "session:123"
        })) {
            Ok(()) => panic!("unknown public parameter should fail validation"),
            Err(error) => error,
        };
        assert!(error.to_string().contains("unknown field `unexpected`"));
    }

    #[test]
    fn normalizes_reference_defaults() {
        let input = parse_input(json!({"query": "  rust  "}))
            .and_then(DuckDuckGoSearchInput::normalize)
            .unwrap_or_else(|error| panic!("input normalization failed: {error}"));
        assert_eq!(input, NormalizedInput {
            query: "rust".to_string(),
            page: 1,
            num_results: 10,
        });
    }

    #[test]
    fn rejects_missing_query_with_reference_error() {
        let error = match parse_input(json!({})).and_then(DuckDuckGoSearchInput::normalize) {
            Ok(_) => panic!("missing query should fail"),
            Err(error) => error,
        };
        assert_eq!(error.to_string(), "Missing required parameter: query");
    }

    #[test]
    fn rejects_more_than_twenty_results_with_reference_error() {
        let error = match parse_input(json!({"query": "rust", "numResults": 21}))
            .and_then(DuckDuckGoSearchInput::normalize)
        {
            Ok(_) => panic!("too many results should fail"),
            Err(error) => error,
        };
        assert_eq!(error.to_string(), "numResults cannot exceed 20");
    }
}
