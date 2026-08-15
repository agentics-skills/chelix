//! Shared GitHub request execution for tool endpoints.

use crate::{
    client::{GitHubClient, GitHubResponse, RequestOptions},
    error::Result,
};

/// Execute one GET request with the default media type and retry it once when
/// GitHub supplies a usable rate-limit cooldown.
pub(super) async fn get_with_rate_limit_retry(
    client: &GitHubClient,
    url: &url::Url,
    tool_name: &str,
) -> Result<GitHubResponse> {
    get_with_rate_limit_retry_and_options(client, url, tool_name, RequestOptions::default()).await
}

/// Execute one GET request with explicit options and retry it once when GitHub
/// supplies a usable rate-limit cooldown.
pub(super) async fn get_with_rate_limit_retry_and_options(
    client: &GitHubClient,
    url: &url::Url,
    tool_name: &str,
    options: RequestOptions,
) -> Result<GitHubResponse> {
    let options = RequestOptions {
        return_rate_limit_response: true,
        ..options
    };
    let response = client.get(url, options).await?;
    let Some(cooldown_ms) = response
        .is_rate_limited()
        .then(|| response.rate_limit_cooldown_ms())
        .flatten()
    else {
        return Ok(response);
    };

    #[cfg(feature = "tracing")]
    tracing::warn!(
        tool = tool_name,
        cooldown_ms,
        "GitHub tool hit a rate limit; retrying once through the shared cooldown gate"
    );
    client.get(url, options).await
}
