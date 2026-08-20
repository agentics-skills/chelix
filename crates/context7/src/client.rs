//! Authenticated Context7 HTTP client shared by every `context7_*` tool.
//!
//! The optional token comes from `tools.context7.token`. A request sent with a
//! configured token that Context7 answers with `401` or `403` is an explicit
//! authorization error; the client never prompts for another token.

use {
    reqwest::{StatusCode, header::HeaderValue},
    secrecy::{ExposeSecret, Secret},
    std::time::Duration,
};

use crate::{
    error::{Error, Result},
    rate_limit::{CONTEXT7_MAX_RATE_LIMIT_COOLDOWN_MS, RateLimitCoordinator},
};

/// Context7 API root used by every tool.
pub const CONTEXT7_API_BASE_URL: &str = "https://context7.com/api";

/// Source identifier sent with every request.
const CONTEXT7_SOURCE: &str = "chelix";

/// Extra delay added on top of the server-provided cooldown.
const CONTEXT7_RATE_LIMIT_COOLDOWN_BUFFER_MS: u64 = 5_000;

/// Per-request behaviour toggles.
#[derive(Debug, Clone, Copy, Default)]
pub struct RequestOptions {
    /// Hand a rate-limited response back to the caller so it can retry once
    /// through the shared cooldown gate.
    pub return_rate_limit_response: bool,
}

/// One fully read Context7 response.
#[derive(Debug, Clone)]
pub struct Context7Response {
    status: StatusCode,
    retry_after: Option<String>,
    body: String,
}

impl Context7Response {
    #[must_use]
    pub const fn status(&self) -> StatusCode {
        self.status
    }

    #[must_use]
    pub fn is_success(&self) -> bool {
        self.status.is_success()
    }

    #[must_use]
    pub fn body(&self) -> &str {
        &self.body
    }

    /// Error text used when the response is not successful: the response body,
    /// or the status line when the body is empty.
    #[must_use]
    pub fn failure_message(&self) -> String {
        if self.body.is_empty() {
            self.status.to_string()
        } else {
            self.body.clone()
        }
    }

    /// Deserialize the body of a successful response.
    pub fn json<T: serde::de::DeserializeOwned>(&self) -> Result<T> {
        Ok(serde_json::from_str(&self.body)?)
    }

    /// Whether the response reports a Context7 rate limit.
    #[must_use]
    pub fn is_rate_limited(&self) -> bool {
        self.status == StatusCode::TOO_MANY_REQUESTS
    }

    /// Cooldown derived from the documented `Retry-After` seconds.
    #[must_use]
    pub fn rate_limit_cooldown_ms(&self) -> Option<u64> {
        let seconds = self.retry_after.as_deref()?.parse::<u64>().ok()?;
        let cooldown_ms = seconds
            .checked_mul(1_000)
            .and_then(|milliseconds| {
                milliseconds.checked_add(CONTEXT7_RATE_LIMIT_COOLDOWN_BUFFER_MS)
            })
            .unwrap_or(CONTEXT7_MAX_RATE_LIMIT_COOLDOWN_MS);
        Some(cooldown_ms.min(CONTEXT7_MAX_RATE_LIMIT_COOLDOWN_MS))
    }
}

/// HTTP client bound to one optional Context7 API token.
pub struct Context7Client {
    http: reqwest::Client,
    base_url: String,
    token: Option<Secret<String>>,
    request_timeout: Option<Duration>,
    rate_limit: RateLimitCoordinator,
}

impl Context7Client {
    /// Build a client for the public Context7 API.
    #[must_use]
    pub fn new(token: Option<Secret<String>>, request_timeout_secs: u64) -> Self {
        Self {
            http: chelix_common::http_client::build_default_http_client(),
            base_url: CONTEXT7_API_BASE_URL.to_string(),
            token,
            request_timeout: Some(Duration::from_secs(request_timeout_secs)),
            rate_limit: RateLimitCoordinator::default(),
        }
    }

    /// Build a client pointed at a test double without a wall-clock deadline.
    #[cfg(test)]
    pub(crate) fn for_test(base_url: String, token: Option<Secret<String>>) -> Self {
        Self {
            http: chelix_common::http_client::build_default_http_client(),
            base_url,
            token,
            request_timeout: None,
            rate_limit: RateLimitCoordinator::default(),
        }
    }

