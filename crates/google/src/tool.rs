//! Google Programmable Search HTTP request and paginated text output.
use {
    crate::rate_limit::RateLimitCoordinator,
    async_trait::async_trait,
    chelix_agents::{tool_context::ToolExecutionContext, tool_registry::AgentTool},
    chelix_config::schema::GoogleConfig,
    secrecy::ExposeSecret,
    serde::Deserialize,
    serde_json::{Number, Value, json},
    std::{sync::Arc, time::Duration},
};

const DESCRIPTION: &str = "Search Google and return relevant results from the web. This tool finds web pages, articles, and information on specific topics using Google's search engine. Results include titles, snippets, and URLs that can be analyzed further.";
const BASE_URL: &str = "https://www.googleapis.com/customsearch/v1";

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Input {
    query: Option<String>,
    #[serde(rename = "num_results")]
    num_results: Option<Number>,
    site: Option<String>,
    language: Option<String>,
    date_restrict: Option<String>,
    exact_terms: Option<String>,
    result_type: Option<String>,
    page: Option<Number>,
    results_per_page: Option<Number>,
    sort: Option<String>,
}

fn params(mut value: Value) -> anyhow::Result<Input> {
    let map = value
        .as_object_mut()
        .ok_or_else(|| anyhow::anyhow!("google_search parameters must be an object"))?;
    map.retain(|name, _| !name.starts_with('_'));
    Ok(serde_json::from_value(value)?)
}

fn normalized_int(value: Option<Number>, default: i64, maximum: i64) -> i64 {
    let Some(value) = value else {
        return default;
    };
    if let Some(integer) = value.as_i64() {
        return integer.clamp(1, maximum);
    }
    if let Some(integer) = value.as_u64() {
        return integer.min(maximum as u64) as i64;
    }
    value
        .as_f64()
        .map(|number| (number.trunc() as i64).clamp(1, maximum))
        .unwrap_or(default)
}

fn format_results(query: &str, page: i64, response: &Value) -> String {
    let items = response.get("items").and_then(Value::as_array);
    let Some(items) = items.filter(|items| !items.is_empty()) else {
        return "No results found. Try:\n- Using different keywords\n- Removing quotes from non-exact phrases\n- Using more general terms".to_string();
    };
    let total = response
        .pointer("/searchInformation/totalResults")
        .or_else(|| response.pointer("/queries/request/0/totalResults"))
        .and_then(Value::as_str)
        .filter(|total| !total.is_empty());
    let mut lines = vec![format!("Search results for \"{query}\":"), String::new()];
    match total.filter(|total| total.parse::<f64>().is_ok()) {
        Some(total) => lines.push(format!(
            "Showing page {page} of approximately {total} results"
        )),
        None => lines.push(format!("Showing page {page}")),
    }
    lines.push(String::new());
    for (index, item) in items.iter().enumerate() {
        let title = item
            .get("title")
            .and_then(Value::as_str)
            .unwrap_or("undefined");
        let link = item
            .get("link")
            .and_then(Value::as_str)
            .unwrap_or("undefined");
        lines.push(format!("{}. {title}", index + 1));
        lines.push(format!("   URL: {link}"));
        if let Some(snippet) = item
            .get("snippet")
            .and_then(Value::as_str)
            .filter(|text| !text.is_empty())
        {
            lines.push(format!("   {snippet}"));
        }
        lines.push(String::new());
    }
    let has_previous = response
        .pointer("/queries/previousPage")
        .and_then(Value::as_array)
        .is_some_and(|pages| !pages.is_empty());
    let has_next = response
        .pointer("/queries/nextPage")
        .and_then(Value::as_array)
        .is_some_and(|pages| !pages.is_empty());
    if has_previous || has_next {
        let mut parts = Vec::new();
        if has_previous && page > 1 {
            parts.push(format!("Use 'page: {}' for previous results.", page - 1));
        }
        if has_next {
            parts.push(format!("Use 'page: {}' for more results.", page + 1));
        }
        if !parts.is_empty() {
            lines.push(format!("Navigation: {}", parts.join(" ")));
        }
    }
    lines.join("\n")
}

pub struct GoogleSearchTool {
    config: Arc<GoogleConfig>,
    http: reqwest::Client,
    gate: RateLimitCoordinator,
    base_url: String,
    request_timeout: Option<Duration>,
}

impl GoogleSearchTool {
    pub fn new(config: Arc<GoogleConfig>) -> Self {
        let timeout = Duration::from_secs(config.request_timeout_secs);
        Self {
            config,
            http: chelix_common::http_client::build_default_http_client(),
            gate: RateLimitCoordinator::default(),
            base_url: BASE_URL.into(),
            request_timeout: Some(timeout),
        }
    }

