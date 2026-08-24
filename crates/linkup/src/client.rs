//! Authenticated Linkup HTTP client shared by every `linkup_search` call.

use {
    reqwest::{StatusCode, header::HeaderValue},
    secrecy::{ExposeSecret, Secret},
    std::time::Duration,
};

use crate::{
    api::LinkupSearchRequest,
    error::{Error, Result},
    rate_limit::{LINKUP_MAX_RATE_LIMIT_COOLDOWN_MS, RateLimitCoordinator},
    usage_limit::UsageLimitCoordinator,
};

/// Linkup API root used by the search tool.
pub const LINKUP_API_BASE_URL: &str = "https://api.linkup.so";
const LINKUP_SEARCH_PATH: &str = "/v1/search";
const LINKUP_USER_AGENT: &str = "chelix";
const LINKUP_RATE_LIMIT_COOLDOWN_BUFFER_MS: u64 = 5_000;

/// One fully read Linkup response.
#[derive(Debug, Clone)]
pub struct LinkupResponse {
    status: StatusCode,
    retry_after: Option<String>,
    body: String,
}

impl LinkupResponse {
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

    #[must_use]
    pub fn failure_message(&self) -> String {
        if self.body.is_empty() {
            self.status.to_string()
        } else {
            self.body.clone()
        }
    }

    #[must_use]
    pub fn is_rate_limited(&self) -> bool {
        self.status == StatusCode::TOO_MANY_REQUESTS
    }

    #[must_use]
    pub fn rate_limit_cooldown_ms(&self) -> Option<u64> {
        let seconds = self.retry_after.as_deref()?.parse::<u64>().ok()?;
        let cooldown_ms = seconds
            .checked_mul(1_000)
            .and_then(|milliseconds| milliseconds.checked_add(LINKUP_RATE_LIMIT_COOLDOWN_BUFFER_MS))
            .unwrap_or(LINKUP_MAX_RATE_LIMIT_COOLDOWN_MS);
        Some(cooldown_ms.min(LINKUP_MAX_RATE_LIMIT_COOLDOWN_MS))
    }
}

/// HTTP client bound to one optional Linkup API token.
pub struct LinkupClient {
    http: reqwest::Client,
    base_url: String,
    token: Option<Secret<String>>,
    request_timeout: Option<Duration>,
    usage_limit: UsageLimitCoordinator,
    rate_limit: RateLimitCoordinator,
}

impl LinkupClient {
    /// Build a client for the public Linkup API.
    #[must_use]
    pub fn new(token: Option<Secret<String>>, request_timeout_secs: u64) -> Self {
        Self {
            http: chelix_common::http_client::build_default_http_client(),
            base_url: LINKUP_API_BASE_URL.to_string(),
            token,
            request_timeout: Some(Duration::from_secs(request_timeout_secs)),
            usage_limit: UsageLimitCoordinator::default(),
            rate_limit: RateLimitCoordinator::default(),
        }
    }

    #[cfg(test)]
    pub(crate) fn for_test(base_url: String, token: Option<Secret<String>>) -> Self {
        Self {
            http: chelix_common::http_client::build_default_http_client(),
            base_url,
            token,
            request_timeout: None,
            usage_limit: UsageLimitCoordinator::default(),
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
            usage_limit: UsageLimitCoordinator::default(),
            rate_limit: RateLimitCoordinator::default(),
        }
    }

    fn authorization_header(&self) -> Result<HeaderValue> {
        let token = self
            .token
            .as_ref()
            .map(ExposeSecret::expose_secret)
            .map(String::as_str)
            .map(str::trim)
            .filter(|token| !token.is_empty())
            .ok_or_else(|| {
                Error::authorization("Linkup API token is not configured in tools.linkup.token")
            })?;
        let mut authorization = HeaderValue::try_from(format!("Bearer {token}"))
            .map_err(|error| Error::message(format!("invalid Linkup token: {error}")))?;
        authorization.set_sensitive(true);
        Ok(authorization)
    }

    fn map_request_error(&self, error: reqwest::Error) -> Error {
        if error.is_timeout()
            && let Some(request_timeout) = self.request_timeout
        {
            #[cfg(feature = "tracing")]
            tracing::warn!(
                timeout = ?request_timeout,
                "Linkup HTTP request timed out"
            );
            return Error::message(format!(
                "Linkup request timed out after {request_timeout:?}"
            ));
        }
        error.into()
    }

    async fn post_once(
        &self,
        body: &LinkupSearchRequest<'_>,
        authorization: &HeaderValue,
    ) -> Result<LinkupResponse> {
        let rate_limit_permit = self.rate_limit.acquire().await;
        self.usage_limit.acquire().await;
        let url = format!(
            "{}{LINKUP_SEARCH_PATH}",
            self.base_url.trim_end_matches('/')
        );
        let mut request = self
            .http
            .post(url)
            .header(reqwest::header::AUTHORIZATION, authorization.clone())
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .header(reqwest::header::USER_AGENT, LINKUP_USER_AGENT)
            .json(body);
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
        let response = LinkupResponse {
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

        if response.status == StatusCode::UNAUTHORIZED {
            return Err(Error::authorization(response.failure_message()));
        }
        Ok(response)
    }

    /// Execute `POST /v1/search` and retry once after a usable shared cooldown.
    #[cfg_attr(feature = "tracing", tracing::instrument(skip(self, body)))]
    pub(crate) async fn search(&self, body: &LinkupSearchRequest<'_>) -> Result<LinkupResponse> {
        let authorization = self.authorization_header()?;
        let response = self.post_once(body, &authorization).await?;
        let Some(cooldown_ms) = response
            .is_rate_limited()
            .then(|| response.rate_limit_cooldown_ms())
            .flatten()
        else {
            return Ok(response);
        };

        #[cfg(feature = "tracing")]
        tracing::warn!(
            tool = "linkup_search",
            cooldown_ms,
            "Linkup search hit a rate limit; retrying once through the shared cooldown gate"
        );
        self.post_once(body, &authorization).await
    }
}

#[cfg(test)]
mod tests {
    use {mockito::Matcher, serde_json::json};

