//! DuckDuckGo search client with shared queueing, retry, and timeout handling.

use std::{sync::Arc, time::Duration};

use {chelix_config_service::ConfigService, url::Url};

use crate::{
    error::{Error, Result},
    parser::{SearchParser, SearchResultItem},
    rate_limit::{BASE_INTERVAL, RequestCoordinator},
    transport::{HttpResponse, HttpTransport, ReqwestTransport},
};

const DUCKDUCKGO_SEARCH_URL: &str = "https://duckduckgo.com/html/";

/// Shared client used by every `duckduckgo_search` invocation.
pub(crate) struct DuckDuckGoClient {
    transport: Arc<dyn HttpTransport>,
    coordinator: RequestCoordinator,
    parser: SearchParser,
    search_url: String,
    config: ConfigService,
}

impl DuckDuckGoClient {
    pub(crate) fn new(config: ConfigService) -> Result<Self> {
        Ok(Self {
            transport: Arc::new(ReqwestTransport::new()?),
            coordinator: RequestCoordinator::default(),
            parser: SearchParser::new()?,
            search_url: DUCKDUCKGO_SEARCH_URL.to_string(),
            config,
        })
    }

    #[cfg(test)]
    pub(crate) fn for_test(
        transport: Arc<dyn HttpTransport>,
        search_url: String,
        config: ConfigService,
    ) -> Self {
        Self {
            transport,
            coordinator: RequestCoordinator::default(),
            parser: SearchParser::new()
                .unwrap_or_else(|error| panic!("test parser creation failed: {error}")),
            search_url,
            config,
        }
    }

    #[cfg_attr(feature = "tracing", tracing::instrument(skip(self, query)))]
    pub(crate) async fn search(
        &self,
        query: &str,
        page: u32,
        num_results: u32,
    ) -> Result<Vec<SearchResultItem>> {
        let request_timeout_secs = self
            .config
            .read(|config| config.tools.duckduckgo.request_timeout_secs)
            .await?;
        if request_timeout_secs == 0 {
            return Err(Error::message(
                "tools.duckduckgo.request_timeout_secs must be at least 1",
            ));
        }
        let request_timeout = Duration::from_secs(request_timeout_secs);
        match tokio::time::timeout(
            request_timeout,
            self.search_with_retry(query, page, num_results),
        )
        .await
        {
            Ok(result) => result,
            Err(_) => Err(Error::message(format!(
                "duckduckgo_search timed out after {} second(s) while waiting for the shared request queue, retry/backoff, and HTTP response",
                request_timeout.as_secs()
            ))),
        }
    }

    async fn search_with_retry(
        &self,
        query: &str,
        page: u32,
        num_results: u32,
    ) -> Result<Vec<SearchResultItem>> {
        let url = self.build_url(query, page)?;
        let permit = self.coordinator.acquire().await?;
        permit.wait_for_initial_request().await;
        let mut response = self.transport.get(&url).await?;

        if self.is_rate_limited(&response) {
            let backoff = rate_limit_backoff(&response);
            #[cfg(feature = "tracing")]
            tracing::warn!(
                backoff = ?backoff,
                "duckduckgo_search hit a request limit; retrying once while holding the shared queue"
            );
            permit.wait_for_retry(backoff).await;
            response = self.transport.get(&url).await?;
        }

        self.parse_response(response, num_results)
    }

    fn is_rate_limited(&self, response: &HttpResponse) -> bool {
        response.status == reqwest::StatusCode::TOO_MANY_REQUESTS
            || (response.status.is_success() && self.parser.is_block_page(&response.body))
    }

    fn build_url(&self, query: &str, page: u32) -> Result<Url> {
        let start_index = u64::from(page.saturating_sub(1)) * 10;
        let encoded_query = urlencoding::encode(query);
        Ok(Url::parse(&format!(
            "{}?q={encoded_query}&s={start_index}",
            self.search_url
        ))?)
    }

