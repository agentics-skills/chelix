//! Exa HTTP request and Markdown search response.
use {
    crate::rate_limit::RateLimitCoordinator,
    async_trait::async_trait,
    chelix_agents::{tool_context::ToolExecutionContext, tool_registry::AgentTool},
    chelix_config::schema::ExaConfig,
    secrecy::ExposeSecret,
    serde::Deserialize,
    serde_json::{Number, Value, json},
    std::{sync::Arc, time::Duration},
    time::{
        Date, OffsetDateTime,
        format_description::{self, well_known::Rfc3339},
    },
};

const DESCRIPTION: &str = "Search the web via Exa API.";
const BASE_URL: &str = "https://api.exa.ai/search";

#[derive(Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct DateRange {
    from_date: Option<String>,
    to_date: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Input {
    query: Option<String>,
    max_results: Option<Number>,
    published_date_range: Option<DateRange>,
    crawl_date_range: Option<DateRange>,
    user_location: Option<String>,
    include_text: Option<String>,
    exclude_text: Option<String>,
    domain: Option<String>,
}

struct Normalized {
    query: String,
    max_results: i64,
    domain: Option<String>,
    location: Option<String>,
    include: Option<String>,
    exclude: Option<String>,
    published_from: Option<String>,
    published_to: Option<String>,
    crawl_from: Option<String>,
    crawl_to: Option<String>,
}

fn optional_trim(value: Option<String>) -> Option<String> {
    value
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn phrase(value: Option<String>) -> anyhow::Result<Option<String>> {
    let value = optional_trim(value);
    if let Some(value) = &value
        && value.split_whitespace().count() > 5
    {
        anyhow::bail!("Phrase must be up to 5 words: {value}");
    }
    Ok(value)
}

fn numeric_date(value: &str) -> bool {
    let bytes = value.as_bytes();
    bytes.len() == 10
        && bytes[..4].iter().all(u8::is_ascii_digit)
        && bytes[4] == b'-'
        && bytes[5..7].iter().all(u8::is_ascii_digit)
        && bytes[7] == b'-'
        && bytes[8..].iter().all(u8::is_ascii_digit)
}

fn timestamp(value: &str) -> anyhow::Result<OffsetDateTime> {
    let normalized = value.strip_suffix('z').map(|prefix| format!("{prefix}Z"));
    let value = normalized.as_deref().unwrap_or(value);
    if !matches!(value.get(10..11), Some("T" | "t" | " ")) {
        anyhow::bail!("invalid timestamp");
    }
    let day = value
        .get(..10)
        .ok_or_else(|| anyhow::anyhow!("invalid date"))?;
    let clock = value
        .get(11..)
        .ok_or_else(|| anyhow::anyhow!("invalid time"))?;
    if !numeric_date(day) {
        anyhow::bail!("invalid date");
    }
    let offset_start = clock
        .char_indices()
        .find(|(index, character)| *index > 0 && matches!(character, 'Z' | '+' | '-'))
        .map(|(index, _)| index)
        .unwrap_or(clock.len());
    let (time, offset) = clock.split_at(offset_start);
    let (hours, rest) = time
        .split_once(':')
        .ok_or_else(|| anyhow::anyhow!("invalid time"))?;
    let minutes = rest
        .get(..2)
        .ok_or_else(|| anyhow::anyhow!("invalid minutes"))?;
    if hours.len() != 2
        || minutes.len() != 2
        || !hours.bytes().all(|byte| byte.is_ascii_digit())
        || !minutes.bytes().all(|byte| byte.is_ascii_digit())
    {
        anyhow::bail!("invalid time");
    }
    let rollover = hours == "24";
    let zero_time = rest == "00"
        || rest == "00:00"
        || rest.strip_prefix("00:00.").is_some_and(|fraction| {
            !fraction.is_empty() && fraction.bytes().all(|byte| byte == b'0')
        });
    if rollover && !zero_time {
        anyhow::bail!("invalid 24-hour timestamp");
    }
    let normalized_time = if rest.len() == 2 {
        format!(
            "{}:{minutes}:00",
            if rollover {
                "00"
            } else {
                hours
            }
        )
    } else if rest.as_bytes().get(2) == Some(&b':') {
        if rollover {
            format!("00:{rest}")
        } else {
            time.to_string()
        }
    } else {
        anyhow::bail!("invalid time");
    };
    let year: i32 = day[..4].parse()?;
    let month = time::Month::try_from(day[5..7].parse::<u8>()?)?;
    let calendar_day: u8 = day[8..10].parse()?;
    if !(1..=31).contains(&calendar_day) {
        anyhow::bail!("invalid day");
    }
    let first = Date::from_calendar_date(year, month, 1)?;
    let corrected = first
        .checked_add(time::Duration::days(
            i64::from(calendar_day - 1) + i64::from(rollover),
        ))
        .ok_or_else(|| anyhow::anyhow!("date outside supported range"))?;
    let normalized_day = format!(
        "{:04}-{:02}-{:02}",
        corrected.year(),
        corrected.month() as u8,
        corrected.day()
    );
    let complete = format!(
        "{normalized_day}T{normalized_time}{}",
        if offset.is_empty() {
            "Z"
        } else {
            offset
        }
    );
    OffsetDateTime::parse(&complete, &Rfc3339)?
        .checked_to_offset(time::UtcOffset::UTC)
        .ok_or_else(|| anyhow::anyhow!("UTC timestamp is outside the supported range"))
}

fn iso_millis(parsed: OffsetDateTime) -> String {
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:03}Z",
        parsed.year(),
        parsed.month() as u8,
        parsed.day(),
        parsed.hour(),
        parsed.minute(),
        parsed.second(),
        parsed.millisecond()
    )
}