    fn request_error(&self, error: reqwest::Error) -> anyhow::Error {
        use std::error::Error as _;
        let error = error.without_url();
        let mut message = if error.is_timeout() {
            format!(
                "Google request timed out after {:?}: {error}",
                Duration::from_secs(self.config.request_timeout_secs)
            )
        } else {
            error.to_string()
        };
        let mut cause = error.source();
        while let Some(source) = cause {
            message.push_str(": ");
            message.push_str(&source.to_string());
            cause = source.source();
        }
        #[cfg(feature = "tracing")]
        tracing::warn!(error = %message, "Google search transport failure");
        anyhow::anyhow!(message)
    }

    async fn send_once(
        &self,
        request: reqwest::RequestBuilder,
    ) -> anyhow::Result<(reqwest::Response, bool)> {
        let permit = self.gate.acquire().await;
        let response = request
            .send()
            .await
            .map_err(|error| self.request_error(error))?;
        let retry_after = response
            .headers()
            .get(reqwest::header::RETRY_AFTER)
            .and_then(|value| value.to_str().ok());
        let cooldown = permit.complete(
            response.status() == reqwest::StatusCode::TOO_MANY_REQUESTS,
            retry_after,
        );
        Ok((response, cooldown))
    }

    async fn send_with_retry(
        &self,
        context: Option<&ToolExecutionContext>,
        make_request: impl Fn() -> reqwest::RequestBuilder,
    ) -> anyhow::Result<reqwest::Response> {
        let (response, cooldown) = self.send_once(make_request()).await?;
        if !cooldown {
            return Ok(response);
        }
        #[cfg(feature = "tracing")]
        tracing::warn!(
            tool = "google_search",
            "Google rate limited request; retrying once after shared cooldown"
        );
        if let Some(context) = context {
            context.record_retry();
        }
        self.send_once(make_request())
            .await
            .map(|(response, _)| response)
    }

    #[cfg(test)]
    fn for_test(config: Arc<GoogleConfig>, base_url: String) -> Self {
        Self {
            base_url,
            request_timeout: None,
            ..Self::new(config)
        }
    }

    #[cfg_attr(feature = "tracing", tracing::instrument(skip(self, value, context)))]
    async fn run(
        &self,
        value: Value,
        context: Option<&ToolExecutionContext>,
    ) -> anyhow::Result<String> {
        let input = params(value)?;
        let query = input
            .query
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| anyhow::anyhow!("Missing required parameter: query"))?;
        let api_key = self
            .config
            .token
            .as_ref()
            .map(ExposeSecret::expose_secret)
            .map(String::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| {
                anyhow::anyhow!("Google API key is not configured in tools.google.token")
            })?;
        let engine_id = self
            .config
            .engine_id
            .as_ref()
            .map(ExposeSecret::expose_secret)
            .map(String::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "Google search engine ID is not configured in tools.google.engine_id"
                )
            })?;
        let page = normalized_int(input.page, 1, 9_007_199_254_740_991);
        let per_page = normalized_int(input.results_per_page.or(input.num_results), 5, 10);
        let start = (page - 1).saturating_mul(per_page).saturating_add(1);
        let mut url = url::Url::parse(&self.base_url)?;
        {
            let mut pairs = url.query_pairs_mut();
            pairs
                .append_pair("key", api_key)
                .append_pair("cx", engine_id)
                .append_pair("q", query)
                .append_pair("num", &per_page.to_string())
                .append_pair("start", &start.to_string());
            if let Some(site) = input.site.as_deref().filter(|value| !value.is_empty()) {
                pairs
                    .append_pair("siteSearch", site)
                    .append_pair("siteSearchFilter", "i");
            }
            if let Some(language) = input.language.as_deref().filter(|value| !value.is_empty()) {
                let lowered = language.to_lowercase();
                let language_restriction = if lowered.starts_with("lang_") {
                    lowered
                } else {
                    format!("lang_{lowered}")
                };
                pairs.append_pair("lr", &language_restriction);
            }
            if let Some(date) = input
                .date_restrict
                .as_deref()
                .filter(|value| !value.is_empty())
            {
                pairs.append_pair("dateRestrict", date);
            }
            if let Some(exact) = input
                .exact_terms
                .as_deref()
                .filter(|value| !value.is_empty())
            {
                pairs.append_pair("exactTerms", exact);
            }
            if input.result_type.as_deref().is_some_and(|value| {
                value.eq_ignore_ascii_case("image") || value.eq_ignore_ascii_case("images")
            }) {
                pairs.append_pair("searchType", "image");
            }
            if input
                .sort
                .as_deref()
                .is_some_and(|value| value.eq_ignore_ascii_case("date"))
            {
                pairs.append_pair("sort", "date");
            }
        }
        let response = self
            .send_with_retry(context, || {
                let request = self
                    .http
                    .get(url.clone())
                    .header(reqwest::header::USER_AGENT, "chelix");
                if let Some(timeout) = self.request_timeout {
                    request.timeout(timeout)
                } else {
                    request
                }
            })
            .await?;
        let status = response.status();
        let text = response
            .text()
            .await
            .map_err(|error| self.request_error(error))?;
        let parsed: Value = serde_json::from_str(&text).unwrap_or_else(|_| json!({
            "error": { "code": if status.is_success() { 500 } else { status.as_u16() },
                "message": if status.is_success() { "Failed to parse Google Search response" } else { &text } }
        }));
        if parsed.get("error").is_some_and(|error| match error {
            Value::Null | Value::Bool(false) => false,
            Value::Number(number) => number.as_f64().is_some_and(|value| value != 0.0),
            Value::String(text) => !text.is_empty(),
            Value::Array(_) | Value::Object(_) | Value::Bool(true) => true,
        }) {
            return Err(anyhow::anyhow!(serde_json::to_string_pretty(&parsed)?));
        }
        Ok(format_results(query, page, &parsed))
    }
}