    fn parse_response(
        &self,
        response: HttpResponse,
        num_results: u32,
    ) -> Result<Vec<SearchResultItem>> {
        if !response.status.is_success() {
            return Err(Error::message(format!(
                "Failed to fetch search results: {}",
                response.status.as_u16()
            )));
        }
        if self.parser.is_block_page(&response.body) {
            return Err(Error::message(
                "Request limit exceeded, try other tool for search",
            ));
        }
        self.parser.parse(
            &response.body,
            usize::try_from(num_results).unwrap_or(usize::MAX),
        )
    }
}

fn rate_limit_backoff(response: &HttpResponse) -> Duration {
    response
        .retry_after
        .as_deref()
        .and_then(|value| value.parse::<u64>().ok())
        .map(Duration::from_secs)
        .unwrap_or(BASE_INTERVAL)
        .max(BASE_INTERVAL)
}

#[cfg(test)]
mod tests {
    use std::{
        collections::VecDeque,
        sync::{
            Mutex,
            atomic::{AtomicUsize, Ordering},
        },
    };

    use {async_trait::async_trait, reqwest::StatusCode};

    use {super::*, chelix_agents::test_support::ConfigFixture, chelix_config::ChelixConfig};

    async fn fixture(timeout_secs: u64) -> ConfigFixture {
        let mut config = ChelixConfig::default();
        config.tools.duckduckgo.request_timeout_secs = timeout_secs;
        ConfigFixture::new(config)
            .await
            .unwrap_or_else(|error| panic!("fixture failed: {error}"))
    }

    struct FakeTransport {
        responses: Mutex<VecDeque<HttpResponse>>,
        calls: AtomicUsize,
    }

    impl FakeTransport {
        fn new(responses: Vec<HttpResponse>) -> Self {
            Self {
                responses: Mutex::new(responses.into()),
                calls: AtomicUsize::new(0),
            }
        }

        fn calls(&self) -> usize {
            self.calls.load(Ordering::SeqCst)
        }
    }