fn date(value: Option<String>) -> anyhow::Result<Option<String>> {
    let Some(value) = optional_trim(value) else {
        return Ok(None);
    };
    if numeric_date(&value) {
        return Ok(Some(format!("{value}T00:00:00.000Z")));
    }
    let expanded = match value.len() {
        4 if value.bytes().all(|byte| byte.is_ascii_digit()) => format!("{value}-01-01"),
        7 if value.as_bytes().get(4) == Some(&b'-') => format!("{value}-01"),
        _ => value.clone(),
    };
    let parsed = if matches!(expanded.get(10..11), Some("T" | "t" | " ")) {
        timestamp(&expanded)
    } else {
        let layout = format_description::parse("[year]-[month]-[day]")?;
        Date::parse(&expanded, &layout)
            .map(|day| day.midnight().assume_utc())
            .map_err(anyhow::Error::from)
    }
    .map_err(|_| {
        anyhow::anyhow!("Invalid date-time: {value}. Expected RFC3339/ISO 8601 or YYYY-MM-DD")
    })?;
    Ok(Some(iso_millis(parsed)))
}

fn domain(value: Option<String>) -> Option<String> {
    let value = optional_trim(value)?;
    let has_scheme = value
        .get(..7)
        .is_some_and(|part| part.eq_ignore_ascii_case("http://"))
        || value
            .get(..8)
            .is_some_and(|part| part.eq_ignore_ascii_case("https://"));
    if has_scheme && let Ok(url) = url::Url::parse(&value) {
        return url.host_str().map(str::to_string);
    }
    if value.contains('/')
        && let Ok(url) = url::Url::parse(&format!("https://{value}"))
    {
        return url.host_str().map(str::to_string);
    }
    let without_scheme = if has_scheme {
        value
            .split_once("://")
            .map(|(_, rest)| rest)
            .unwrap_or(&value)
    } else {
        &value
    };
    Some(
        without_scheme
            .strip_suffix('/')
            .unwrap_or(without_scheme)
            .to_string(),
    )
}

fn bounded_int(value: Option<Number>, default: i64, max: i64) -> i64 {
    let Some(value) = value else {
        return default;
    };
    if let Some(integer) = value.as_i64() {
        return integer.clamp(1, max);
    }
    if let Some(integer) = value.as_u64() {
        return integer.min(max as u64) as i64;
    }
    value
        .as_f64()
        .map(|number| (number.trunc() as i64).clamp(1, max))
        .unwrap_or(default)
}

fn js_truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(value) => *value,
        Value::Number(number) => number.as_f64().is_some_and(|number| number != 0.0),
        Value::String(value) => !value.is_empty(),
        Value::Array(_) | Value::Object(_) => true,
    }
}

