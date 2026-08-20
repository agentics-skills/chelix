//! HTTP transport boundary for DuckDuckGo search.

use std::sync::Arc;

use {
    async_trait::async_trait,
    reqwest::{StatusCode, cookie::Jar},
    url::Url,
};

use crate::error::Result;

const USER_AGENT: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36";
const ACCEPT: &str = "text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8";
const ACCEPT_LANGUAGE: &str = "en-US,en;q=0.9";
const ACCEPT_ENCODING: &str = "gzip, deflate, br";

/// Complete HTTP response needed by the search client.
#[derive(Debug, Clone)]
pub(crate) struct HttpResponse {
    pub(crate) status: StatusCode,
    pub(crate) retry_after: Option<String>,
    pub(crate) body: String,
}

/// Replaceable HTTP execution boundary.
#[async_trait]
pub(crate) trait HttpTransport: Send + Sync {
    async fn get(&self, url: &Url) -> Result<HttpResponse>;
}

/// Current Reqwest transport with one shared in-memory cookie jar.
pub(crate) struct ReqwestTransport {
    client: reqwest::Client,
}

impl ReqwestTransport {
    pub(crate) fn new() -> Result<Self> {
        let cookie_jar = Arc::new(Jar::default());
        let builder = reqwest::Client::builder().cookie_provider(cookie_jar);
        let client = chelix_common::http_client::apply_proxy(builder).build()?;
        Ok(Self { client })
    }
}

#[async_trait]
impl HttpTransport for ReqwestTransport {
    #[cfg_attr(feature = "tracing", tracing::instrument(skip(self, url)))]
    async fn get(&self, url: &Url) -> Result<HttpResponse> {
        let raw = self
            .client
            .get(url.clone())
            .header(reqwest::header::USER_AGENT, USER_AGENT)
            .header(reqwest::header::ACCEPT, ACCEPT)
            .header(reqwest::header::ACCEPT_LANGUAGE, ACCEPT_LANGUAGE)
            .header(reqwest::header::ACCEPT_ENCODING, ACCEPT_ENCODING)
            .send()
            .await?;
        let status = raw.status();
        let retry_after = raw
            .headers()
            .get(reqwest::header::RETRY_AFTER)
            .and_then(|value| value.to_str().ok())
            .map(str::to_string);
        let body = raw.text().await?;
        Ok(HttpResponse {
            status,
            retry_after,
            body,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn reqwest_transport_reuses_response_cookies() {
        let mut server = mockito::Server::new_async().await;
        let set_cookie = server
            .mock("GET", "/first")
            .match_header("user-agent", USER_AGENT)
            .match_header("accept", ACCEPT)
            .match_header("accept-language", ACCEPT_LANGUAGE)
            .match_header("accept-encoding", ACCEPT_ENCODING)
            .with_status(200)
            .with_header("set-cookie", "session=stored; Path=/")
            .with_body("first")
            .expect(1)
            .create_async()
            .await;
        let receive_cookie = server
            .mock("GET", "/second")
            .match_header("cookie", "session=stored")
            .with_status(200)
            .with_body("second")
            .expect(1)
            .create_async()
            .await;
        let transport = ReqwestTransport::new()
            .unwrap_or_else(|error| panic!("transport creation failed: {error}"));
        let first_url = Url::parse(&format!("{}/first", server.url()))
            .unwrap_or_else(|error| panic!("first URL is invalid: {error}"));
        let second_url = Url::parse(&format!("{}/second", server.url()))
            .unwrap_or_else(|error| panic!("second URL is invalid: {error}"));

        let first = transport
            .get(&first_url)
            .await
            .unwrap_or_else(|error| panic!("first request failed: {error}"));
        let second = transport
            .get(&second_url)
            .await
            .unwrap_or_else(|error| panic!("second request failed: {error}"));

        assert_eq!(first.body, "first");
        assert_eq!(second.body, "second");
        set_cookie.assert_async().await;
        receive_cookie.assert_async().await;
    }
}
