//! Authenticated Felo search through the official Chat API.
use {
    crate::rate_limit::RateLimitCoordinator,
    async_trait::async_trait,
    chelix_agents::{tool_context::ToolExecutionContext, tool_registry::AgentTool},
    chelix_config::schema::FeloConfig,
    reqwest::header::{AUTHORIZATION, HeaderValue, RETRY_AFTER},
    secrecy::ExposeSecret,
    serde::Deserialize,
    serde_json::{Value, json},
    std::{sync::Arc, time::Duration},
};

const DESCRIPTION: &str = "Search the web for up-to-date technical information like latest releases, security advisories, migration guides, benchmarks, and community insights.";
const BASE_URL: &str = "https://openapi.felo.ai/v2/chat";

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Input {
    query: Option<String>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum ApiStatus {
    Label(StatusLabel),
    Http(u16),
}

#[derive(Deserialize)]
#[serde(rename_all = "lowercase")]
enum StatusLabel {
    Ok,
    Error,
}

#[derive(Deserialize)]
enum ApiCode {
    #[serde(rename = "OK")]
    Ok,
    #[serde(untagged)]
    Other(String),
}

impl ApiCode {
    fn as_str(&self) -> &str {
        match self {
            Self::Ok => "OK",
            Self::Other(code) => code,
        }
    }
}

#[derive(Deserialize)]
struct ChatResponse {
    status: ApiStatus,
    data: Option<ChatData>,
    code: Option<ApiCode>,
    message: Option<String>,
}

#[derive(Deserialize)]
struct ChatData {
    answer: String,
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

    fn authorization(&self) -> anyhow::Result<HeaderValue> {
        let token = self
            .config
            .token
            .as_ref()
            .map(ExposeSecret::expose_secret)
            .map(String::as_str)
            .map(str::trim)
            .filter(|token| !token.is_empty())
            .ok_or_else(|| anyhow::anyhow!("Felo API key is not configured in tools.felo.token"))?;
        let mut value = HeaderValue::try_from(format!("Bearer {token}"))
            .map_err(|error| anyhow::anyhow!("invalid Felo API key: {error}"))?;
        value.set_sensitive(true);
        Ok(value)
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
            .get(RETRY_AFTER)
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
        let authorization = self.authorization()?;
        let response = self
            .send_with_retry(context, || {
                self.http
                    .post(&self.base_url)
                    .header(AUTHORIZATION, authorization.clone())
                    .json(&json!({ "query": query }))
                    .timeout(Duration::from_secs(self.config.request_timeout_secs))
            })
            .await?;
        let status = response.status();
        let body = response
            .text()
            .await
            .map_err(|error| self.request_error(error))?;
        if !status.is_success() {
            anyhow::bail!("Felo API request failed: {} - {body}", status.as_u16());
        }
        let chat: ChatResponse = serde_json::from_str(&body)
            .map_err(|error| anyhow::anyhow!("invalid Felo API response: {error}"))?;
        let success = matches!(
            (&chat.status, chat.code.as_ref()),
            (ApiStatus::Label(StatusLabel::Ok), None | Some(ApiCode::Ok))
                | (ApiStatus::Http(200), Some(ApiCode::Ok))
        );
        if !success {
            let code = chat
                .code
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("Felo API error response missing code"))?;
            let message = chat
                .message
                .as_deref()
                .ok_or_else(|| anyhow::anyhow!("Felo API error response missing message"))?;
            anyhow::bail!("Felo API request failed: {} - {message}", code.as_str());
        }
        let answer = chat
            .data
            .ok_or_else(|| anyhow::anyhow!("Felo API response missing data"))?
            .answer;
        if answer.is_empty() {
            Ok("No response received from Felo AI.".to_owned())
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
    use {super::*, secrecy::Secret};

    fn config() -> Arc<FeloConfig> {
        Arc::new(FeloConfig {
            token: Some(Secret::new("test-key".to_owned())),
            request_timeout_secs: 10,
        })
    }

    #[tokio::test]
    async fn returns_answer_from_authenticated_chat_request() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("POST", "/v2/chat")
            .match_header("authorization", "Bearer test-key")
            .match_header("content-type", "application/json")
            .match_body(mockito::Matcher::Json(json!({"query":"sample"})))
            .with_status(200)
            .with_body(
                r#"{"status":200,"code":"OK","data":{"answer":"**Привет**","resources":[]}}"#,
            )
            .create_async()
            .await;
        let tool = FeloSearchTool::for_test(config(), format!("{}/v2/chat", server.url()));
        let result = tool
            .execute(json!({"query":" sample "}))
            .await
            .unwrap_or_else(|error| panic!("search failed: {error}"));
        assert_eq!(result, json!("**Привет**"));
        mock.assert_async().await;
    }

    #[tokio::test]
    async fn reports_http_and_api_errors() {
        let mut server = mockito::Server::new_async().await;
        let denied = server
            .mock("POST", "/v2/chat")
            .with_status(401)
            .with_body(r#"{"status":"error","code":"INVALID_API_KEY","message":"Invalid key"}"#)
            .create_async()
            .await;
        let tool = FeloSearchTool::for_test(config(), format!("{}/v2/chat", server.url()));
        let error = tool
            .execute(json!({"query":"sample"}))
            .await
            .err()
            .map(|error| error.to_string())
            .unwrap_or_default();
        assert!(error.contains("401"));
        assert!(error.contains("INVALID_API_KEY"));
        denied.assert_async().await;
        let mut server = mockito::Server::new_async().await;
        let api_error = server
            .mock("POST", "/v2/chat")
            .with_status(200)
            .with_body(r#"{"status":"error","code":"CHAT_FAILED","message":"Internal error"}"#)
            .create_async()
            .await;
        let tool = FeloSearchTool::for_test(config(), format!("{}/v2/chat", server.url()));
        let error = tool
            .execute(json!({"query":"sample"}))
            .await
            .err()
            .map(|error| error.to_string())
            .unwrap_or_default();
        assert!(error.contains("CHAT_FAILED - Internal error"));
        api_error.assert_async().await;

        let mut server = mockito::Server::new_async().await;
        let numeric_error = server
            .mock("POST", "/v2/chat")
            .with_status(200)
            .with_body(r#"{"status":500,"code":"CHAT_FAILED","message":"Provider failure"}"#)
            .create_async()
            .await;
        let tool = FeloSearchTool::for_test(config(), format!("{}/v2/chat", server.url()));
        let error = tool
            .execute(json!({"query":"sample"}))
            .await
            .err()
            .map(|error| error.to_string())
            .unwrap_or_default();
        assert!(error.contains("CHAT_FAILED - Provider failure"));
        numeric_error.assert_async().await;

        let mut server = mockito::Server::new_async().await;
        let malformed = server
            .mock("POST", "/v2/chat")
            .with_status(200)
            .with_body(r#"{"status":true}"#)
            .create_async()
            .await;
        let tool = FeloSearchTool::for_test(config(), format!("{}/v2/chat", server.url()));
        let error = tool
            .execute(json!({"query":"sample"}))
            .await
            .err()
            .map(|error| error.to_string())
            .unwrap_or_default();
        assert!(error.contains("invalid Felo API response"));
        malformed.assert_async().await;
    }

    #[tokio::test]
    async fn rate_limit_retries_once_and_records_progress() {
        let mut server = mockito::Server::new_async().await;
        let limited = server
            .mock("POST", "/v2/chat")
            .with_status(429)
            .with_header("retry-after", "0")
            .expect(1)
            .create_async()
            .await;
        let success = server
            .mock("POST", "/v2/chat")
            .with_status(200)
            .with_body(r#"{"status":"ok","data":{"answer":"Ready"}}"#)
            .expect(1)
            .create_async()
            .await;
        let tool = FeloSearchTool::for_test(config(), format!("{}/v2/chat", server.url()));
        let context = ToolExecutionContext::for_session(chelix_sessions::SessionKey::new(
            "session:search-test",
        ));
        let result = tool
            .execute_with_context(json!({"query":"sample"}), &context)
            .await
            .unwrap_or_else(|error| panic!("retry failed: {error}"));
        assert_eq!(result, json!("Ready"));
        assert_eq!(context.retry_count(), 1);
        limited.assert_async().await;
        success.assert_async().await;
    }

    #[tokio::test]
    async fn missing_token_fails_before_network_request() {
        let mut server = mockito::Server::new_async().await;
        let post = server
            .mock("POST", "/v2/chat")
            .expect(0)
            .create_async()
            .await;
        let tool = FeloSearchTool::for_test(
            Arc::new(FeloConfig::default()),
            format!("{}/v2/chat", server.url()),
        );
        let error = tool
            .execute(json!({"query":"sample"}))
            .await
            .err()
            .map(|error| error.to_string())
            .unwrap_or_default();
        assert!(error.contains("tools.felo.token"));
        post.assert_async().await;
    }

    #[tokio::test]
    async fn empty_answer_preserves_result_format() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("POST", "/v2/chat")
            .with_status(200)
            .with_body(r#"{"status":"ok","data":{"answer":""}}"#)
            .create_async()
            .await;
        let tool = FeloSearchTool::for_test(config(), format!("{}/v2/chat", server.url()));
        let result = tool
            .execute(json!({"query":"sample"}))
            .await
            .unwrap_or_else(|error| panic!("empty answer failed: {error}"));
        assert_eq!(result, json!("No response received from Felo AI."));
        mock.assert_async().await;
    }
}