fn normalize(input: Input) -> anyhow::Result<Normalized> {
    let query = optional_trim(input.query)
        .ok_or_else(|| anyhow::anyhow!("Missing required parameter: query"))?;
    let location = optional_trim(input.user_location)
        .map(|value| {
            if value.len() == 2 && value.bytes().all(|byte| byte.is_ascii_alphabetic()) {
                Ok(value.to_uppercase())
            } else {
                anyhow::bail!(
                    "Invalid userLocation: {value}. Expected 2-letter ISO country code (e.g., US)"
                )
            }
        })
        .transpose()?;
    let published = input.published_date_range.unwrap_or_default();
    let crawl = input.crawl_date_range.unwrap_or_default();
    Ok(Normalized {
        query,
        max_results: bounded_int(input.max_results, 10, 25),
        domain: domain(input.domain),
        location,
        include: phrase(input.include_text)?,
        exclude: phrase(input.exclude_text)?,
        published_from: date(published.from_date)?,
        published_to: date(published.to_date)?,
        crawl_from: date(crawl.from_date)?,
        crawl_to: date(crawl.to_date)?,
    })
}

fn escape_md(text: &str) -> String {
    let mut result = String::new();
    for character in text.chars() {
        if "\\`*_{}[]()#+-.!|>".contains(character) {
            result.push('\\');
        }
        result.push(character);
    }
    result
}

fn display_date(value: &str) -> std::borrow::Cow<'_, str> {
    let value = value.trim();
    if numeric_date(value) {
        return std::borrow::Cow::Borrowed(value);
    }
    if let Ok(parsed) = timestamp(value) {
        return std::borrow::Cow::Owned(format!(
            "{:04}-{:02}-{:02}",
            parsed.year(),
            parsed.month() as u8,
            parsed.day()
        ));
    }
    std::borrow::Cow::Borrowed(value)
}

fn format_score(score: f64) -> String {
    let scaled = score.abs() * 32.0;
    if scaled.fract() == 0.0 && scaled % 2.0 == 1.0 {
        let numerator = u128::from(scaled as u64) * 625;
        let rounded = numerator.div_ceil(2);
        let sign = if score < 0.0 {
            "-"
        } else {
            ""
        };
        format!("{sign}{}.{:04}", rounded / 10_000, rounded % 10_000)
    } else {
        format!("{score:.4}")
    }
}

fn markdown(input: &Normalized, response: &Value) -> String {
    let mut lines = vec![
        format!("### Exa search results for \"{}\"", escape_md(&input.query)),
        String::new(),
        format!("- Max results: `{}`", input.max_results),
    ];
    for (label, field) in [
        ("Domain", &input.domain),
        ("User location", &input.location),
        ("Include text", &input.include),
        ("Exclude text", &input.exclude),
    ] {
        if let Some(value) = field {
            lines.push(format!("- {label}: `{}`", escape_md(value)));
        }
    }
    for (label, from, to) in [
        ("Published", &input.published_from, &input.published_to),
        ("Crawl", &input.crawl_from, &input.crawl_to),
    ] {
        if from.is_some() || to.is_some() {
            lines.push(format!(
                "- {label} date range: from `{}` to `{}`",
                escape_md(
                    &from
                        .as_deref()
                        .map(display_date)
                        .unwrap_or(std::borrow::Cow::Borrowed("-"))
                ),
                escape_md(
                    &to.as_deref()
                        .map(display_date)
                        .unwrap_or(std::borrow::Cow::Borrowed("-"))
                )
            ));
        }
    }
    if let Some(id) = response
        .get("requestId")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
    {
        lines.push(format!("- Request id: `{}`", escape_md(id)));
    }
    lines.push(String::new());
    let items = response.get("results").and_then(Value::as_array);
    let Some(items) = items.filter(|items| !items.is_empty()) else {
        lines.push("No results found.".to_string());
        return lines.join("\n");
    };
    for (index, item) in items.iter().enumerate() {
        let title = item
            .get("title")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| format!("Result {}", index + 1));
        let link = item
            .get("url")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or("#");
        lines.push(format!("{}. [{}]({link})", index + 1, escape_md(&title)));
        let mut meta = Vec::new();
        if let Some(score) = item
            .get("score")
            .and_then(Value::as_f64)
            .filter(|score| score.is_finite())
        {
            meta.push(format!("score: {}", format_score(score)));
        }
        if let Some(date) = item
            .get("publishedDate")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
        {
            meta.push(format!("published: {}", display_date(date)));
        }
        if let Some(author) = item
            .get("author")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
        {
            meta.push(format!("author: {author}"));
        }
        if !meta.is_empty() {
            lines.push(format!("   {}", escape_md(&meta.join(" · "))));
        }
        lines.push(String::new());
    }
    lines.join("\n")
}

