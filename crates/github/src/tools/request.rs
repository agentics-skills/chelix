//! Shared GitHub request execution for tool endpoints.

use crate::{
    client::{GitHubClient, GitHubResponse, RequestOptions},
    error::Result,
};

/// Execute one GET request and retry it once when GitHub supplies a usable
/// rate-limit cooldown.
pub(super) async fn get_with_rate_limit_retry(
    client: &GitHubClient,
    url: &url::Url,
    tool_name: &str,
) -> Result<GitHubResponse> {
    let options = RequestOptions {
        return_rate_limit_response: true,
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