    #[async_trait]
    impl HttpTransport for FakeTransport {
        async fn get(&self, _url: &Url) -> Result<HttpResponse> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.responses
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .pop_front()
                .ok_or_else(|| Error::message("fake response queue is empty"))
        }
    }

    fn success_html() -> String {
        format!(
            r#"<a class="result__snippet">Snippet</a><span class="result__url">example.com</span><a class="result__a" href="https://example.com">Example</a>{}"#,
            " ".repeat(1_000)
        )
    }

    fn response(status: StatusCode, body: String, retry_after: Option<&str>) -> HttpResponse {
        HttpResponse {
            status,
            retry_after: retry_after.map(str::to_string),
            body,
        }
    }

    #[tokio::test]
    async fn zero_request_timeout_is_rejected_before_transport() {
        let fixture = fixture(0).await;
        let transport = Arc::new(FakeTransport::new(vec![]));
        let client = DuckDuckGoClient::for_test(
            transport.clone(),
            "https://duckduckgo.test/html/".to_string(),
            fixture.service.clone(),
        );
        let error = match client.search("rust", 1, 10).await {
            Ok(_) => panic!("zero timeout must be rejected"),
            Err(error) => error,
        };
        assert_eq!(
            error.to_string(),
            "tools.duckduckgo.request_timeout_secs must be at least 1"
        );
        assert_eq!(transport.calls(), 0);
    }

    #[tokio::test(start_paused = true)]
    async fn retries_one_rate_limited_response() {
        let transport = Arc::new(FakeTransport::new(vec![
            response(StatusCode::TOO_MANY_REQUESTS, "slow down".into(), Some("1")),
            response(StatusCode::OK, success_html(), None),
        ]));
        let fixture = fixture(30).await;
        let client = DuckDuckGoClient::for_test(
            transport.clone(),
            "https://duckduckgo.test/html/".to_string(),
            fixture.service.clone(),
        );

        let results = client
            .search("rust", 1, 10)
            .await
            .unwrap_or_else(|error| panic!("search failed: {error}"));

        assert_eq!(results.len(), 1);
        assert_eq!(transport.calls(), 2);
    }

    #[tokio::test(start_paused = true)]
    async fn request_timeout_covers_waiting_in_the_shared_queue() -> anyhow::Result<()> {
        let transport = Arc::new(FakeTransport::new(vec![
            response(StatusCode::OK, success_html(), None),
            response(StatusCode::OK, success_html(), None),
        ]));
        let fixture = fixture(1).await;
        let client = DuckDuckGoClient::for_test(
            transport.clone(),
            "https://duckduckgo.test/html/".to_string(),
            fixture.service.clone(),
        );
        let first = client.search("first", 1, 10).await?;
        assert_eq!(first.len(), 1);

        let started = tokio::time::Instant::now();
        let second = client.search("second", 1, 10);
        tokio::pin!(second);
        tokio::select! {
            result = &mut second => panic!("second search finished before its deadline: {result:?}"),
            () = tokio::time::sleep(Duration::from_millis(100)) => {},
        }
        // Hold the paused clock while the configuration write performs filesystem I/O.
        let (release_clock, hold_clock) = std::sync::mpsc::channel::<()>();
        let clock_guard = tokio::task::spawn_blocking(move || {
            let _ = hold_clock.recv();
        });
        let updated = fixture
            .service
            .update(|config| {
                config.tools.duckduckgo.request_timeout_secs = 30;
                Ok(())
            })
            .await;
        drop(release_clock);
        clock_guard.await?;
        updated?;
        let error = match second.await {
            Ok(_) => panic!("second search should time out in the queue"),
            Err(error) => error,
        };
        assert_eq!(started.elapsed(), Duration::from_secs(1));
        assert_eq!(
            error.to_string(),
            "duckduckgo_search timed out after 1 second(s) while waiting for the shared request queue, retry/backoff, and HTTP response"
        );
        assert_eq!(transport.calls(), 1);
        let third = client.search("third", 1, 10).await?;
        assert_eq!(third.len(), 1);
        assert_eq!(transport.calls(), 2);
        assert_eq!(started.elapsed(), BASE_INTERVAL);
        Ok(())
    }

    #[tokio::test(start_paused = true)]
    async fn retries_blocked_success_response_and_preserves_final_error() {
        let transport = Arc::new(FakeTransport::new(vec![
            response(
                StatusCode::OK,
                format!(
                    r#"<div class="captcha">Challenge</div>{}"#,
                    " ".repeat(1_000)
                ),
                None,
            ),
            response(
                StatusCode::OK,
                format!(
                    r#"<div class="anomaly-modal">Challenge</div>{}"#,
                    " ".repeat(1_000)
                ),
                None,
            ),
        ]));
        let fixture = fixture(30).await;
        let client = DuckDuckGoClient::for_test(
            transport.clone(),
            "https://duckduckgo.test/html/".to_string(),
            fixture.service.clone(),
        );

        let error = match client.search("rust", 1, 10).await {
            Ok(_) => panic!("blocked response should fail"),
            Err(error) => error,
        };

        assert_eq!(
            error.to_string(),
            "Request limit exceeded, try other tool for search"
        );
        assert_eq!(transport.calls(), 2);
    }

    #[tokio::test(start_paused = true)]
    async fn result_text_does_not_trigger_block_detection() {
        let html = format!(
            r#"<a class="result__snippet">captcha bypass for a blocked account</a><span class="result__url">example.com</span><a class="result__a" href="https://example.com">Blocked account guide</a>{}"#,
            " ".repeat(1_000)
        );
        let transport = Arc::new(FakeTransport::new(vec![response(
            StatusCode::OK,
            html,
            None,
        )]));
        let fixture = fixture(30).await;
        let client = DuckDuckGoClient::for_test(
            transport.clone(),
            "https://duckduckgo.test/html/".to_string(),
            fixture.service.clone(),
        );

        let results = client
            .search("captcha bypass blocked account", 1, 10)
            .await
            .unwrap_or_else(|error| panic!("search failed: {error}"));

        assert_eq!(results.len(), 1);
        assert_eq!(transport.calls(), 1);
    }
}
