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
    config_service: chelix_config_service::ConfigService,
    usage_limit: UsageLimitCoordinator,
    rate_limit: RateLimitCoordinator,
}

impl LinkupClient {
    /// Build a client for the public Linkup API.
    #[must_use]
    pub fn new(config_service: chelix_config_service::ConfigService) -> Self {
        Self {
            http: chelix_common::http_client::build_default_http_client(),
            base_url: LINKUP_API_BASE_URL.to_string(),
            config_service,
            usage_limit: UsageLimitCoordinator::default(),
            rate_limit: RateLimitCoordinator::default(),
        }
    }

    /// Build a client pointed at a test double.
    #[cfg(test)]
    pub(crate) fn for_test(
        base_url: String,
        config_service: chelix_config_service::ConfigService,
    ) -> Self {
        Self {
            base_url,
            ..Self::new(config_service)
        }
    }

    fn authorization_header(token: Option<&Secret<String>>) -> Result<HeaderValue> {
        let token = token
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

    fn map_request_error(error: reqwest::Error, request_timeout: Duration) -> Error {
        if error.is_timeout() {
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

    async fn post_once(&self, body: &LinkupSearchRequest<'_>) -> Result<LinkupResponse> {
        let rate_limit_permit = self.rate_limit.acquire().await;
        self.usage_limit.acquire().await;
        let url = format!(
            "{}{LINKUP_SEARCH_PATH}",
            self.base_url.trim_end_matches('/')
        );
        let (token, request_timeout_secs) = self
            .config_service
            .read(|config| {
                (
                    config.tools.linkup.token.clone(),
                    config.tools.linkup.request_timeout_secs,
                )
            })
            .await?;
        let authorization = Self::authorization_header(token.as_ref())?;
        let request_timeout = Duration::from_secs(request_timeout_secs);
        let request = self
            .http
            .post(url)
            .header(reqwest::header::AUTHORIZATION, authorization)
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .header(reqwest::header::USER_AGENT, LINKUP_USER_AGENT)
            .json(body)
            .timeout(request_timeout);

        let raw = request
            .send()
            .await
            .map_err(|error| Self::map_request_error(error, request_timeout))?;
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
                .map_err(|error| Self::map_request_error(error, request_timeout))?,
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
        let token = self
            .config_service
            .read(|config| config.tools.linkup.token.clone())
            .await?;
        Self::authorization_header(token.as_ref())?;
        let response = self.post_once(body).await?;
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
        self.post_once(body).await
    }
}

#[cfg(test)]
mod tests {
    use {
        crate::test_support::fixture, chelix_agents::test_support::ConfigFixture,
        chelix_config::ChelixConfig, mockito::Matcher, secrecy::Secret, serde_json::json,
    };

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
        let fixture = fixture(Some(Secret::new("api-token".to_string()))).await;
        let client = LinkupClient::for_test(server.url(), fixture.service.clone());

        let response = client
            .search(&request(Some(&domains)))
            .await
            .unwrap_or_else(|error| panic!("search failed: {error}"));

        assert_eq!(response.body(), r#"{"results":[]}"#);
        call.assert_async().await;
    }

    #[tokio::test]
    async fn next_request_uses_updated_credentials_and_timeout_through_body_read()
    -> anyhow::Result<()> {
        use tokio::io::AsyncWriteExt;

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let base_url = format!("http://{}", listener.local_addr()?);
        let (body_tx, body_rx) = tokio::sync::oneshot::channel();
        let server = tokio::spawn(async move {
            let (mut first, _) = listener.accept().await?;
            let headers = crate::test_support::read_request(&mut first).await?;
            assert!(headers.contains("Bearer initial\r\n"));
            first
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok")
                .await?;
            drop(first);

            let (mut second, _) = listener.accept().await?;
            let headers = crate::test_support::read_request(&mut second).await?;
            assert!(headers.contains("Bearer next\r\n"));
            second
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\no")
                .await?;
            body_tx
                .send(())
                .map_err(|_| anyhow::anyhow!("body receiver dropped"))?;
            tokio::time::sleep(Duration::from_secs(5)).await;
            Ok::<(), anyhow::Error>(())
        });
        let mut config = ChelixConfig::default();
        config.tools.linkup.token = Some(Secret::new("initial".to_string()));
        config.tools.linkup.request_timeout_secs = 3;
        let fixture = ConfigFixture::new(config).await?;
        let client = LinkupClient::for_test(base_url, fixture.service.clone());
        let body = request(None);
        assert_eq!(client.search(&body).await?.body(), "ok");

        fixture
            .service
            .update(|config| {
                config.tools.linkup.token = Some(Secret::new("next".to_string()));
                config.tools.linkup.request_timeout_secs = 1;
                Ok(())
            })
            .await?;
        let update = async {
            body_rx.await?;
            fixture
                .service
                .update(|config| {
                    config.tools.linkup.request_timeout_secs = 3;
                    Ok(())
                })
                .await?;
            Ok::<(), anyhow::Error>(())
        };
        let pending = client.search(&body);
        tokio::pin!(pending);
        tokio::select! {
            biased;
            result = &mut pending => panic!("request finished before config update: {result:?}"),
            updated = update => updated?,
        }
        let result = pending.await;
        let error = result
            .err()
            .unwrap_or_else(|| panic!("expected a body timeout"));
        assert_eq!(error.to_string(), "Linkup request timed out after 1s");
        server.abort();
        let _ = server.await;
        Ok(())
    }

    #[tokio::test]
    async fn missing_token_fails_before_sending_a_request() {
        let fixture = fixture(None).await;
        let client =
            LinkupClient::for_test("http://127.0.0.1:1".to_string(), fixture.service.clone());

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
        let fixture = fixture(Some(Secret::new("api-token".to_string()))).await;
        let client = LinkupClient::for_test(server.url(), fixture.service.clone());

        let error = client
            .search(&request(None))
            .await
            .err()
            .unwrap_or_else(|| panic!("authorization should fail"));

        assert!(matches!(error, Error::Authorization { .. }));
        assert_eq!(error.to_string(), "invalid token");
        call.assert_async().await;
    }

    #[tokio::test]
    async fn numeric_retry_after_retries_with_updated_credentials_through_the_shared_cooldown()
    -> anyhow::Result<()> {
        use tokio::io::AsyncWriteExt;

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let base_url = format!("http://{}", listener.local_addr()?);
        let (received_tx, received_rx) = tokio::sync::oneshot::channel();
        let server = tokio::spawn(async move {
            let (mut first, _) = listener.accept().await?;
            let headers = crate::test_support::read_request(&mut first).await?;
            assert!(headers.contains("Bearer initial\r\n"));
            first.write_all(b"HTTP/1.1 429 Too Many Requests\r\nContent-Length: 0\r\nRetry-After: 0\r\nConnection: close\r\n\r\n").await?;
            received_tx
                .send(())
                .map_err(|_| anyhow::anyhow!("response receiver dropped"))?;
            drop(first);

            let (mut retry, _) = listener.accept().await?;
            let headers = crate::test_support::read_request(&mut retry).await?;
            assert!(headers.contains("Bearer next\r\n"));
            let body = r#"{"results":[]}"#;
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            retry.write_all(response.as_bytes()).await?;
            Ok::<(), anyhow::Error>(())
        });
        let fixture = fixture(Some(Secret::new("initial".to_string()))).await;
        let client = LinkupClient::for_test(base_url, fixture.service.clone());
        let started = tokio::time::Instant::now();
        let body = request(None);
        let pending = client.search(&body);
        tokio::pin!(pending);
        tokio::select! {
            result = &mut pending => panic!("retry finished before config update: {result:?}"),
            sent = received_rx => sent?,
        }
        assert!(
            tokio::time::timeout(Duration::from_secs(1), &mut pending)
                .await
                .is_err()
        );
        fixture
            .service
            .update(|config| {
                config.tools.linkup.token = Some(Secret::new("next".to_string()));
                config.tools.linkup.request_timeout_secs = 1;
                Ok(())
            })
            .await?;
        assert_eq!(pending.await?.body(), r#"{"results":[]}"#);
        assert!(
            tokio::time::Instant::now() - started
                >= Duration::from_millis(LINKUP_RATE_LIMIT_COOLDOWN_BUFFER_MS)
        );
        server.await??;
        Ok(())
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
        let fixture = fixture(Some(Secret::new("api-token".to_string()))).await;
        let client = LinkupClient::for_test(server.url(), fixture.service.clone());

        let response = client
            .search(&request(None))
            .await
            .unwrap_or_else(|error| panic!("search failed: {error}"));

        assert!(response.is_rate_limited());
        assert_eq!(response.rate_limit_cooldown_ms(), None);
        limited.assert_async().await;
    }

    #[tokio::test]
    async fn timed_out_probe_reopens_the_shared_gate() -> anyhow::Result<()> {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .unwrap_or_else(|error| panic!("test listener failed to bind: {error}"));
        let address = listener
            .local_addr()
            .unwrap_or_else(|error| panic!("test listener has no local address: {error}"));
        let (received_tx, received_rx) = tokio::sync::oneshot::channel();
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await?;
            crate::test_support::read_request(&mut socket).await?;
            received_tx
                .send(())
                .map_err(|_| anyhow::anyhow!("request receiver dropped"))?;
            tokio::time::sleep(Duration::from_secs(5)).await;
            Ok::<(), anyhow::Error>(())
        });
        let mut config = ChelixConfig::default();
        config.tools.linkup.token = Some(Secret::new("api-token".to_string()));
        config.tools.linkup.request_timeout_secs = 1;
        let fixture = ConfigFixture::new(config)
            .await
            .unwrap_or_else(|error| panic!("config fixture failed: {error}"));
        let client = LinkupClient::for_test(format!("http://{address}"), fixture.service.clone());
        client.rate_limit.acquire().await.complete(true, Some(0));

        let update = async {
            received_rx.await?;
            fixture
                .service
                .update(|config| {
                    config.tools.linkup.request_timeout_secs = 2;
                    Ok(())
                })
                .await?;
            Ok::<(), anyhow::Error>(())
        };
        let body = request(None);
        let pending = client.search(&body);
        tokio::pin!(pending);
        tokio::select! {
            biased;
            result = &mut pending => panic!("request finished before config update: {result:?}"),
            updated = update => updated?,
        }
        let result = pending.await;
        let error = result
            .err()
            .unwrap_or_else(|| panic!("expected a request timeout"));

        assert_eq!(error.to_string(), "Linkup request timed out after 1s");
        let permit = tokio::time::timeout(Duration::from_millis(50), client.rate_limit.acquire())
            .await
            .unwrap_or_else(|_| panic!("shared gate remained blocked after probe timeout"));
        permit.complete(false, None);
        server.abort();
        let _ = server.await;
        Ok(())
    }
}
