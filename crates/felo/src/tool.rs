//! Felo request fingerprint and incremental SSE answer extraction.
use {
    crate::rate_limit::RateLimitCoordinator,
    async_trait::async_trait,
    chelix_agents::{tool_context::ToolExecutionContext, tool_registry::AgentTool},
    chelix_config::schema::FeloConfig,
    serde::Deserialize,
    serde_json::{Value, json},
    std::{sync::Arc, time::Duration},
    uuid::Uuid,
};

const DESCRIPTION: &str = "Search the web for up-to-date technical information like latest releases, security advisories, migration guides, benchmarks, and community insights.";
const BASE_URL: &str = "https://api.felo.ai/search/threads";
const USER_AGENTS: [&str; 5] = [
    "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36",
    "Mozilla/5.0 (Windows NT 10.0; Win64; x64) Edge/120.0.0.0",
    "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/17.2.1 Safari/605.1.15",
    "Mozilla/5.0 (Windows NT 10.0; Win64; x64; rv:122.0) Gecko/20100101 Firefox/122.0",
    "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36",
];
const COOKIE: &str = "_clck=1gifk45%7C2%7Cfoa%7C0%7C1686; _clsk=1g5lv07%7C1723558310439%7C1%7C1%7Cu.clarity.ms%2Fcollect; _ga=GA1.1.877307181.1723558313; _ga_8SZPRV97HV=GS1.1.1723558313.1.1.1723558341.0.0.0; _ga_Q9Q1E734CC=GS1.1.1723558313.1.1.1723558341.0.0.0";

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Input {
    query: Option<String>,
}

fn answer_from_line(line: &[u8], answer: &mut String) {
    let line = String::from_utf8_lossy(line);
    let Some(data) = line.strip_prefix("data:") else {
        return;
    };
    let data = data.trim();
    if data.is_empty() || data == "[DONE]" {
        return;
    }
    let Ok(parsed) = serde_json::from_str::<Value>(data) else {
        return;
    };
    if parsed.get("type").and_then(Value::as_str) == Some("answer")
        && let Some(text) = parsed.pointer("/data/text").and_then(Value::as_str)
        && text.encode_utf16().count() > answer.encode_utf16().count()
    {
        *answer = text.to_string();
    }
}

