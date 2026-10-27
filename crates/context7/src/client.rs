//! Authenticated Context7 HTTP client shared by every `context7_*` tool.
//!
//! Tokens come from `tools.context7.token` in priority order. A missing or empty
//! list sends requests without authorization. A `429` response whose JSON field
//! `error` is `Quota Exceeded` rotates to the next token and retries before the
//! shared cooldown. A request sent with a configured token that Context7 answers
//! with `401` or `403` is an explicit authorization error; the client never
//! prompts for another token.

use {
    reqwest::{StatusCode, header::HeaderValue},
    secrecy::ExposeSecret,
    std::time::Duration,
};

use crate::{
    error::{Error, Result},
    rate_limit::{CONTEXT7_MAX_RATE_LIMIT_COOLDOWN_MS, RateLimitCoordinator},
    tokens::Context7Tokens,
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

    /// `429` whose JSON field `error` is exactly `Quota Exceeded`.
    #[must_use]
    fn is_quota_exceeded(&self) -> bool {
        self.is_rate_limited()
            && serde_json::from_str::<QuotaExceededBody>(&self.body)
                .is_ok_and(|body| body.error == "Quota Exceeded")
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

#[derive(serde::Deserialize)]
struct QuotaExceededBody {
    error: String,
}

/// HTTP client bound to the configured Context7 API tokens.
pub struct Context7Client {
    http: reqwest::Client,
    base_url: String,
    tokens: Context7Tokens,
    config_service: chelix_config_service::ConfigService,
    rate_limit: RateLimitCoordinator,
}

impl Context7Client {
    /// Build a client for the public Context7 API.
    #[must_use]
    pub fn new(config_service: chelix_config_service::ConfigService) -> Self {
        Self {
            http: chelix_common::http_client::build_default_http_client(),
            base_url: CONTEXT7_API_BASE_URL.to_string(),
            config_service,
            tokens: Context7Tokens::default(),
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

    /// Absolute API root, without a trailing slash.
    #[must_use]
    pub fn base_url(&self) -> &str {
        self.base_url.trim_end_matches('/')
    }

    fn map_request_error(error: reqwest::Error, request_timeout: Duration) -> Error {
        if error.is_timeout() {
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
    ///
    /// A `429` body whose `error` field is `Quota Exceeded` rotates the token and
    /// retries before the shared cooldown, with N - 1 retries for N configured tokens.
    /// Each call restarts its retry budget when it observes a new token-list generation.
    /// Tokens shared by successive lists can be sent again after the reset.
    pub async fn get(&self, url: &url::Url, options: RequestOptions) -> Result<Context7Response> {
        let rate_limit_permit = self.rate_limit.acquire().await;
        let response = self.get_rotating_on_quota(url).await?;
        rate_limit_permit.complete(
            response.is_rate_limited(),
            response.rate_limit_cooldown_ms(),
        );

        if options.return_rate_limit_response && response.is_rate_limited() {
            return Ok(response);
        }

        if matches!(
            response.status(),
            StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN
        ) {
            return Err(Error::authorization(response.failure_message()));
        }

        Ok(response)
    }

    async fn get_rotating_on_quota(&self, url: &url::Url) -> Result<Context7Response> {
        let (mut response, count, mut generation) = self.send_get(url, false).await?;
        let mut remaining = count.saturating_sub(1);
        while response.is_quota_exceeded() && remaining > 0 {
            #[cfg(feature = "tracing")]
            tracing::warn!("Context7 quota exceeded; rotating API token and retrying the request");
            let (next_response, count, next_generation) = self.send_get(url, true).await?;
            response = next_response;
            remaining = if next_generation != generation {
                count.saturating_sub(1)
            } else {
                remaining - 1
            };
            generation = next_generation;
        }
        Ok(response)
    }

    async fn send_get(
        &self,
        url: &url::Url,
        rotate: bool,
    ) -> Result<(Context7Response, usize, u64)> {
        let (tokens, request_timeout_secs) = self
            .config_service
            .read(|config| {
                (
                    config.tools.context7.token.clone(),
                    config.tools.context7.request_timeout_secs,
                )
            })
            .await?;
        let (token, count, generation) = self.tokens.select(tokens, rotate);
        let request_timeout = Duration::from_secs(request_timeout_secs);
        let mut request = self
            .http
            .get(url.as_str())
            .header("X-Context7-Source", CONTEXT7_SOURCE);
        if let Some(token) = token {
            let mut authorization =
                HeaderValue::try_from(format!("Bearer {}", token.expose_secret()))
                    .map_err(|error| Error::message(format!("invalid Context7 token: {error}")))?;
            authorization.set_sensitive(true);
            request = request.header(reqwest::header::AUTHORIZATION, authorization);
        }

        request = request.timeout(request_timeout);
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
        Ok((
            Context7Response {
                status,
                retry_after,
                body: raw
                    .text()
                    .await
                    .map_err(|error| Self::map_request_error(error, request_timeout))?,
            },
            count,
            generation,
        ))
    }
}

#[cfg(test)]
mod tests {
    use {
        super::*, crate::test_support::fixture, chelix_agents::test_support::ConfigFixture,
        chelix_config::ChelixConfig, secrecy::Secret,
    };

    async fn probe(
        server: &mockito::ServerGuard,
        options: RequestOptions,
    ) -> Result<Context7Response> {
        let url = url::Url::parse(&format!("{}/probe", server.url()))
            .unwrap_or_else(|error| panic!("probe URL is invalid: {error}"));
        let fixture = fixture(Vec::new()).await;
        Context7Client::for_test(server.url(), fixture.service.clone())
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
        let fixture = fixture(vec![Secret::new("api-token".to_string())]).await;
        let client = Context7Client::for_test(server.url(), fixture.service.clone());

        let response = client
            .get(&url, RequestOptions::default())
            .await
            .unwrap_or_else(|error| panic!("request failed: {error}"));

        assert_eq!(response.body(), "ok");
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
        config.tools.context7.token = vec![Secret::new("initial".to_string())];
        config.tools.context7.request_timeout_secs = 3;
        let fixture = ConfigFixture::new(config).await?;
        let client = Context7Client::for_test(base_url.clone(), fixture.service.clone());
        let url = url::Url::parse(&format!("{base_url}/probe"))?;
        assert_eq!(
            client.get(&url, RequestOptions::default()).await?.body(),
            "ok"
        );

        fixture
            .service
            .update(|config| {
                config.tools.context7.token = vec![Secret::new("next".to_string())];
                config.tools.context7.request_timeout_secs = 1;
                Ok(())
            })
            .await?;
        let update = async {
            body_rx.await?;
            fixture
                .service
                .update(|config| {
                    config.tools.context7.request_timeout_secs = 3;
                    Ok(())
                })
                .await?;
            Ok::<(), anyhow::Error>(())
        };
        let pending = client.get(&url, RequestOptions::default());
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
        assert_eq!(error.to_string(), "Context7 request timed out after 1s");
        server.abort();
        let _ = server.await;
        Ok(())
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

    #[tokio::test]
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
        let fixture = fixture(Vec::new()).await;
        let client = Context7Client::for_test(server.url(), fixture.service.clone());
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

        limited.assert_async().await;
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
        let base_url = format!("http://{address}");
        let mut config = ChelixConfig::default();
        config.tools.context7.request_timeout_secs = 1;
        let fixture = ConfigFixture::new(config)
            .await
            .unwrap_or_else(|error| panic!("config fixture failed: {error}"));
        let client = Context7Client::for_test(base_url.clone(), fixture.service.clone());
        client.rate_limit.acquire().await.complete(true, Some(0));
        let url = url::Url::parse(&format!("{base_url}/probe"))
            .unwrap_or_else(|error| panic!("probe URL is invalid: {error}"));

        let update = async {
            received_rx.await?;
            fixture
                .service
                .update(|config| {
                    config.tools.context7.request_timeout_secs = 2;
                    Ok(())
                })
                .await?;
            Ok::<(), anyhow::Error>(())
        };
        let pending = client.get(&url, RequestOptions::default());
        tokio::pin!(pending);
        tokio::select! {
            biased;
            result = &mut pending => panic!("request finished before config update: {result:?}"),
            updated = update => updated?,
        }
        let result = pending.await;
        let error = match result {
            Ok(_) => panic!("expected a request timeout"),
            Err(error) => error,
        };

        assert_eq!(error.to_string(), "Context7 request timed out after 1s");
        let permit = tokio::time::timeout(Duration::from_millis(50), client.rate_limit.acquire())
            .await
            .unwrap_or_else(|_| panic!("shared gate remained blocked after probe timeout"));
        permit.complete(false, None);
        server.abort();
        let _ = server.await;
        Ok(())
    }

    #[tokio::test]
    async fn quota_exceeded_rotates_before_cooldown_and_keeps_the_next_token() {
        let mut server = mockito::Server::new_async().await;
        let recovered = server
            .mock("GET", "/probe")
            .match_header("authorization", "Bearer second")
            .with_status(200)
            .with_body("ok")
            .expect(1)
            .create_async()
            .await;
        let again = server
            .mock("GET", "/probe")
            .match_header("authorization", "Bearer second")
            .with_status(200)
            .with_body("again")
            .expect(1)
            .create_async()
            .await;
        let limited = server
            .mock("GET", "/probe")
            .match_header("authorization", "Bearer first")
            .with_status(429)
            .with_header("retry-after", "7")
            .with_body(r#"{"error":"Quota Exceeded"}"#)
            .expect(1)
            .create_async()
            .await;
        let fixture = fixture(vec![
            Secret::new("first".to_string()),
            Secret::new("second".to_string()),
        ])
        .await;
        let client = Context7Client::for_test(server.url(), fixture.service.clone());
        let url = url::Url::parse(&format!("{}/probe", server.url()))
            .unwrap_or_else(|error| panic!("probe URL is invalid: {error}"));
        let started = tokio::time::Instant::now();

        let response = client
            .get(&url, RequestOptions::default())
            .await
            .unwrap_or_else(|error| panic!("rotated request failed: {error}"));
        assert_eq!(response.body(), "ok");

        let response = client
            .get(&url, RequestOptions::default())
            .await
            .unwrap_or_else(|error| panic!("follow-up request failed: {error}"));
        assert_eq!(response.body(), "again");
        assert!(tokio::time::Instant::now().duration_since(started) < Duration::from_secs(12));
        limited.assert_async().await;
        recovered.assert_async().await;
        again.assert_async().await;
    }

    #[tokio::test]
    async fn quota_retry_rebuilds_attempts_for_changed_tokens() -> anyhow::Result<()> {
        use tokio::io::AsyncWriteExt;

        let fixture = fixture(Vec::new()).await;
        for (new_tokens, sibling_request, expected_tokens) in [
            (
                &["next", "spare", "extra"][..],
                true,
                &[Some("spare"), Some("extra"), Some("next")][..],
            ),
            (&["next"][..], false, &[Some("next")][..]),
            (&[][..], false, &[None][..]),
            (
                &["second", "first"][..],
                false,
                &[Some("second"), Some("first")][..],
            ),
        ] {
            fixture
                .service
                .update(|config| {
                    config.tools.context7.token = vec![
                        Secret::new("first".to_string()),
                        Secret::new("second".to_string()),
                    ];
                    Ok(())
                })
                .await?;
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
            let base_url = format!("http://{}", listener.local_addr()?);
            let (received_tx, received_rx) = tokio::sync::oneshot::channel();
            let (resume_tx, resume_rx) = tokio::sync::oneshot::channel();
            let expected_tokens = expected_tokens.to_vec();
            let server = tokio::spawn(async move {
                let quota = r#"{"error":"Quota Exceeded"}"#;
                let limited = format!(
                    "HTTP/1.1 429 Too Many Requests\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{quota}",
                    quota.len()
                );
                let success =
                    b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok";
                let (mut first, _) = listener.accept().await?;
                let headers = crate::test_support::read_request(&mut first).await?;
                assert!(headers.contains("Bearer first\r\n"));
                received_tx
                    .send(())
                    .map_err(|_| anyhow::anyhow!("request receiver dropped"))?;
                if sibling_request {
                    let (mut sibling, _) = listener.accept().await?;
                    let headers = crate::test_support::read_request(&mut sibling).await?;
                    assert!(headers.contains("Bearer next\r\n"));
                    sibling.write_all(success).await?;
                }
                resume_rx.await?;
                first.write_all(limited.as_bytes()).await?;
                drop(first);

                for (index, expected) in expected_tokens
                    .iter()
                    .chain(expected_tokens.last())
                    .enumerate()
                {
                    let (mut socket, _) = listener.accept().await?;
                    let headers = crate::test_support::read_request(&mut socket).await?;
                    match expected {
                        Some(token) => assert!(headers.contains(&format!("Bearer {token}\r\n"))),
                        None => assert!(!headers.lines().any(|line| {
                            line.split_once(':')
                                .is_some_and(|(name, _)| name.eq_ignore_ascii_case("authorization"))
                        })),
                    }
                    let response = if index == expected_tokens.len() {
                        &success[..]
                    } else {
                        limited.as_bytes()
                    };
                    socket.write_all(response).await?;
                }
                Ok::<(), anyhow::Error>(())
            });
            let client = Context7Client::for_test(base_url.clone(), fixture.service.clone());
            let url = url::Url::parse(&format!("{base_url}/probe"))?;
            let replacement = new_tokens
                .iter()
                .map(|token| Secret::new((*token).to_string()))
                .collect();
            let update = async {
                received_rx.await?;
                fixture
                    .service
                    .update(move |config| {
                        config.tools.context7.token = replacement;
                        Ok(())
                    })
                    .await?;
                if sibling_request {
                    assert_eq!(
                        client.get(&url, RequestOptions::default()).await?.body(),
                        "ok"
                    );
                }
                resume_tx
                    .send(())
                    .map_err(|_| anyhow::anyhow!("response receiver dropped"))?;
                Ok::<(), anyhow::Error>(())
            };
            let (response, updated) =
                tokio::join!(client.get(&url, RequestOptions::default()), update);
            updated?;
            assert_eq!(response?.status(), StatusCode::TOO_MANY_REQUESTS);
            assert_eq!(
                client.get(&url, RequestOptions::default()).await?.body(),
                "ok"
            );
            server.await??;
        }
        Ok(())
    }

    #[tokio::test]
    async fn quota_exceeded_returns_the_last_response_after_rotating_the_other_tokens() {
        let mut server = mockito::Server::new_async().await;
        let first = server
            .mock("GET", "/probe")
            .match_header("authorization", "Bearer first")
            .with_status(429)
            .with_body(r#"{"error":"Quota Exceeded"}"#)
            .expect(1)
            .create_async()
            .await;
        let second = server
            .mock("GET", "/probe")
            .match_header("authorization", "Bearer second")
            .with_status(429)
            .with_body(r#"{"error":"Quota Exceeded"}"#)
            .expect(1)
            .create_async()
            .await;
        let third = server
            .mock("GET", "/probe")
            .match_header("authorization", "Bearer third")
            .with_status(429)
            .with_body(r#"{"error":"Quota Exceeded"}"#)
            .expect(1)
            .create_async()
            .await;
        let fixture = fixture(vec![
            Secret::new("first".to_string()),
            Secret::new("second".to_string()),
            Secret::new("third".to_string()),
        ])
        .await;
        let client = Context7Client::for_test(server.url(), fixture.service.clone());
        let url = url::Url::parse(&format!("{}/probe", server.url()))
            .unwrap_or_else(|error| panic!("probe URL is invalid: {error}"));

        let response = client
            .get(&url, RequestOptions::default())
            .await
            .unwrap_or_else(|error| panic!("quota request failed: {error}"));

        assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(response.body(), r#"{"error":"Quota Exceeded"}"#);
        first.assert_async().await;
        second.assert_async().await;
        third.assert_async().await;
    }
}