#[async_trait]
impl AgentTool for GoogleSearchTool {
    fn name(&self) -> &str {
        "google_search"
    }

    fn description(&self) -> &str {
        DESCRIPTION
    }

    fn parameters_schema(&self) -> Value {
        json!({ "type": "object", "additionalProperties": false, "properties": {
            "query": { "type": "string", "description": "Search query - be specific and use quotes for exact matches. For best results, use clear keywords and avoid very long queries." },
            "num_results": { "type": "integer", "description": "Number of results to return (default: 5, max: 10). Increase for broader coverage, decrease for faster response.", "default": 5, "minimum": 1, "maximum": 10 },
            "site": { "type": "string", "description": "Limit search results to a specific website domain (e.g., 'wikipedia.org' or 'nytimes.com')." },
            "language": { "type": "string", "description": "Filter results by language using ISO 639-1 codes (e.g., 'en' for English, 'es' for Spanish, 'fr' for French)." },
            "dateRestrict": { "type": "string", "description": "Filter results by date using Google's date restriction format: 'd[number]' for past days, 'w[number]' for past weeks, 'm[number]' for past months, or 'y[number]' for past years. Example: 'm6' for results from the past 6 months." },
            "exactTerms": { "type": "string", "description": "Search for results that contain this exact phrase. This is equivalent to putting the terms in quotes in the search query." },
            "resultType": { "type": "string", "description": "Specify the type of results to return. Options include 'image' (or 'images'), 'news', and 'video' (or 'videos'). Default is general web results." },
            "page": { "type": "integer", "description": "Page number for paginated results (starts at 1). Use in combination with resultsPerPage to navigate through large result sets.", "default": 1, "minimum": 1 },
            "resultsPerPage": { "type": "integer", "description": "Number of results to show per page (default: 5, max: 10). Controls how many results are returned for each page.", "default": 5, "minimum": 1, "maximum": 10 },
            "sort": { "type": "string", "description": "Sorting method for search results. Options: 'relevance' (default) or 'date' (most recent first)." }
        }, "required": ["query"] })
    }

    async fn execute(&self, params: Value) -> anyhow::Result<Value> {
        let result = self.run(params, None).await;
        crate::metrics::record_execution(self.name(), result.is_ok());
        result
            .map(Value::String)
            .map_err(|error| anyhow::anyhow!("google_search error: {error}"))
    }

    async fn execute_with_context(
        &self,
        params: Value,
        context: &ToolExecutionContext,
    ) -> anyhow::Result<Value> {
        let result = self.run(params, Some(context)).await;
        crate::metrics::record_execution(self.name(), result.is_ok());
        result
            .map(Value::String)
            .map_err(|error| anyhow::anyhow!("google_search error: {error}"))
    }
}

#[cfg(test)]
mod tests {
    use {super::*, secrecy::Secret};

    #[test]
    fn integer_valued_floats_match_schema_validation() {
        let value: Number =
            serde_json::from_str("10.0").unwrap_or_else(|error| panic!("invalid number: {error}"));
        assert_eq!(normalized_int(Some(value), 5, 10), 10);
    }

