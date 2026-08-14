//! Authenticated GitHub REST client shared by every `github_*` tool.
//!
//! The token comes from `tools.github.pat`. A request sent with a configured
//! token that GitHub answers with `401` or `403` is an explicit authorization
//! error; the client never prompts for another token.

use {
    reqwest::{StatusCode, header::HeaderValue},
    secrecy::{ExposeSecret, Secret},
};

use crate::error::{Error, Result};

/// GitHub REST API root used by every tool.
pub const GITHUB_API_BASE_URL: &str = "https://api.github.com";

/// Media type requested when a tool does not need a specialised one.
const DEFAULT_ACCEPT: &str = "application/vnd.github.v3+json";

/// REST API version pinned for every request.
const GITHUB_API_VERSION: &str = "2022-11-28";

/// Extra delay added on top of a computed rate-limit cooldown.
const GITHUB_RATE_LIMIT_COOLDOWN_BUFFER_MS: u64 = 5_000;

/// Per-request behaviour toggles.
#[derive(Debug, Clone, Copy, Default)]
pub struct RequestOptions {
    /// Hand a rate-limited response back to the caller instead of treating it
    /// as an authorization failure, so the caller can apply its own cooldown.
    pub return_rate_limit_response: bool,
}

/// One fully read GitHub REST response.
#[derive(Debug, Clone)]
pub struct GitHubResponse {
    status: StatusCode,
    retry_after: Option<String>,
    rate_limit_remaining: Option<String>,
    rate_limit_reset: Option<String>,
    body: String,
}

impl GitHubResponse {
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

    /// Whether the response reports a primary or secondary rate limit.
    #[must_use]
    pub fn is_rate_limited(&self) -> bool {
        if !matches!(
            self.status,
            StatusCode::FORBIDDEN | StatusCode::TOO_MANY_REQUESTS
        ) {
            return false;
        }
        if self.retry_after.is_some() {
            return true;
        }
        if self.rate_limit_remaining.as_deref() == Some("0") {
            return true;
        }
        self.body.to_lowercase().contains("rate limit")
    }

    /// Cooldown to wait before one retry, or `None` when the response carries
    /// no usable rate-limit timing.
    #[must_use]
    pub fn rate_limit_cooldown_ms(&self) -> Option<u64> {
        if let Some(retry_after) = &self.retry_after
            && let Ok(seconds) = retry_after.parse::<u64>()
        {
            return Some(seconds * 1_000 + GITHUB_RATE_LIMIT_COOLDOWN_BUFFER_MS);
        }

        if self.rate_limit_remaining.as_deref() == Some("0")
            && let Some(reset) = &self.rate_limit_reset
            && let Ok(reset_seconds) = reset.parse::<i64>()
            && reset_seconds >= 0
        {
            let now_ms = time::OffsetDateTime::now_utc().unix_timestamp() * 1_000;
            let remaining_ms = u64::try_from((reset_seconds * 1_000 - now_ms).max(0))
                .unwrap_or(GITHUB_RATE_LIMIT_COOLDOWN_BUFFER_MS);
            return Some(remaining_ms + GITHUB_RATE_LIMIT_COOLDOWN_BUFFER_MS);
        }

        None
    }
}

/// HTTP client bound to one optional personal access token.
pub struct GitHubClient {
    http: reqwest::Client,
    base_url: String,
    token: Option<Secret<String>>,
}

impl GitHubClient {
    /// Build a client for the public GitHub API.
    #[must_use]
    pub fn new(token: Option<Secret<String>>) -> Self {
        Self {
            http: chelix_common::http_client::build_default_http_client(),
            base_url: GITHUB_API_BASE_URL.to_string(),
            token,
        }
    }

    /// Build a client pointed at a test double.
    #[cfg(test)]
    pub(crate) fn for_test(base_url: String, token: Option<Secret<String>>) -> Self {
        Self {
            http: chelix_common::http_client::build_default_http_client(),
            base_url,
            token,
        }
    }

    /// Absolute API root, without a trailing slash.
    #[must_use]
    pub fn base_url(&self) -> &str {
        self.base_url.trim_end_matches('/')
    }

    /// Fail when a tool that requires authentication has no configured token.
    pub fn require_token(&self) -> Result<()> {
        if self.token.is_none() {
            return Err(Error::MissingToken);
        }
        Ok(())
    }

