//! `linkup_search` agent tool.

use std::{collections::HashSet, sync::Arc};

use {
    async_trait::async_trait,
    chelix_agents::tool_registry::AgentTool,
    serde::Deserialize,
    serde_json::{Number, Value, json},
};

use crate::{
    api::{
        LinkupApiErrorEnvelope, LinkupApiResponse, LinkupSearchError, LinkupSearchErrorBody,
        LinkupSearchRequest,
    },
    client::LinkupClient,
    error::{Error, Result},
    metrics::record_execution,
};

const TOOL_DESCRIPTION: &str =
    "Search the web via Linkup API and return relevant results in Markdown.";
const DEFAULT_MAX_RESULTS: u64 = 5;
const MAX_DOMAINS: usize = 50;
const NUMBER_MAX_SAFE_INTEGER: u64 = 9_007_199_254_740_991;

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct LinkupDateFilterInput {
    from_date: Option<String>,
    to_date: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct LinkupSearchInput {
    query: Option<String>,
    only_search_these_domains: Option<Vec<String>>,
    date_filter: Option<LinkupDateFilterInput>,
    max_results: Option<Number>,
}

#[derive(Debug, PartialEq, Eq)]
struct NormalizedInput {
    query: String,
    domains: Option<Vec<String>>,
    from_date: Option<String>,
    to_date: Option<String>,
    max_results: u64,
}

impl LinkupSearchInput {
    fn normalize(self) -> Result<NormalizedInput> {
        let query = self
            .query
            .as_deref()
            .map(str::trim)
            .filter(|query| !query.is_empty())
            .ok_or_else(|| Error::message("Missing required parameter: query"))?
            .to_string();
        let date_filter = self.date_filter.unwrap_or_default();
        Ok(NormalizedInput {
            query,
            domains: normalize_domains(self.only_search_these_domains),
            from_date: normalize_date(date_filter.from_date)?,
            to_date: normalize_date(date_filter.to_date)?,
            max_results: normalize_max_results(self.max_results.as_ref())?,
        })
    }
}

fn normalize_max_results(raw: Option<&Number>) -> Result<u64> {
    let Some(raw) = raw else {
        return Ok(DEFAULT_MAX_RESULTS);
    };
    if let Some(value) = raw.as_u64() {
        return Ok(value.clamp(1, NUMBER_MAX_SAFE_INTEGER));
    }
    if let Some(value) = raw.as_i64() {
        if value < 1 {
            return Ok(1);
        }
        return u64::try_from(value)
            .map(|value| value.min(NUMBER_MAX_SAFE_INTEGER))
            .map_err(|error| Error::message(format!("invalid maxResults: {error}")));
    }
    Err(Error::message("invalid maxResults: expected an integer"))
}

fn strip_ascii_prefix_case_insensitive<'a>(value: &'a str, prefix: &str) -> Option<&'a str> {
    value
        .get(..prefix.len())
        .filter(|candidate| candidate.eq_ignore_ascii_case(prefix))
        .and_then(|_| value.get(prefix.len()..))
}

fn normalize_domains(raw: Option<Vec<String>>) -> Option<Vec<String>> {
    let raw = raw?;
    let mut seen = HashSet::new();
    let mut normalized = Vec::new();
    for domain in raw {
        let trimmed = domain.trim();
        if trimmed.is_empty() {
            continue;
        }
        let without_scheme = strip_ascii_prefix_case_insensitive(trimmed, "https://")
            .or_else(|| strip_ascii_prefix_case_insensitive(trimmed, "http://"))
            .unwrap_or(trimmed);
        let normalized_domain = without_scheme.strip_suffix('/').unwrap_or(without_scheme);
        let normalized_domain = normalized_domain.to_string();
        if seen.insert(normalized_domain.clone()) {
            normalized.push(normalized_domain);
            if normalized.len() == MAX_DOMAINS {
                break;
            }
        }
    }
    (!normalized.is_empty()).then_some(normalized)
}