fn map_api_error(status: u16, text: &str) -> Value {
    let parsed: Value = serde_json::from_str(text).unwrap_or(Value::Null);
    let details = parsed.get("error").cloned();
    let message = details
        .as_ref()
        .and_then(|error| error.get("message"))
        .and_then(Value::as_str)
        .filter(|message| !message.is_empty())
        .unwrap_or(text);
    let mut error = json!({ "error": { "code": status, "message": message } });
    if let Some(details) = details {
        error["error"]["details"] = details;
    }
    error
}

pub struct ExaSearchTool {
    config: Arc<ExaConfig>,
    http: reqwest::Client,
    gate: RateLimitCoordinator,
    base_url: String,
}
impl ExaSearchTool {
    pub fn new(config: Arc<ExaConfig>) -> Self {
        Self {
            config,
            http: chelix_common::http_client::build_default_http_client(),
            gate: RateLimitCoordinator::default(),
            base_url: BASE_URL.into(),
        }
    }

    fn request_error(&self, error: reqwest::Error) -> anyhow::Error {
        use std::error::Error as _;
        let error = error.without_url();
        let mut message = if error.is_timeout() {
            format!(
                "Exa request timed out after {:?}: {error}",
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
        tracing::warn!(error = %message, "Exa search transport failure");
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
            tool = "exa_search",
            "Exa rate limited request; retrying once after shared cooldown"
        );
        if let Some(context) = context {
            context.record_retry();
        }
        self.send_once(make_request())
            .await
            .map(|(response, _)| response)
    }

    #[cfg(test)]
    fn for_test(config: Arc<ExaConfig>, base_url: String) -> Self {
        Self {
            base_url,
            ..Self::new(config)
        }
    }

    #[cfg_attr(feature = "tracing", tracing::instrument(skip(self, params, context)))]
    async fn run(
        &self,
        params: Value,
        context: Option<&ToolExecutionContext>,
    ) -> anyhow::Result<String> {
        let mut params = params;
        let map = params
            .as_object_mut()
            .ok_or_else(|| anyhow::anyhow!("exa_search parameters must be an object"))?;
        map.retain(|name, _| !name.starts_with('_'));
        let input = normalize(serde_json::from_value(params)?)?;
        let token = self
            .config
            .token
            .as_ref()
            .map(ExposeSecret::expose_secret)
            .map(String::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| anyhow::anyhow!("Exa API key is not configured in tools.exa.token"))?;
        let mut body =
            json!({ "query": input.query, "type": "auto", "numResults": input.max_results });
        for (key, value) in [
            ("userLocation", &input.location),
            ("startPublishedDate", &input.published_from),
            ("endPublishedDate", &input.published_to),
            ("startCrawlDate", &input.crawl_from),
            ("endCrawlDate", &input.crawl_to),
        ] {
            if let Some(value) = value {
                body[key] = json!(value);
            }
        }
        for (key, value) in [
            ("includeDomains", &input.domain),
            ("includeText", &input.include),
            ("excludeText", &input.exclude),
        ] {
            if let Some(value) = value {
                body[key] = json!([value]);
            }
        }
        let response = self
            .send_with_retry(context, || {
                self.http
                    .post(&self.base_url)
                    .header("x-api-key", token)
                    .header(reqwest::header::USER_AGENT, "chelix")
                    .json(&body)
                    .timeout(Duration::from_secs(self.config.request_timeout_secs))
            })
            .await?;
        let status = response.status();
        let text = response
            .text()
            .await
            .map_err(|error| self.request_error(error))?;
        if !status.is_success() {
            return Err(anyhow::anyhow!(serde_json::to_string_pretty(
                &map_api_error(status.as_u16(), &text)
            )?));
        }
        let parsed: Value = serde_json::from_str(&text).map_err(|_| anyhow::anyhow!("{{\n  \"error\": {{\n    \"code\": 500,\n    \"message\": \"Failed to parse Exa response\"\n  }}\n}}"))?;
        if parsed.get("error").is_some_and(js_truthy) {
            return Err(anyhow::anyhow!(serde_json::to_string_pretty(&parsed)?));
        }
        Ok(markdown(&input, &parsed))
    }
}

#[async_trait]
impl AgentTool for ExaSearchTool {
    fn name(&self) -> &str {
        "exa_search"
    }