    #[cfg(test)]
    fn for_test_with_timeout(
        base_url: String,
        token: Option<Secret<String>>,
        request_timeout: Duration,
    ) -> Self {
        Self {
            http: chelix_common::http_client::build_default_http_client(),
            base_url,
            token,
            request_timeout: Some(request_timeout),
            rate_limit: RateLimitCoordinator::default(),
        }
    }

    /// Absolute API root, without a trailing slash.
    #[must_use]
    pub fn base_url(&self) -> &str {
        self.base_url.trim_end_matches('/')
    }

    fn map_request_error(&self, error: reqwest::Error) -> Error {
        if error.is_timeout()
            && let Some(request_timeout) = self.request_timeout
        {
            #[cfg(feature = "tracing")]
            tracing::warn!(
                timeout = ?request_timeout,
                "Context7 HTTP request timed out"
            );
            return Error::message(format!(
                "Context7 request timed out after {request_timeout:?}"
            ));
        }
        error.into()
    }

    /// Perform one authenticated `GET` and read the complete response.
    pub async fn get(&self, url: &url::Url, options: RequestOptions) -> Result<Context7Response> {
        let rate_limit_permit = self.rate_limit.acquire().await;
        let mut request = self
            .http
            .get(url.as_str())
            .header("X-Context7-Source", CONTEXT7_SOURCE);
        if let Some(token) = &self.token {
            let mut authorization =
                HeaderValue::try_from(format!("Bearer {}", token.expose_secret()))
                    .map_err(|error| Error::message(format!("invalid Context7 token: {error}")))?;
            authorization.set_sensitive(true);
            request = request.header(reqwest::header::AUTHORIZATION, authorization);
        }

        if let Some(request_timeout) = self.request_timeout {
            request = request.timeout(request_timeout);
        }
        let raw = request
            .send()
            .await
            .map_err(|error| self.map_request_error(error))?;
        let status = raw.status();
        let retry_after = raw
            .headers()
            .get(reqwest::header::RETRY_AFTER)
            .and_then(|value| value.to_str().ok())
            .map(str::to_string);
        let response = Context7Response {
            status,
            retry_after,
            body: raw
                .text()
                .await
                .map_err(|error| self.map_request_error(error))?,
        };
        rate_limit_permit.complete(
            response.is_rate_limited(),
            response.rate_limit_cooldown_ms(),
        );

        if options.return_rate_limit_response && response.is_rate_limited() {
            return Ok(response);
        }

        if matches!(status, StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN) {
            return Err(Error::authorization(response.failure_message()));
        }

        Ok(response)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn probe(
        server: &mockito::ServerGuard,
        options: RequestOptions,
    ) -> Result<Context7Response> {
        let url = url::Url::parse(&format!("{}/probe", server.url()))
            .unwrap_or_else(|error| panic!("probe URL is invalid: {error}"));
        Context7Client::for_test(server.url(), None)
            .get(&url, options)
            .await
    }

    #[tokio::test]
    async fn configured_token_and_source_are_sent() {
        let mut server = mockito::Server::new_async().await;
        let call = server
            .mock("GET", "/probe")
            .match_header("authorization", "Bearer api-token")
            .match_header("x-context7-source", "chelix")
            .with_status(200)
            .with_body("ok")
            .expect(1)
            .create_async()
            .await;
        let url = url::Url::parse(&format!("{}/probe", server.url()))
            .unwrap_or_else(|error| panic!("probe URL is invalid: {error}"));
        let client =
            Context7Client::for_test(server.url(), Some(Secret::new("api-token".to_string())));

        let response = client
            .get(&url, RequestOptions::default())
            .await
            .unwrap_or_else(|error| panic!("request failed: {error}"));

        assert_eq!(response.body(), "ok");
        call.assert_async().await;
    }

    #[tokio::test]
    async fn authorization_failure_is_explicit() {
        let mut server = mockito::Server::new_async().await;
        let call = server
            .mock("GET", "/probe")
            .with_status(401)
            .with_body("invalid token")
            .expect(1)
            .create_async()
            .await;

        let error = match probe(&server, RequestOptions::default()).await {
            Ok(_) => panic!("expected an authorization error"),
            Err(error) => error,
        };

        assert!(matches!(error, Error::Authorization { .. }));
        assert_eq!(error.to_string(), "invalid token");
        call.assert_async().await;
    }

    #[tokio::test]
    async fn retry_after_header_drives_the_shared_cooldown() {
        let mut server = mockito::Server::new_async().await;
        let _call = server
            .mock("GET", "/probe")
            .with_status(429)
            .with_header("retry-after", "7")
            .with_body("slow down")
            .create_async()
            .await;

        let response = probe(&server, RequestOptions {
            return_rate_limit_response: true,
        })
        .await
        .unwrap_or_else(|error| panic!("probe request failed: {error}"));

        assert!(response.is_rate_limited());
        assert_eq!(response.rate_limit_cooldown_ms(), Some(12_000));
    }

    #[tokio::test(start_paused = true)]
    async fn enormous_retry_after_is_capped_before_building_the_shared_deadline() {
        let mut server = mockito::Server::new_async().await;
        let limited = server
            .mock("GET", "/probe")
            .with_status(429)
            .with_header("retry-after", "18446744073709551615")
            .with_body("slow down")
            .expect(1)
            .create_async()
            .await;
        let recovered = server
            .mock("GET", "/probe")
            .with_status(200)
            .with_body("ok")
            .expect(1)
            .create_async()
            .await;
        let client = Context7Client::for_test(server.url(), None);
        let url = url::Url::parse(&format!("{}/probe", server.url()))
            .unwrap_or_else(|error| panic!("probe URL is invalid: {error}"));

        let response = client
            .get(&url, RequestOptions {
                return_rate_limit_response: true,
            })
            .await
            .unwrap_or_else(|error| panic!("limited request failed: {error}"));
        assert_eq!(
            response.rate_limit_cooldown_ms(),
            Some(CONTEXT7_MAX_RATE_LIMIT_COOLDOWN_MS)
        );

        let cooldown_started = tokio::time::Instant::now();
        let response = client
            .get(&url, RequestOptions::default())
            .await
            .unwrap_or_else(|error| panic!("probe request failed: {error}"));

        assert_eq!(response.body(), "ok");
        assert_eq!(
            tokio::time::Instant::now().duration_since(cooldown_started),
            Duration::from_millis(CONTEXT7_MAX_RATE_LIMIT_COOLDOWN_MS)
        );
        limited.assert_async().await;
        recovered.assert_async().await;
    }

    #[tokio::test]
    async fn rate_limit_without_retry_after_has_no_cooldown() {
        let mut server = mockito::Server::new_async().await;
        let _call = server
            .mock("GET", "/probe")
            .with_status(429)
            .with_body("slow down")
            .create_async()
            .await;

        let response = probe(&server, RequestOptions {
            return_rate_limit_response: true,
        })
        .await
        .unwrap_or_else(|error| panic!("probe request failed: {error}"));

        assert!(response.is_rate_limited());
        assert_eq!(response.rate_limit_cooldown_ms(), None);
    }

    #[tokio::test]
    async fn timed_out_probe_reopens_the_shared_gate() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .unwrap_or_else(|error| panic!("test listener failed to bind: {error}"));
        let address = listener
            .local_addr()
            .unwrap_or_else(|error| panic!("test listener has no local address: {error}"));
        let server = tokio::spawn(async move {
            let (_socket, _) = listener
                .accept()
                .await
                .unwrap_or_else(|error| panic!("test listener failed to accept: {error}"));
            tokio::time::sleep(Duration::from_secs(5)).await;
        });
        let base_url = format!("http://{address}");
        let client = Context7Client::for_test_with_timeout(
            base_url.clone(),
            None,
            Duration::from_millis(50),
        );
        client.rate_limit.acquire().await.complete(true, Some(0));
        let url = url::Url::parse(&format!("{base_url}/probe"))
            .unwrap_or_else(|error| panic!("probe URL is invalid: {error}"));

        let error = match client.get(&url, RequestOptions::default()).await {
            Ok(_) => panic!("expected a request timeout"),
            Err(error) => error,
        };

        assert_eq!(error.to_string(), "Context7 request timed out after 50ms");
        let permit = tokio::time::timeout(Duration::from_millis(50), client.rate_limit.acquire())
            .await
            .unwrap_or_else(|_| panic!("shared gate remained blocked after probe timeout"));
        permit.complete(false, None);
        server.abort();
        let _ = server.await;
    }
}