pub struct FeloSearchTool {
    config: Arc<FeloConfig>,
    http: reqwest::Client,
    gate: RateLimitCoordinator,
    base_url: String,
}
impl FeloSearchTool {
    pub fn new(config: Arc<FeloConfig>) -> Self {
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
                "Felo request timed out after {:?}: {error}",
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
        tracing::warn!(error = %message, "Felo search transport failure");
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
            tool = "felo_search",
            "Felo rate limited request; retrying once after shared cooldown"
        );
        if let Some(context) = context {
            context.record_retry();
        }
        self.send_once(make_request())
            .await
            .map(|(response, _)| response)
    }

    #[cfg(test)]
    fn for_test(config: Arc<FeloConfig>, base_url: String) -> Self {
        Self {
            base_url,
            ..Self::new(config)
        }
    }

    #[cfg_attr(feature = "tracing", tracing::instrument(skip(self, params, context)))]
    async fn run(
        &self,
        mut params: Value,
        context: Option<&ToolExecutionContext>,
    ) -> anyhow::Result<String> {
        let map = params
            .as_object_mut()
            .ok_or_else(|| anyhow::anyhow!("felo_search parameters must be an object"))?;
        map.retain(|name, _| !name.starts_with('_'));
        let input: Input = serde_json::from_value(params)?;
        let query = input
            .query
            .as_deref()
            .map(str::trim)
            .filter(|text| !text.is_empty())
            .ok_or_else(|| anyhow::anyhow!("Missing required parameter: query"))?;
        let uuid = Uuid::new_v4();
        let index = usize::from(uuid.as_bytes()[0]) % USER_AGENTS.len();
        let payload = json!({ "query": query, "search_uuid": uuid.to_string(), "lang": "", "agent_lang": "en",
            "search_options": { "langcode": "en-US" }, "search_video": true, "contexts_from": "google" });
        let mut response = self.send_with_retry(context, || self.http.post(&self.base_url)
            .header("accept", "*/*")
            .header("accept-encoding", "gzip, deflate, br")
            .header("accept-language", "en-US,en;q=0.9")
            .header("content-type", "application/json")
            .header("cookie", COOKIE)
            .header("dnt", "1")
            .header("origin", "https://felo.ai")
            .header("referer", "https://felo.ai/")
            .header("sec-ch-ua", "\"Not)A;Brand\";v=\"99\", \"Microsoft Edge\";v=\"127\", \"Chromium\";v=\"127\"")
            .header("sec-ch-ua-mobile", "?0")
            .header("sec-ch-ua-platform", "\"Windows\"")
            .header("sec-fetch-dest", "empty")
            .header("sec-fetch-mode", "cors")
            .header("sec-fetch-site", "same-site")
            .header(reqwest::header::USER_AGENT, USER_AGENTS[index])
            .json(&payload)
            .timeout(Duration::from_secs(self.config.request_timeout_secs))).await?;
        if !response.status().is_success() {
            let status = response.status();
            let text = response
                .text()
                .await
                .map_err(|error| self.request_error(error))?;
            anyhow::bail!("Felo API request failed: {} - {text}", status.as_u16());
        }
        let mut buffered = Vec::new();
        let mut answer = String::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|error| self.request_error(error))?
        {
            buffered.extend_from_slice(&chunk);
            while let Some(index) = buffered.iter().position(|byte| *byte == b'\n') {
                let line: Vec<u8> = buffered.drain(..=index).collect();
                let line = line.strip_suffix(b"\n").unwrap_or(&line);
                let line = line.strip_suffix(b"\r").unwrap_or(line);
                answer_from_line(line, &mut answer);
            }
        }
        if !buffered.is_empty() {
            answer_from_line(&buffered, &mut answer);
        }
        if answer.is_empty() {
            Ok("No response received from Felo AI.".to_string())
        } else {
            Ok(answer)
        }
    }
}

#[async_trait]
impl AgentTool for FeloSearchTool {
    fn name(&self) -> &str {
        "felo_search"
    }

    fn description(&self) -> &str {
        DESCRIPTION
    }

    fn parameters_schema(&self) -> Value {
        json!({ "type": "object", "additionalProperties": false, "properties": {
            "query": { "type": "string", "description": "The search query or prompt" }
        }, "required": ["query"] })
    }