    fn description(&self) -> &str {
        DESCRIPTION
    }

    fn parameters_schema(&self) -> Value {
        let range = |description: &str, from: &str, to: &str| {
            json!({ "type": "object", "additionalProperties": false,
            "description": description, "properties": {
                "fromDate": { "type": "string", "description": from },
                "toDate": { "type": "string", "description": to }
            } })
        };
        json!({ "type": "object", "additionalProperties": false, "properties": {
            "query": { "type": "string", "description": "Search query." },
            "maxResults": { "type": "integer", "description": "Number of results to return. Default 10, min 1, max 25.", "default": 10, "minimum": 1, "maximum": 25 },
            "publishedDateRange": range("Publication date range filter (RFC3339/ISO 8601 date-time or YYYY-MM-DD).", "Start published date (inclusive-ish).", "End published date (inclusive-ish)."),
            "crawlDateRange": range("Crawl date range filter (RFC3339/ISO 8601 date-time or YYYY-MM-DD).", "Start crawl date.", "End crawl date."),
            "userLocation": { "type": "string", "description": "Two-letter ISO country code (e.g., US)." },
            "includeText": { "type": "string", "description": "Exact phrase that must appear in webpage text (up to 5 words)." },
            "excludeText": { "type": "string", "description": "Exact phrase that must NOT appear in webpage text (up to 5 words)." },
            "domain": { "type": "string", "description": "Domain filter (e.g., arxiv.org). A URL is also accepted and will be normalized to domain." }
        }, "required": ["query"] })
    }

    async fn execute(&self, params: Value) -> anyhow::Result<Value> {
        let result = self.run(params, None).await;
        crate::metrics::record_execution(self.name(), result.is_ok());
        result
            .map(Value::String)
            .map_err(|error| anyhow::anyhow!("exa_search error: {error}"))
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
            .map_err(|error| anyhow::anyhow!("exa_search error: {error}"))
    }
}

#[cfg(test)]
mod tests {
    use {super::*, secrecy::Secret};

    #[test]
    fn normalizes_dates_and_escapes_markdown() {
        assert_eq!(
            date(Some("2025-01-02".into()))
                .unwrap_or_default()
                .as_deref(),
            Some("2025-01-02T00:00:00.000Z")
        );
        assert_eq!(
            date(Some("2025-01-02T04:05:06.123+02:00".into()))
                .unwrap_or_default()
                .as_deref(),
            Some("2025-01-02T02:05:06.123Z")
        );
        let input = normalize(Input {
            query: Some(" a*b ".into()),
            max_results: None,
            published_date_range: None,
            crawl_date_range: None,
            user_location: None,
            include_text: None,
            exclude_text: None,
            domain: None,
        })
        .unwrap_or_else(|error| panic!("invalid input: {error}"));
        assert_eq!(format_score(0.03125), "0.0313");
        assert_eq!(format_score(0.5), "0.5000");
        assert_eq!(
            markdown(
                &input,
                &json!({ "results": [{ "title": "C++", "url": "https://example.org", "score": 0.5 }] })
            ),
            "### Exa search results for \"a\\*b\"\n\n- Max results: `10`\n\n1. [C\\+\\+](https://example.org)\n   score: 0\\.5000\n"
        );
    }

    #[test]
    fn api_errors_omit_missing_details_and_preserve_raw_text() {
        assert_eq!(
            map_api_error(503, "unavailable"),
            json!({ "error": { "code": 503, "message": "unavailable" } })
        );
        assert_eq!(
            map_api_error(400, "{\"tag\":\"bad\"}"),
            json!({ "error": { "code": 400, "message": "{\"tag\":\"bad\"}" } })
        );
        assert_eq!(
            map_api_error(400, "{\"error\":{\"message\":\"\"}}"),
            json!({ "error": { "code": 400, "message": "{\"error\":{\"message\":\"\"}}", "details": { "message": "" } } })
        );
    }