    /// Perform one authenticated `GET` and read the complete response.
    pub async fn get(&self, url: &url::Url, options: RequestOptions) -> Result<GitHubResponse> {
        let mut request = self
            .http
            .get(url.as_str())
            .header(reqwest::header::ACCEPT, DEFAULT_ACCEPT)
            .header("X-GitHub-Api-Version", GITHUB_API_VERSION);
        if let Some(token) = &self.token {
            let mut authorization =
                HeaderValue::try_from(format!("Bearer {}", token.expose_secret()))
                    .map_err(|error| Error::message(format!("invalid GitHub token: {error}")))?;
            authorization.set_sensitive(true);
            request = request.header(reqwest::header::AUTHORIZATION, authorization);
        }

        let raw = request.send().await?;
        let status = raw.status();
        let header = |name: &str| {
            raw.headers()
                .get(name)
                .and_then(|value| value.to_str().ok())
                .map(str::to_string)
        };
        let response = GitHubResponse {
            status,
            retry_after: header("retry-after"),
            rate_limit_remaining: header("x-ratelimit-remaining"),
            rate_limit_reset: header("x-ratelimit-reset"),
            body: raw.text().await?,
        };

        if options.return_rate_limit_response && response.is_rate_limited() {
            return Ok(response);
        }

        if matches!(status, StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN)
            && self.token.is_some()
        {
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
    ) -> Result<GitHubResponse> {
        let url = url::Url::parse(&format!("{}/probe", server.url()))
            .unwrap_or_else(|error| panic!("probe URL is invalid: {error}"));
        GitHubClient::for_test(server.url(), None)
            .get(&url, options)
            .await
    }

    #[test]
    fn missing_token_is_reported_before_any_request() {
        let error = match GitHubClient::new(None).require_token() {
            Ok(()) => panic!("expected a missing token error"),
            Err(error) => error,
        };

        assert!(matches!(error, Error::MissingToken));
        assert_eq!(
            error.to_string(),
            "GitHub personal access token is not configured: set `tools.github.pat` in chelix.toml"
        );
    }

    #[tokio::test]
    async fn authorization_failure_with_token_is_an_error() {
        let mut server = mockito::Server::new_async().await;
        let call = server
            .mock("GET", "/probe")
            .match_header("authorization", "Bearer pat-token")
            .match_header("x-github-api-version", "2022-11-28")
            .with_status(401)
            .with_body("{\"message\":\"Bad credentials\"}")
            .expect(1)
            .create_async()
            .await;
        let url = url::Url::parse(&format!("{}/probe", server.url()))
            .unwrap_or_else(|error| panic!("probe URL is invalid: {error}"));
        let client =
            GitHubClient::for_test(server.url(), Some(Secret::new("pat-token".to_string())));

        let error = match client.get(&url, RequestOptions::default()).await {
            Ok(_) => panic!("expected an authorization error"),
            Err(error) => error,
        };

        assert!(matches!(error, Error::Authorization { .. }));
        assert!(error.to_string().contains("Bad credentials"));
        call.assert_async().await;
    }

    #[tokio::test]
    async fn unauthenticated_forbidden_response_is_returned_to_the_caller() {
        let mut server = mockito::Server::new_async().await;
        let _call = server
            .mock("GET", "/probe")
            .with_status(403)
            .with_body("")
            .create_async()
            .await;

        let response = probe(&server, RequestOptions::default())
            .await
            .unwrap_or_else(|error| panic!("probe request failed: {error}"));

        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        assert!(!response.is_success());
        assert_eq!(response.failure_message(), "403 Forbidden");
    }

    #[tokio::test]
    async fn retry_after_header_drives_the_cooldown() {
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
    async fn exhausted_quota_uses_the_reset_header() {
        let mut server = mockito::Server::new_async().await;
        let reset = time::OffsetDateTime::now_utc().unix_timestamp() + 30;
        let _call = server
            .mock("GET", "/probe")
            .with_status(403)
            .with_header("x-ratelimit-remaining", "0")
            .with_header("x-ratelimit-reset", &reset.to_string())
            .with_body("API rate limit exceeded")
            .create_async()
            .await;

        let response = probe(&server, RequestOptions {
            return_rate_limit_response: true,
        })
        .await
        .unwrap_or_else(|error| panic!("probe request failed: {error}"));

        assert!(response.is_rate_limited());
        let cooldown = response
            .rate_limit_cooldown_ms()
            .unwrap_or_else(|| panic!("expected a cooldown from the reset header"));
        assert!((30_000..=35_000).contains(&cooldown), "cooldown {cooldown}");
    }

    #[tokio::test]
    async fn secondary_limit_without_timing_has_no_cooldown() {
        let mut server = mockito::Server::new_async().await;
        let _call = server
            .mock("GET", "/probe")
            .with_status(403)
            .with_body("You have exceeded a secondary rate limit")
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
}