    #[tokio::test]
    async fn missing_credentials_fail_before_http() {
        let tool = GoogleSearchTool::new(Arc::new(GoogleConfig::default()));
        let error = tool
            .execute(json!({ "query": "rust" }))
            .await
            .err()
            .map(|error| error.to_string())
            .unwrap_or_default();
        assert!(error.contains("tools.google.token"));
        let tool = GoogleSearchTool::new(Arc::new(GoogleConfig {
            token: Some(Secret::new("key".into())),
            ..GoogleConfig::default()
        }));
        let error = tool
            .execute(json!({ "query": "rust" }))
            .await
            .err()
            .map(|error| error.to_string())
            .unwrap_or_default();
        assert!(error.contains("tools.google.engine_id"));
    }

    #[tokio::test]
    async fn formats_pagination_and_snippets() {
        let mut server = mockito::Server::new_async().await;
        let mock = server.mock("GET", "/customsearch/v1")
            .match_query(mockito::Matcher::AllOf(vec![
                mockito::Matcher::UrlEncoded("key".into(), "private-key".into()),
                mockito::Matcher::UrlEncoded("cx".into(), "engine".into()),
                mockito::Matcher::UrlEncoded("q".into(), "rust".into()),
            ]))
            .with_status(200).with_body(r#"{"items":[{"title":"Docs","link":"https://example.org","snippet":"Guide"}],"searchInformation":{"totalResults":"42"},"queries":{"nextPage":[{"startIndex":6}]}}"#)
            .create_async().await;
        let tool = GoogleSearchTool::for_test(
            Arc::new(GoogleConfig {
                token: Some(Secret::new("private-key".into())),
                engine_id: Some(Secret::new("engine".into())),
                request_timeout_secs: 10,
            }),
            format!("{}/customsearch/v1", server.url()),
        );
        let result = tool
            .execute(json!({ "query": "rust" }))
            .await
            .unwrap_or_else(|error| panic!("search failed: {error}"));
        assert_eq!(
            result.as_str(),
            Some(
                "Search results for \"rust\":\n\nShowing page 1 of approximately 42 results\n\n1. Docs\n   URL: https://example.org\n   Guide\n\nNavigation: Use 'page: 2' for more results."
            )
        );
        mock.assert_async().await;
    }

    #[tokio::test(start_paused = true)]
    async fn rate_limit_retries_once_and_records_progress() {
        let mut server = mockito::Server::new_async().await;
        let limited = server
            .mock("GET", "/customsearch/v1")
            .match_query(mockito::Matcher::Any)
            .with_status(429)
            .with_header("retry-after", "0")
            .expect(1)
            .create_async()
            .await;
        let success = server
            .mock("GET", "/customsearch/v1")
            .match_query(mockito::Matcher::Any)
            .with_status(200)
            .with_body("{}")
            .expect(1)
            .create_async()
            .await;
        let tool = GoogleSearchTool::for_test(
            Arc::new(GoogleConfig {
                token: Some(Secret::new("private-key".into())),
                engine_id: Some(Secret::new("engine".into())),
                request_timeout_secs: 10,
            }),
            format!("{}/customsearch/v1", server.url()),
        );
        let context = ToolExecutionContext::for_session(chelix_sessions::SessionKey::new(
            "session:search-test",
        ));
        let started = tokio::time::Instant::now();
        let result = tool
            .execute_with_context(json!({ "query": "rust" }), &context)
            .await
            .unwrap_or_else(|error| panic!("retry failed: {error}"));
        assert_eq!(
            result.as_str(),
            Some(
                "No results found. Try:\n- Using different keywords\n- Removing quotes from non-exact phrases\n- Using more general terms"
            )
        );
        assert_eq!(context.retry_count(), 1);
        assert_eq!(started.elapsed(), Duration::from_secs(5));
        limited.assert_async().await;
        success.assert_async().await;
    }

    #[tokio::test]
    async fn network_error_never_exposes_api_key() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap_or_else(|error| panic!("bind failed: {error}"));
        let address = listener
            .local_addr()
            .unwrap_or_else(|error| panic!("address failed: {error}"));
        drop(listener);
        let tool = GoogleSearchTool::for_test(
            Arc::new(GoogleConfig {
                token: Some(Secret::new("google-secret-key-7f3a".into())),
                engine_id: Some(Secret::new("engine-secret-id-91c2".into())),
                request_timeout_secs: 1,
            }),
            format!("http://{address}/customsearch/v1"),
        );
        let error = tool
            .execute(json!({ "query": "rust" }))
            .await
            .err()
            .map(|error| error.to_string())
            .unwrap_or_default();
        assert!(
            error.starts_with("google_search error: error sending request"),
            "{error}"
        );
        assert!(!error.contains("\"code\""));
        assert!(!error.contains('{'));
        assert!(!error.contains("google-secret-key-7f3a"));
        assert!(!error.contains("engine-secret-id-91c2"));
    }
}