    #[tokio::test]
    async fn missing_key_fails_before_http() {
        let tool = ExaSearchTool::new(Arc::new(ExaConfig::default()));
        let error = tool
            .execute(json!({ "query": "sample" }))
            .await
            .err()
            .map(|error| error.to_string())
            .unwrap_or_default();
        assert!(error.contains("tools.exa.token"));
    }

    #[test]
    fn date_grammar_domain_and_utc_published_date() {
        for (raw, expected) in [
            ("2025", "2025-01-01T00:00:00.000Z"),
            ("2025-01", "2025-01-01T00:00:00.000Z"),
            ("2025-02-30", "2025-02-30T00:00:00.000Z"),
            ("2025-01-02T10:00", "2025-01-02T10:00:00.000Z"),
            ("2025-01-02T10:00Z", "2025-01-02T10:00:00.000Z"),
            ("2025-01-02T24:00:00.000Z", "2025-01-03T00:00:00.000Z"),
            ("2025-01-02T04:05:06.123456Z", "2025-01-02T04:05:06.123Z"),
        ] {
            assert_eq!(
                date(Some(raw.into())).unwrap_or_default().as_deref(),
                Some(expected)
            );
        }
        assert!(date(Some("abcd-ef-gh".into())).is_err());
        assert_eq!(display_date("2025-01-02T23:00:00-05:00"), "2025-01-03");
        assert_eq!(display_date("2025-02-30"), "2025-02-30");
        assert_eq!(display_date("2025-01-◼"), "2025-01-◼");
        let out_of_range = "9999-12-31T23:00:00-05:00";
        assert!(date(Some(out_of_range.into())).is_err());
        assert_eq!(display_date(out_of_range), out_of_range);
        assert_eq!(display_date("2025-02-30T00:00:00.000Z"), "2025-03-02");
        assert_eq!(
            date(Some("2025-02-30T23:00:00-05:00".into()))
                .unwrap_or_default()
                .as_deref(),
            Some("2025-03-03T04:00:00.000Z")
        );
        assert_eq!(display_date("2025-02-30T24:00:00Z"), "2025-03-03");
        assert_eq!(display_date("2025-01-32T00:00:00Z"), "2025-01-32T00:00:00Z");
        assert_eq!(display_date("  garbage  "), "garbage");
        assert_eq!(
            date(Some("2025-01-02 10:00:00".into()))
                .unwrap_or_default()
                .as_deref(),
            Some("2025-01-02T10:00:00.000Z")
        );
        assert_eq!(
            date(Some("2025-01-02t10:00z".into()))
                .unwrap_or_default()
                .as_deref(),
            Some("2025-01-02T10:00:00.000Z")
        );
        assert_eq!(
            bounded_int(
                Some(
                    serde_json::from_str("10.0")
                        .unwrap_or_else(|error| panic!("invalid number: {error}"))
                ),
                1,
                25
            ),
            10
        );
        assert_eq!(
            domain(Some("HTTPS://Example.com/path".into())).as_deref(),
            Some("example.com")
        );
    }