fn normalize_date(raw: Option<String>) -> Result<Option<String>> {
    let Some(raw) = raw else {
        return Ok(None);
    };
    let value = raw.trim();
    if value.is_empty() {
        return Ok(None);
    }
    let bytes = value.as_bytes();
    let valid = bytes.len() == 10
        && bytes[0..4].iter().all(u8::is_ascii_digit)
        && bytes[4] == b'-'
        && bytes[5..7].iter().all(u8::is_ascii_digit)
        && bytes[7] == b'-'
        && bytes[8..10].iter().all(u8::is_ascii_digit);
    if !valid {
        return Err(Error::message(format!(
            "Invalid date format: {value}. Expected YYYY-MM-DD"
        )));
    }
    Ok(Some(value.to_string()))
}

fn escape_md(raw: &str) -> String {
    let mut escaped = String::with_capacity(raw.len());
    for character in raw.chars() {
        if matches!(
            character,
            '\\' | '`'
                | '*'
                | '_'
                | '{'
                | '}'
                | '['
                | ']'
                | '('
                | ')'
                | '#'
                | '+'
                | '-'
                | '.'
                | '!'
                | '|'
                | '>'
        ) {
            escaped.push('\\');
        }
        escaped.push(character);
    }
    escaped
}

fn format_results_markdown(input: &NormalizedInput, response: LinkupApiResponse) -> String {
    let mut lines = Vec::new();
    lines.push(format!(
        "### Linkup search results for \"{}\"",
        escape_md(&input.query)
    ));
    lines.push(String::new());
    lines.push(format!("- Max results: `{}`", input.max_results));
    if let Some(domains) = &input.domains {
        lines.push(format!(
            "- Only search these domains: {}",
            domains
                .iter()
                .map(|domain| format!("`{}`", escape_md(domain)))
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    if input.from_date.is_some() || input.to_date.is_some() {
        lines.push(format!(
            "- Date filter: from `{}` to `{}`",
            input.from_date.as_deref().unwrap_or("-"),
            input.to_date.as_deref().unwrap_or("-")
        ));
    }
    lines.push(String::new());

    let items = response.results.unwrap_or_default();
    if !items.is_empty() {
        for (index, item) in items.into_iter().enumerate() {
            let fallback_title = format!("Result {}", index + 1);
            let title = item
                .name
                .as_deref()
                .map(str::trim)
                .filter(|title| !title.is_empty())
                .unwrap_or(&fallback_title);
            let content = item
                .content
                .as_deref()
                .map(str::trim)
                .filter(|content| !content.is_empty())
                .or_else(|| {
                    item.snippet
                        .as_deref()
                        .map(str::trim)
                        .filter(|snippet| !snippet.is_empty())
                })
                .unwrap_or_default();
            lines.push(format!(
                "{}. [{}]({})",
                index + 1,
                escape_md(title),
                item.url.as_deref().unwrap_or("#")
            ));
            if !content.is_empty() {
                lines.push(format!("   {}", escape_md(content)));
            }
            lines.push(String::new());
        }
        return lines.join("\n");
    }

    if let Some(answer) = response
        .answer
        .as_deref()
        .map(str::trim)
        .filter(|answer| !answer.is_empty())
    {
        lines.push(answer.to_string());
        lines.push(String::new());
        let sources = response.sources.unwrap_or_default();
        if !sources.is_empty() {
            lines.push("#### Sources".to_string());
            lines.push(String::new());
            for (index, source) in sources.into_iter().enumerate() {
                let fallback_name = format!("Source {}", index + 1);
                let name = source
                    .name
                    .as_deref()
                    .map(str::trim)
                    .filter(|name| !name.is_empty())
                    .unwrap_or(&fallback_name);
                lines.push(format!(
                    "{}. [{}]({})",
                    index + 1,
                    escape_md(name),
                    source.url.as_deref().unwrap_or("#")
                ));
                if let Some(snippet) = source.snippet.filter(|snippet| !snippet.is_empty()) {
                    lines.push(format!("   {}", escape_md(&snippet)));
                }
                lines.push(String::new());
            }
        }
        return lines.join("\n");
    }

    lines.push("No results found.".to_string());
    lines.join("\n")
}

fn map_api_error(status: u16, raw_text: &str) -> LinkupSearchError {
    let parsed = serde_json::from_str::<LinkupApiErrorEnvelope>(raw_text).ok();
    let error = parsed.and_then(|parsed| parsed.error);
    let message = error
        .as_ref()
        .and_then(|error| error.message.as_deref())
        .filter(|message| !message.is_empty())
        .unwrap_or(raw_text)
        .to_string();
    LinkupSearchError {
        error: LinkupSearchErrorBody {
            code: status,
            message,
            details: error.and_then(|error| error.details),
        },
    }
}

/// Search the web through the Linkup API.
pub struct LinkupSearchTool {
    client: Arc<LinkupClient>,
}

impl LinkupSearchTool {
    #[must_use]
    pub fn new(client: Arc<LinkupClient>) -> Self {
        Self { client }
    }

    async fn run(&self, params: Value) -> Result<String> {
        let input = parse_params(params)?.normalize()?;
        let request = LinkupSearchRequest {
            q: &input.query,
            depth: "standard",
            output_type: "searchResults",
            include_images: false,
            max_results: input.max_results,
            include_domains: input.domains.as_deref(),
            from_date: input.from_date.as_deref(),
            to_date: input.to_date.as_deref(),
        };
        let response = self.client.search(&request).await?;
        if !response.is_success() {
            return Err(Error::message(serde_json::to_string_pretty(
                &map_api_error(response.status().as_u16(), response.body()),
            )?));
        }
        let parsed = match serde_json::from_str::<LinkupApiResponse>(response.body()) {
            Ok(parsed) => parsed,
            Err(_) => {
                let error = LinkupSearchError {
                    error: LinkupSearchErrorBody {
                        code: 500,
                        message: "Failed to parse Linkup response".to_string(),
                        details: None,
                    },
                };
                return Err(Error::message(serde_json::to_string_pretty(&error)?));
            },
        };
        Ok(format_results_markdown(&input, parsed))
    }
}

#[async_trait]
impl AgentTool for LinkupSearchTool {
    fn name(&self) -> &str {
        "linkup_search"
    }

    fn description(&self) -> &str {
        TOOL_DESCRIPTION
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "additionalProperties": false,
            "properties": {
                "query": {
                    "type": "string",
                    "description": "Natural-language search query."
                },
                "onlySearchTheseDomains": {
                    "type": "array",
                    "description": "Only search these domains (optional).",
                    "items": {
                        "type": "string"
                    }
                },
                "dateFilter": {
                    "type": "object",
                    "description": "Optional date filter.",
                    "additionalProperties": false,
                    "properties": {
                        "fromDate": {
                            "type": "string",
                            "description": "Start date in YYYY-MM-DD format."
                        },
                        "toDate": {
                            "type": "string",
                            "description": "End date in YYYY-MM-DD format."
                        }
                    }
                },
                "maxResults": {
                    "type": "integer",
                    "description": "Maximum number of results to return (default: 5).",
                    "default": 5,
                    "minimum": 1
                }
            },
            "required": ["query"]
        })
    }

    async fn execute(&self, params: Value) -> anyhow::Result<Value> {
        let result = self.run(params).await;
        record_execution(self.name(), result.is_ok());
        match result {
            Ok(output) => Ok(Value::String(output)),
            Err(error) => Err(anyhow::anyhow!("linkup_search error: {error}")),
        }
    }
}

fn parse_params(mut params: Value) -> Result<LinkupSearchInput> {
    let map = params
        .as_object_mut()
        .ok_or_else(|| Error::message("linkup_search parameters must be an object"))?;
    map.retain(|key, _| !key.starts_with('_'));
    serde_json::from_value(params)
        .map_err(|error| Error::message(format!("invalid linkup_search parameters: {error}")))
}

#[cfg(test)]
mod tests {
    use {
        crate::api::{LinkupResultItem, LinkupSourceItem},
        mockito::Matcher,
        secrecy::Secret,
    };

    use super::*;

    fn tool(base_url: String) -> LinkupSearchTool {
        LinkupSearchTool::new(Arc::new(LinkupClient::for_test(
            base_url,
            Some(Secret::new("api-token".to_string())),
        )))
    }

    #[test]
    fn schema_preserves_the_reference_contract_with_strict_objects() {
        let tool = LinkupSearchTool::new(Arc::new(LinkupClient::new(None, 300)));

        assert_eq!(tool.description(), TOOL_DESCRIPTION);
        assert_eq!(
            tool.parameters_schema(),
            json!({
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "query": {
                        "type": "string",
                        "description": "Natural-language search query."
                    },
                    "onlySearchTheseDomains": {
                        "type": "array",
                        "description": "Only search these domains (optional).",
                        "items": { "type": "string" }
                    },
                    "dateFilter": {
                        "type": "object",
                        "description": "Optional date filter.",
                        "additionalProperties": false,
                        "properties": {
                            "fromDate": {
                                "type": "string",
                                "description": "Start date in YYYY-MM-DD format."
                            },
                            "toDate": {
                                "type": "string",
                                "description": "End date in YYYY-MM-DD format."
                            }
                        }
                    },
                    "maxResults": {
                        "type": "integer",
                        "description": "Maximum number of results to return (default: 5).",
                        "default": 5,
                        "minimum": 1
                    }
                },
                "required": ["query"]
            })
        );
    }

    #[test]
    fn input_normalization_preserves_reference_limits() {
        let domains = (0..55)
            .map(|index| format!(" https://EXAMPLE{index}.com/ "))
            .chain(std::iter::once("https://EXAMPLE0.com/".to_string()))
            .collect();
        let input = LinkupSearchInput {
            query: Some("  rust release  ".to_string()),
            only_search_these_domains: Some(domains),
            date_filter: Some(LinkupDateFilterInput {
                from_date: Some(" 2026-01-01 ".to_string()),
                to_date: Some(String::new()),
            }),
            max_results: Some(Number::from(u64::MAX)),
        }
        .normalize()
        .unwrap_or_else(|error| panic!("normalization failed: {error}"));

        assert_eq!(input.query, "rust release");
        assert_eq!(input.max_results, NUMBER_MAX_SAFE_INTEGER);
        assert_eq!(input.domains.as_ref().map(Vec::len), Some(MAX_DOMAINS));
        assert_eq!(
            input
                .domains
                .as_ref()
                .and_then(|values| values.first())
                .map(String::as_str),
            Some("EXAMPLE0.com")
        );
        assert_eq!(input.from_date.as_deref(), Some("2026-01-01"));
        assert_eq!(input.to_date, None);
    }

    #[test]
    fn invalid_date_uses_the_reference_error() {
        let error = normalize_date(Some("2026/01/01".to_string()))
            .err()
            .unwrap_or_else(|| panic!("invalid date should fail"));

        assert_eq!(
            error.to_string(),
            "Invalid date format: 2026/01/01. Expected YYYY-MM-DD"
        );
    }

    #[test]
    fn markdown_results_match_the_reference_format() {
        let input = NormalizedInput {
            query: "rust [release]".to_string(),
            domains: Some(vec!["rust-lang.org".to_string()]),
            from_date: Some("2026-01-01".to_string()),
            to_date: None,
            max_results: 5,
        };
        let response = LinkupApiResponse {
            results: Some(vec![LinkupResultItem {
                name: Some(" Rust 1.0! ".to_string()),
                url: Some("https://rust-lang.org".to_string()),
                content: Some(" Stable *release*. ".to_string()),
                snippet: None,
            }]),
            answer: None,
            sources: None,
        };

        assert_eq!(
            format_results_markdown(&input, response),
            "### Linkup search results for \"rust \\[release\\]\"\n\n- Max results: `5`\n- Only search these domains: `rust\\-lang\\.org`\n- Date filter: from `2026-01-01` to `-`\n\n1. [Rust 1\\.0\\!](https://rust-lang.org)\n   Stable \\*release\\*\\.\n"
        );
    }

    #[test]
    fn answer_sources_and_empty_results_match_the_reference_format() {
        let input = NormalizedInput {
            query: "question".to_string(),
            domains: None,
            from_date: None,
            to_date: None,
            max_results: 5,
        };
        let answer = LinkupApiResponse {
            results: None,
            answer: Some("  **Answer**  ".to_string()),
            sources: Some(vec![LinkupSourceItem {
                name: None,
                url: None,
                snippet: Some("source.snippet".to_string()),
            }]),
        };
        let empty = LinkupApiResponse {
            results: None,
            answer: None,
            sources: None,
        };

        assert_eq!(
            format_results_markdown(&input, answer),
            "### Linkup search results for \"question\"\n\n- Max results: `5`\n\n**Answer**\n\n#### Sources\n\n1. [Source 1](#)\n   source\\.snippet\n"
        );
        assert_eq!(
            format_results_markdown(&input, empty),
            "### Linkup search results for \"question\"\n\n- Max results: `5`\n\nNo results found."
        );
    }

    #[test]
    fn api_error_mapping_preserves_status_message_and_details() {
        let error = map_api_error(
            400,
            r#"{"error":{"message":"bad input","details":[{"field":"q","message":"required"}]}}"#,
        );

        assert_eq!(
            serde_json::to_string_pretty(&error)
                .unwrap_or_else(|source| panic!("error serialization failed: {source}")),
            "{\n  \"error\": {\n    \"code\": 400,\n    \"message\": \"bad input\",\n    \"details\": [\n      {\n        \"field\": \"q\",\n        \"message\": \"required\"\n      }\n    ]\n  }\n}"
        );
    }

    #[tokio::test]
    async fn execute_returns_reference_markdown_from_the_http_response() {
        let mut server = mockito::Server::new_async().await;
        let call = server
            .mock("POST", "/v1/search")
            .match_body(Matcher::Json(json!({
                "q": "rust",
                "depth": "standard",
                "outputType": "searchResults",
                "includeImages": false,
                "maxResults": 5
            })))
            .with_status(200)
            .with_body(
                r#"{"results":[{"name":"Rust","url":"https://rust-lang.org","content":"Stable"}]}"#,
            )
            .expect(1)
            .create_async()
            .await;

        let value = tool(server.url())
            .execute(json!({"query": "rust"}))
            .await
            .unwrap_or_else(|error| panic!("tool execution failed: {error}"));

        assert_eq!(
            value,
            Value::String(
                "### Linkup search results for \"rust\"\n\n- Max results: `5`\n\n1. [Rust](https://rust-lang.org)\n   Stable\n"
                    .to_string()
            )
        );
        call.assert_async().await;
    }

    #[tokio::test]
    async fn execute_preserves_api_error_json_and_registered_prefix() {
        let mut server = mockito::Server::new_async().await;
        let call = server
            .mock("POST", "/v1/search")
            .with_status(400)
            .with_body(
                r#"{"error":{"message":"bad input","details":[{"field":"q","message":"required"}]}}"#,
            )
            .expect(1)
            .create_async()
            .await;

        let error = tool(server.url())
            .execute(json!({"query": "rust"}))
            .await
            .err()
            .unwrap_or_else(|| panic!("expected tool error"));

        assert_eq!(
            error.to_string(),
            "linkup_search error: {\n  \"error\": {\n    \"code\": 400,\n    \"message\": \"bad input\",\n    \"details\": [\n      {\n        \"field\": \"q\",\n        \"message\": \"required\"\n      }\n    ]\n  }\n}"
        );
        call.assert_async().await;
    }

    #[tokio::test]
    async fn execute_maps_malformed_success_response_to_the_reference_error() {
        let mut server = mockito::Server::new_async().await;
        let call = server
            .mock("POST", "/v1/search")
            .with_status(200)
            .with_body("not-json")
            .expect(1)
            .create_async()
            .await;

        let error = tool(server.url())
            .execute(json!({"query": "rust"}))
            .await
            .err()
            .unwrap_or_else(|| panic!("expected tool error"));

        assert_eq!(
            error.to_string(),
            "linkup_search error: {\n  \"error\": {\n    \"code\": 500,\n    \"message\": \"Failed to parse Linkup response\"\n  }\n}"
        );
        call.assert_async().await;
    }

    #[tokio::test]
    async fn execute_preserves_the_registered_parameter_error_prefix() {
        let error = tool("http://127.0.0.1:1".to_string())
            .execute(json!({}))
            .await
            .err()
            .unwrap_or_else(|| panic!("expected tool error"));

        assert_eq!(
            error.to_string(),
            "linkup_search error: Missing required parameter: query"
        );
    }
}