    use super::*;

    fn request<'a>(domains: Option<&'a [String]>) -> LinkupSearchRequest<'a> {
        LinkupSearchRequest {
            q: "rust release",
            depth: "standard",
            output_type: "searchResults",
            include_images: false,
            max_results: 5,
            include_domains: domains,
            from_date: Some("2026-01-01"),
            to_date: None,
        }
    }

    #[tokio::test]
    async fn sends_the_exact_search_contract_and_configured_token() {
        let mut server = mockito::Server::new_async().await;
        let domains = vec!["rust-lang.org".to_string()];
        let call = server
            .mock("POST", "/v1/search")
            .match_header("authorization", "Bearer api-token")
            .match_header("content-type", "application/json")
            .match_header("user-agent", "chelix")
            .match_body(Matcher::Json(json!({
                "q": "rust release",
                "depth": "standard",
                "outputType": "searchResults",
                "includeImages": false,
                "maxResults": 5,
                "includeDomains": ["rust-lang.org"],
                "fromDate": "2026-01-01"
            })))
            .with_status(200)
            .with_body(r#"{"results":[]}"#)
            .expect(1)
            .create_async()
            .await;
        let client =
            LinkupClient::for_test(server.url(), Some(Secret::new("api-token".to_string())));

        let response = client
            .search(&request(Some(&domains)))
            .await
            .unwrap_or_else(|error| panic!("search failed: {error}"));

        assert_eq!(response.body(), r#"{"results":[]}"#);
        call.assert_async().await;
    }

    #[tokio::test]
    async fn missing_token_fails_before_sending_a_request() {
        let client = LinkupClient::for_test("http://127.0.0.1:1".to_string(), None);

        let error = client
            .search(&request(None))
            .await
            .err()
            .unwrap_or_else(|| panic!("missing token should fail"));

        assert!(matches!(error, Error::Authorization { .. }));
        assert_eq!(
            error.to_string(),
            "Linkup API token is not configured in tools.linkup.token"
        );
    }

    #[tokio::test]
    async fn unauthorized_response_is_explicit() {
        let mut server = mockito::Server::new_async().await;
        let call = server
            .mock("POST", "/v1/search")
            .with_status(401)
            .with_body("invalid token")
            .expect(1)
            .create_async()
            .await;
        let client =
            LinkupClient::for_test(server.url(), Some(Secret::new("api-token".to_string())));

        let error = client
            .search(&request(None))
            .await
            .err()
            .unwrap_or_else(|| panic!("authorization should fail"));

        assert!(matches!(error, Error::Authorization { .. }));
        assert_eq!(error.to_string(), "invalid token");
        call.assert_async().await;
    }

    #[tokio::test(start_paused = true)]
    async fn numeric_retry_after_retries_once_through_the_shared_cooldown() {
        let mut server = mockito::Server::new_async().await;
        let limited = server
            .mock("POST", "/v1/search")
            .with_status(429)
            .with_header("retry-after", "0")
            .with_body("slow down")
            .expect(1)
            .create_async()
            .await;
        let recovered = server
            .mock("POST", "/v1/search")
            .with_status(200)
            .with_body(r#"{"results":[]}"#)
            .expect(1)
            .create_async()
            .await;
        let client =
            LinkupClient::for_test(server.url(), Some(Secret::new("api-token".to_string())));
        let started = tokio::time::Instant::now();

        let response = client
            .search(&request(None))
            .await
            .unwrap_or_else(|error| panic!("search failed: {error}"));

        assert_eq!(response.body(), r#"{"results":[]}"#);
        assert_eq!(
            tokio::time::Instant::now() - started,
            Duration::from_millis(LINKUP_RATE_LIMIT_COOLDOWN_BUFFER_MS)
        );
        limited.assert_async().await;
        recovered.assert_async().await;
    }

    #[tokio::test]
    async fn rate_limit_without_retry_after_is_not_retried() {
        let mut server = mockito::Server::new_async().await;
        let limited = server
            .mock("POST", "/v1/search")
            .with_status(429)
            .with_body("slow down")
            .expect(1)
            .create_async()
            .await;
        let client =
            LinkupClient::for_test(server.url(), Some(Secret::new("api-token".to_string())));

        let response = client
            .search(&request(None))
            .await
            .unwrap_or_else(|error| panic!("search failed: {error}"));

        assert!(response.is_rate_limited());
        assert_eq!(response.rate_limit_cooldown_ms(), None);
        limited.assert_async().await;
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
        let client = LinkupClient::for_test_with_timeout(
            format!("http://{address}"),
            Some(Secret::new("api-token".to_string())),
            Duration::from_millis(50),
        );
        client.rate_limit.acquire().await.complete(true, Some(0));

        let error = client
            .search(&request(None))
            .await
            .err()
            .unwrap_or_else(|| panic!("expected a request timeout"));

        assert_eq!(error.to_string(), "Linkup request timed out after 50ms");
        let permit = tokio::time::timeout(Duration::from_millis(50), client.rate_limit.acquire())
            .await
            .unwrap_or_else(|_| panic!("shared gate remained blocked after probe timeout"));
        permit.complete(false, None);
        server.abort();
        let _ = server.await;
    }
}