    async fn execute(&self, params: Value) -> anyhow::Result<Value> {
        let result = self.run(params, None).await;
        crate::metrics::record_execution(self.name(), result.is_ok());
        result
            .map(Value::String)
            .map_err(|error| anyhow::anyhow!("felo_search error: {error}"))
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
            .map_err(|error| anyhow::anyhow!("felo_search error: {error}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_only_longest_answer_and_handles_utf8() {
        let mut answer = String::new();
        answer_from_line(
            "data: {\"type\":\"answer\",\"data\":{\"text\":\"Привет\"}}".as_bytes(),
            &mut answer,
        );
        answer_from_line(
            b"data: {\"type\":\"answer\",\"data\":{\"text\":\"x\"}}",
            &mut answer,
        );
        assert_eq!(answer, "Привет");
        answer_from_line(
            "data: {\"type\":\"answer\",\"data\":{\"text\":\"Ответ: да\"}}".as_bytes(),
            &mut answer,
        );
        answer_from_line(
            "data: {\"type\":\"answer\",\"data\":{\"text\":\"Answer: yes, ok\"}}".as_bytes(),
            &mut answer,
        );
        assert_eq!(answer, "Answer: yes, ok");
    }

    #[tokio::test]
    async fn reports_http_failure_with_reference_status_format() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("POST", "/search/threads")
            .with_status(403)
            .with_body("Denied")
            .create_async()
            .await;
        let tool = FeloSearchTool::for_test(
            Arc::new(FeloConfig {
                request_timeout_secs: 10,
            }),
            format!("{}/search/threads", server.url()),
        );
        let error = tool
            .execute(json!({ "query": "sample" }))
            .await
            .err()
            .map(|error| error.to_string())
            .unwrap_or_default();
        assert_eq!(
            error,
            "felo_search error: Felo API request failed: 403 - Denied"
        );
        mock.assert_async().await;
    }

    #[tokio::test]
    async fn rate_limit_retries_once_and_records_progress() {
        let mut server = mockito::Server::new_async().await;
        let limited = server
            .mock("POST", "/search/threads")
            .with_status(429)
            .with_header("retry-after", "0")
            .expect(1)
            .create_async()
            .await;
        let success = server
            .mock("POST", "/search/threads")
            .with_status(200)
            .with_body("data: {\"type\":\"answer\",\"data\":{\"text\":\"Ready\"}}\n\n")
            .expect(1)
            .create_async()
            .await;
        let tool = FeloSearchTool::for_test(
            Arc::new(FeloConfig {
                request_timeout_secs: 10,
            }),
            format!("{}/search/threads", server.url()),
        );
        let context = ToolExecutionContext::for_session(chelix_sessions::SessionKey::new(
            "session:search-test",
        ));
        let result = tool
            .execute_with_context(json!({ "query": "sample" }), &context)
            .await
            .unwrap_or_else(|error| panic!("retry failed: {error}"));
        assert_eq!(result.as_str(), Some("Ready"));
        assert_eq!(context.retry_count(), 1);
        limited.assert_async().await;
        success.assert_async().await;
    }

    #[tokio::test]
    async fn returns_longest_streamed_answer() {
        let mut server = mockito::Server::new_async().await;
        let mock = server.mock("POST", "/search/threads")
            .match_header("cookie", "_clck=1gifk45%7C2%7Cfoa%7C0%7C1686; _clsk=1g5lv07%7C1723558310439%7C1%7C1%7Cu.clarity.ms%2Fcollect; _ga=GA1.1.877307181.1723558313; _ga_8SZPRV97HV=GS1.1.1723558313.1.1.1723558341.0.0.0; _ga_Q9Q1E734CC=GS1.1.1723558313.1.1.1723558341.0.0.0")
            .match_header("sec-ch-ua", "\"Not)A;Brand\";v=\"99\", \"Microsoft Edge\";v=\"127\", \"Chromium\";v=\"127\"")
            .match_header("origin", "https://felo.ai")
            .match_header("referer", "https://felo.ai/")
            .match_header("accept-encoding", "gzip, deflate, br")
            .match_header("user-agent", mockito::Matcher::AnyOf(vec![
                mockito::Matcher::Exact("Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36".into()),
                mockito::Matcher::Exact("Mozilla/5.0 (Windows NT 10.0; Win64; x64) Edge/120.0.0.0".into()),
                mockito::Matcher::Exact("Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/17.2.1 Safari/605.1.15".into()),
                mockito::Matcher::Exact("Mozilla/5.0 (Windows NT 10.0; Win64; x64; rv:122.0) Gecko/20100101 Firefox/122.0".into()),
                mockito::Matcher::Exact("Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36".into()),
            ]))
            .match_body(mockito::Matcher::PartialJson(json!({
                "query": "sample", "lang": "", "agent_lang": "en", "search_options": {"langcode": "en-US"},
                "search_video": true, "contexts_from": "google"
            })))
            .with_status(200).with_body("data: {\"type\":\"answer\",\"data\":{\"text\":\"Hi\"}}\n\ndata: {\"type\":\"answer\",\"data\":{\"text\":\"Hi there\"}}\n")
            .create_async().await;
        let tool = FeloSearchTool::for_test(
            Arc::new(FeloConfig {
                request_timeout_secs: 10,
            }),
            format!("{}/search/threads", server.url()),
        );
        let result = tool
            .execute(json!({ "query": "sample" }))
            .await
            .unwrap_or_else(|error| panic!("search failed: {error}"));
        assert_eq!(result, json!("Hi there"));
        mock.assert_async().await;
    }
}