    #[tokio::test]
    async fn sends_all_filters_and_formats_success() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("POST", "/search")
            .match_header("x-api-key", "test-key")
            .match_header("user-agent", "chelix")
            .match_header("content-type", "application/json")
            .match_body(mockito::Matcher::Json(json!({
                "query": "sample", "type": "auto", "numResults": 4,
                "userLocation": "US", "includeDomains": ["example.org"],
                "startPublishedDate": "2025-01-02T00:00:00.000Z",
                "endPublishedDate": "2025-01-03T00:00:00.000Z",
                "startCrawlDate": "2025-01-04T00:00:00.000Z",
                "endCrawlDate": "2025-01-05T00:00:00.000Z",
                "includeText": ["one two"], "excludeText": ["three"]
            })))
            .with_status(200)
            .with_body(r#"{"requestId":"abc","results":[{"title":"C++","url":"https://example.org/page","score":0.5,"publishedDate":"2025-01-02T23:00:00-05:00","author":"A*B"}]}"#)
            .create_async()
            .await;
        let tool = ExaSearchTool::for_test(
            Arc::new(ExaConfig {
                token: Some(Secret::new("test-key".into())),
                request_timeout_secs: 10,
            }),
            format!("{}/search", server.url()),
        );
        let result = tool
            .execute(json!({
                "query": "sample", "maxResults": 4, "domain": "https://example.org/path",
                "userLocation": "us", "includeText": "one two", "excludeText": "three",
                "publishedDateRange": { "fromDate": "2025-01-02", "toDate": "2025-01-03" },
                "crawlDateRange": { "fromDate": "2025-01-04", "toDate": "2025-01-05" }
            }))
            .await
            .unwrap_or_else(|error| panic!("search failed: {error}"));
        assert_eq!(
            result.as_str(),
            Some(
                "### Exa search results for \"sample\"\n\n- Max results: `4`\n- Domain: `example\\.org`\n- User location: `US`\n- Include text: `one two`\n- Exclude text: `three`\n- Published date range: from `2025\\-01\\-02` to `2025\\-01\\-03`\n- Crawl date range: from `2025\\-01\\-04` to `2025\\-01\\-05`\n- Request id: `abc`\n\n1. [C\\+\\+](https://example.org/page)\n   score: 0\\.5000 · published: 2025\\-01\\-03 · author: A\\*B\n"
            )
        );
        mock.assert_async().await;
    }

    #[tokio::test]
    async fn rate_limit_retries_once() {
        let mut server = mockito::Server::new_async().await;
        let limited = server
            .mock("POST", "/search")
            .with_status(429)
            .with_header("retry-after", "0")
            .expect(1)
            .create_async()
            .await;
        let success = server
            .mock("POST", "/search")
            .with_status(200)
            .with_body(r#"{"results":[]}"#)
            .expect(1)
            .create_async()
            .await;
        let tool = ExaSearchTool::for_test(
            Arc::new(ExaConfig {
                token: Some(Secret::new("test-key".into())),
                request_timeout_secs: 10,
            }),
            format!("{}/search", server.url()),
        );
        let context = ToolExecutionContext::for_session(chelix_sessions::SessionKey::new(
            "session:search-test",
        ));
        let result = tool
            .execute_with_context(json!({ "query": "sample" }), &context)
            .await
            .unwrap_or_else(|error| panic!("retry failed: {error}"));
        assert_eq!(
            result.as_str(),
            Some(
                "### Exa search results for \"sample\"\n\n- Max results: `10`\n\nNo results found."
            )
        );
        assert_eq!(context.retry_count(), 1);
        limited.assert_async().await;
        success.assert_async().await;
    }

    #[tokio::test]
    async fn success_status_with_error_is_not_a_search_result() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("POST", "/search")
            .with_status(200)
            .with_body(r#"{"requestId":"abc","error":{"message":"failed"}}"#)
            .create_async()
            .await;
        let tool = ExaSearchTool::for_test(
            Arc::new(ExaConfig {
                token: Some(Secret::new("test-key".into())),
                request_timeout_secs: 10,
            }),
            format!("{}/search", server.url()),
        );
        let error = tool
            .execute(json!({"query": "sample"}))
            .await
            .err()
            .map(|error| error.to_string())
            .unwrap_or_default();
        assert!(error.contains("\"requestId\": \"abc\""));
        assert!(error.contains("\"message\": \"failed\""));
        mock.assert_async().await;
    }

    #[tokio::test]
    async fn error_keeps_reference_code_message_and_details() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("POST", "/search")
            .with_status(401)
            .with_body(r#"{"error":"Invalid API key","tag":"INVALID_API_KEY"}"#)
            .create_async()
            .await;
        let tool = ExaSearchTool::for_test(
            Arc::new(ExaConfig {
                token: Some(Secret::new("test-key".into())),
                request_timeout_secs: 10,
            }),
            format!("{}/search", server.url()),
        );
        let error = tool
            .execute(json!({ "query": "sample" }))
            .await
            .err()
            .map(|error| error.to_string())
            .unwrap_or_default();
        assert!(error.contains("\"code\": 401"));
        assert!(error.contains("\"details\": \"Invalid API key\""));
        mock.assert_async().await;
    }
}
