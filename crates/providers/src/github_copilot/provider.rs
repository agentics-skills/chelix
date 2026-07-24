//! GitHub Copilot provider.
//!
//! Authentication uses the GitHub device-flow OAuth to obtain a GitHub token,
//! then exchanges it for a short-lived Copilot API token via
//! `https://api.github.com/copilot_internal/v2/token`.
//!
//! The Copilot API itself is OpenAI-compatible (`/chat/completions`).

use std::{pin::Pin, time::Duration};

use {
    async_trait::async_trait,
    chelix_config::WireApi,
    chelix_oauth::{OAuthTokens, TokenStore},
    futures::StreamExt,
    secrecy::{ExposeSecret, Secret},
    tokio_stream::Stream,
    tracing::{debug, trace, warn},
};

use {
    super::super::openai_compat::{
        ResponsesSseLineResult, ResponsesStreamState, SseLineResult, StreamingToolState,
        finalize_responses_stream, finalize_stream, parse_openai_compat_usage_from_payload,
        parse_responses_completion, parse_tool_calls, process_openai_sse_line,
        process_responses_sse_line, split_responses_instructions_and_input, to_openai_tools,
        to_responses_api_tools,
    },
    chelix_agents::model::{
        ChatMessage, CompletionOptions, CompletionResponse, LlmProvider, StreamEvent, ToolCall,
        Usage, decode_tool_call_arguments_from_str,
    },
};

// ── Constants ────────────────────────────────────────────────────────────────

/// GitHub OAuth app client ID for Copilot (VS Code's public client ID).
const GITHUB_CLIENT_ID: &str = "Iv1.b507a08c87ecfe98";

const GITHUB_DEVICE_CODE_URL: &str = "https://github.com/login/device/code";
const GITHUB_TOKEN_URL: &str = "https://github.com/login/oauth/access_token";
const COPILOT_TOKEN_URL: &str = "https://api.github.com/copilot_internal/v2/token";
const COPILOT_API_BASE: &str = "https://api.individual.githubcopilot.com";

const PROVIDER_NAME: &str = "github-copilot";

/// Required headers for the Copilot chat completions API.
/// The API rejects requests without `Editor-Version`.
const EDITOR_VERSION: &str = "vscode/1.96.2";
const COPILOT_USER_AGENT: &str = "GitHubCopilotChat/0.26.7";

// ── Device flow types ────────────────────────────────────────────────────────

#[derive(Debug, serde::Deserialize)]
pub struct DeviceCodeResponse {
    pub device_code: String,
    pub user_code: String,
    pub verification_uri: String,
    pub interval: u64,
}

#[derive(Debug, serde::Deserialize)]
struct GithubTokenResponse {
    access_token: Option<String>,
    error: Option<String>,
}

#[derive(serde::Deserialize)]
struct CopilotTokenResponse {
    token: Secret<String>,
    expires_at: u64,
    /// Enterprise accounts return a proxy endpoint hostname (e.g.
    /// `proxy.enterprise.githubcopilot.com`). When present, all API
    /// requests must be routed through `https://{proxy_ep}/…` and chat
    /// completions must use `stream: true`.
    #[serde(rename = "proxy-ep")]
    proxy_ep: Option<String>,
}

impl std::fmt::Debug for CopilotTokenResponse {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CopilotTokenResponse")
            .field("token", &"[REDACTED]")
            .field("expires_at", &self.expires_at)
            .field("proxy_ep", &self.proxy_ep)
            .finish()
    }
}

/// Resolved authentication: a valid Copilot API token plus the base URL to
/// use for API requests (may differ for enterprise vs individual accounts).
struct CopilotAuth {
    token: Secret<String>,
    base_url: String,
    /// `true` when the endpoint is an enterprise proxy that only supports
    /// streaming chat completions.
    is_enterprise: bool,
}

// ── Provider ─────────────────────────────────────────────────────────────────

pub struct GitHubCopilotProvider {
    model: String,
    client: &'static reqwest::Client,
    token_store: TokenStore,
    wire_api: WireApi,
}

impl GitHubCopilotProvider {
    pub fn new(model: String, wire_api: WireApi) -> Self {
        Self {
            model,
            client: crate::shared_http_client(),
            token_store: TokenStore::new(),
            wire_api,
        }
    }

    /// Start the GitHub device-flow: request a device code from GitHub.
    pub async fn request_device_code(
        client: &reqwest::Client,
    ) -> anyhow::Result<DeviceCodeResponse> {
        let resp = client
            .post(GITHUB_DEVICE_CODE_URL)
            .header("Accept", "application/json")
            .form(&[("client_id", GITHUB_CLIENT_ID), ("scope", "")])
            .send()
            .await?;

        if !resp.status().is_success() {
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("GitHub device code request failed: {body}");
        }

        Ok(resp.json().await?)
    }

    /// Poll GitHub for the access token after the user has entered the code.
    pub async fn poll_for_token(
        client: &reqwest::Client,
        device_code: &str,
        interval: u64,
    ) -> anyhow::Result<String> {
        loop {
            tokio::time::sleep(Duration::from_secs(interval)).await;

            let resp = client
                .post(GITHUB_TOKEN_URL)
                .header("Accept", "application/json")
                .form(&[
                    ("client_id", GITHUB_CLIENT_ID),
                    ("device_code", device_code),
                    ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
                ])
                .send()
                .await?;

            let body: GithubTokenResponse = resp.json().await?;

            if let Some(token) = body.access_token {
                return Ok(token);
            }

            match body.error.as_deref() {
                Some("authorization_pending") => continue,
                Some("slow_down") => {
                    tokio::time::sleep(Duration::from_secs(5)).await;
                    continue;
                },
                Some(err) => anyhow::bail!("GitHub device flow error: {err}"),
                None => anyhow::bail!("unexpected response from GitHub token endpoint"),
            }
        }
    }

    /// Get a valid Copilot API token and resolved base URL.
    async fn get_copilot_auth(&self) -> anyhow::Result<CopilotAuth> {
        fetch_copilot_auth_with_fallback(self.client, &self.token_store).await
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use std::{collections::HashMap, sync::Arc};

    use {
        axum::{
            Json, Router,
            body::Body,
            extract::State,
            http::{Uri, header::CONTENT_TYPE},
            response::Response,
            routing::post,
        },
        secrecy::Secret,
        tokio::sync::Mutex,
    };

    use super::*;

    type CapturedBodies = Arc<Mutex<HashMap<String, serde_json::Value>>>;

    async fn capture_request(
        State(captured): State<CapturedBodies>,
        uri: Uri,
        Json(body): Json<serde_json::Value>,
    ) -> Response<Body> {
        let path = uri.path().to_string();
        captured.lock().await.insert(path.clone(), body);

        let response_body = if path.ends_with("/responses") {
            let completed = serde_json::json!({
                "type": "response.completed",
                "response": {
                    "usage": {
                        "input_tokens": 1,
                        "output_tokens": 1,
                        "input_tokens_details": { "cached_tokens": 0 }
                    }
                }
            });
            format!("data: {completed}\n\n")
        } else {
            "data: [DONE]\n\n".to_string()
        };

        Response::builder()
            .header(CONTENT_TYPE, "text/event-stream")
            .body(Body::from(response_body))
            .expect("capture response should build")
    }

    async fn start_capture_server() -> (String, CapturedBodies) {
        let captured = Arc::new(Mutex::new(HashMap::new()));
        let app = Router::new()
            .route("/chat/completions", post(capture_request))
            .route("/responses", post(capture_request))
            .with_state(Arc::clone(&captured));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("capture server should bind");
        let address = listener
            .local_addr()
            .expect("capture server should have an address");
        tokio::spawn(async move {
            axum::serve(listener, app)
                .await
                .expect("capture server should run");
        });
        (format!("http://{address}"), captured)
    }

    #[tokio::test]
    async fn enterprise_requests_preserve_non_strict_tool_schema() {
        let (base_url, captured) = start_capture_server().await;
        let client = reqwest::Client::new();
        let auth = CopilotAuth {
            token: Secret::new("test-token".to_string()),
            base_url,
            is_enterprise: true,
        };
        let messages = [ChatMessage::user("run a command")];
        let tools = [serde_json::json!({
            "name": "execute_command",
            "description": "Execute a command",
            "parameters": {
                "type": "object",
                "properties": {
                    "command": { "type": "string" },
                    "terminalId": { "type": "string" }
                },
                "required": ["command"]
            }
        })];

        collect_streamed_completion(&client, &auth, "test-chat", &messages, &tools, None)
            .await
            .expect("chat completion should succeed");
        collect_streamed_responses_completion(
            &client,
            &auth,
            "test-responses",
            &messages,
            &tools,
            None,
        )
        .await
        .expect("responses completion should succeed");

        let captured = captured.lock().await;
        let chat_tool = &captured["/chat/completions"]["tools"][0]["function"];
        assert_eq!(chat_tool["strict"], false);
        assert_eq!(
            chat_tool["parameters"]["required"],
            serde_json::json!(["command"])
        );
        assert_eq!(
            chat_tool["parameters"]["properties"]["terminalId"]["type"],
            "string"
        );

        let responses_tool = &captured["/responses"]["tools"][0];
        assert_eq!(responses_tool["strict"], false);
        assert_eq!(
            responses_tool["parameters"]["required"],
            serde_json::json!(["command"])
        );
        assert_eq!(
            responses_tool["parameters"]["properties"]["terminalId"]["type"],
            "string"
        );
    }
}

fn home_token_store_if_different() -> Option<TokenStore> {
    let home = chelix_config::user_global_config_dir_if_different()?;
    Some(TokenStore::with_path(home.join("oauth_tokens.json")))
}

fn token_store_with_provider_tokens(primary: &TokenStore) -> Option<TokenStore> {
    debug!("checking primary token store for {PROVIDER_NAME}");
    if primary.load(PROVIDER_NAME).is_some() {
        debug!("found {PROVIDER_NAME} tokens in primary store");
        return Some(primary.clone());
    }
    if let Some(home_store) = home_token_store_if_different() {
        debug!("checking home token store for {PROVIDER_NAME}");
        if home_store.load(PROVIDER_NAME).is_some() {
            debug!("found {PROVIDER_NAME} tokens in home store");
            return Some(home_store);
        }
    }
    debug!("{PROVIDER_NAME} tokens not found in any store");
    None
}

/// Check if we have stored GitHub tokens for Copilot.
pub fn has_stored_tokens() -> bool {
    let found = token_store_with_provider_tokens(&TokenStore::new()).is_some();
    if found {
        debug!("{PROVIDER_NAME} stored tokens found");
    } else {
        debug!("{PROVIDER_NAME} stored tokens not found");
    }
    found
}

/// Build a [`CopilotAuth`] from an `account_id` value that may contain a
/// proxy-ep hostname persisted from a previous token exchange.
fn copilot_auth_from_parts(token: Secret<String>, proxy_ep: Option<&str>) -> CopilotAuth {
    match proxy_ep.filter(|s| !s.is_empty()) {
        Some(ep) => {
            let ep = ep.trim();
            // Reject anything that isn't a plain hostname to prevent SSRF via
            // crafted proxy-ep values (e.g. internal IPs, @-redirects).
            if !ep
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-'))
            {
                warn!(proxy_ep = %ep, "ignoring malformed proxy-ep, falling back to individual endpoint");
                return CopilotAuth {
                    token,
                    base_url: COPILOT_API_BASE.to_string(),
                    is_enterprise: false,
                };
            }
            // Reject bare IP addresses (v4/v6) to prevent SSRF against cloud
            // metadata services, loopback, and RFC-1918 ranges.
            if ep.parse::<std::net::IpAddr>().is_ok() {
                warn!(proxy_ep = %ep, "ignoring IP-address proxy-ep, falling back to individual endpoint");
                return CopilotAuth {
                    token,
                    base_url: COPILOT_API_BASE.to_string(),
                    is_enterprise: false,
                };
            }
            debug!(proxy_ep = %ep, "using enterprise proxy endpoint");
            CopilotAuth {
                token,
                base_url: format!("https://{ep}"),
                is_enterprise: true,
            }
        },
        None => CopilotAuth {
            token,
            base_url: COPILOT_API_BASE.to_string(),
            is_enterprise: false,
        },
    }
}

async fn fetch_copilot_auth(
    client: &reqwest::Client,
    token_store: &TokenStore,
) -> anyhow::Result<CopilotAuth> {
    let tokens = token_store.load(PROVIDER_NAME).ok_or_else(|| {
        anyhow::anyhow!("not logged in to github-copilot — run OAuth device flow first")
    })?;

    // The `access_token` stored is the GitHub user token.
    // We exchange it for a short-lived Copilot API token and cache it.
    // The proxy-ep (if any) is persisted in the `account_id` field.
    if let Some(copilot_tokens) = token_store.load("github-copilot-api")
        && let Some(expires_at) = copilot_tokens.expires_at
    {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        if now + 60 < expires_at {
            let token = copilot_tokens.access_token.clone();
            let proxy_ep = copilot_tokens.account_id.as_deref();
            return Ok(copilot_auth_from_parts(token, proxy_ep));
        }
    }

    let resp = client
        .get(COPILOT_TOKEN_URL)
        .header(
            "Authorization",
            format!("token {}", tokens.access_token.expose_secret()),
        )
        .header("Accept", "application/json")
        .header(
            "User-Agent",
            "chelix/0.1.0 (GitHub Copilot compatible client)",
        )
        .send()
        .await?;

    if !resp.status().is_success() {
        let body = resp.text().await.unwrap_or_default();
        anyhow::bail!("Copilot token exchange failed: {body}");
    }

    let copilot_resp: CopilotTokenResponse = resp.json().await?;
    let _ = token_store.save("github-copilot-api", &OAuthTokens {
        access_token: copilot_resp.token.clone(),
        refresh_token: None,
        id_token: None,
        // NOTE: account_id is repurposed here to persist the enterprise
        // proxy-ep hostname so it can be recovered from the token cache.
        account_id: copilot_resp.proxy_ep.clone(),
        expires_at: Some(copilot_resp.expires_at),
    });

    Ok(copilot_auth_from_parts(
        copilot_resp.token,
        copilot_resp.proxy_ep.as_deref(),
    ))
}

async fn fetch_copilot_auth_with_fallback(
    client: &reqwest::Client,
    primary_store: &TokenStore,
) -> anyhow::Result<CopilotAuth> {
    let Some(token_store) = token_store_with_provider_tokens(primary_store) else {
        anyhow::bail!("not logged in to github-copilot — run OAuth device flow first");
    };
    fetch_copilot_auth(client, &token_store).await
}

fn parse_models_payload(value: &serde_json::Value) -> Vec<super::super::DiscoveredModel> {
    super::super::openai::parse_models_value(value).unwrap_or_default()
}

async fn fetch_models_from_api(
    client: &reqwest::Client,
    auth: &CopilotAuth,
) -> anyhow::Result<Vec<super::super::DiscoveredModel>> {
    let response = client
        .get(format!("{}/models", auth.base_url))
        .header(
            "Authorization",
            format!("Bearer {}", auth.token.expose_secret()),
        )
        .header("Accept", "application/json")
        .header("Editor-Version", EDITOR_VERSION)
        .header("User-Agent", COPILOT_USER_AGENT)
        .send()
        .await?;
    let status = response.status();
    let body = response.text().await?;
    if !status.is_success() {
        anyhow::bail!("copilot models API error HTTP {status}");
    }
    let payload: serde_json::Value = serde_json::from_str(&body)?;
    let models = parse_models_payload(&payload);
    if models.is_empty() {
        anyhow::bail!("copilot models API returned no models");
    }
    Ok(models)
}

/// Fetch current Copilot model records without static fallback catalogs.
pub async fn fetch_models() -> anyhow::Result<Vec<super::super::DiscoveredModel>> {
    let client = super::super::shared_http_client();
    let token_store = TokenStore::new();
    let auth = fetch_copilot_auth_with_fallback(client, &token_store).await?;
    let models = fetch_models_from_api(client, &auth).await?;
    debug!(
        model_count = models.len(),
        "loaded github-copilot live models"
    );
    Ok(models)
}

// ── Enterprise streaming-to-sync bridge ──────────────────────────────────────

/// Send a streaming chat completion request and collect the SSE events into a
/// single [`CompletionResponse`].  Used for enterprise proxy endpoints that
/// reject non-streaming requests.
async fn collect_streamed_completion(
    client: &reqwest::Client,
    auth: &CopilotAuth,
    model: &str,
    messages: &[ChatMessage],
    tools: &[serde_json::Value],
    max_output_tokens: Option<u32>,
) -> anyhow::Result<CompletionResponse> {
    let openai_messages: Vec<serde_json::Value> =
        messages.iter().map(ChatMessage::to_openai_value).collect();
    let mut body = serde_json::json!({
        "model": model,
        "messages": openai_messages,
        "stream": true,
        "stream_options": { "include_usage": true },
    });

    if !tools.is_empty() {
        body["tools"] = serde_json::Value::Array(to_openai_tools(tools, false));
    }
    if let Some(max_output_tokens) = max_output_tokens {
        body["max_completion_tokens"] = serde_json::json!(max_output_tokens);
    }

    debug!(
        model = %model,
        messages_count = messages.len(),
        tools_count = tools.len(),
        "github-copilot enterprise complete (streaming) request"
    );
    trace!(body = %serde_json::to_string(&body).unwrap_or_default(), "github-copilot enterprise request body");

    let http_resp = client
        .post(format!("{}/chat/completions", auth.base_url))
        .header(
            "Authorization",
            format!("Bearer {}", auth.token.expose_secret()),
        )
        .header("content-type", "application/json")
        .header("Editor-Version", EDITOR_VERSION)
        .header("User-Agent", COPILOT_USER_AGENT)
        .json(&body)
        .send()
        .await?;

    let status = http_resp.status();
    if !status.is_success() {
        let retry_after_ms = super::super::retry_after_ms_from_headers(http_resp.headers());
        let body_text = http_resp.text().await.unwrap_or_default();
        warn!(status = %status, body = %body_text, "github-copilot enterprise API error");
        anyhow::bail!(
            "{}",
            super::super::with_retry_after_marker(
                format!("GitHub Copilot API error HTTP {status}: {body_text}"),
                retry_after_ms,
            )
        );
    }

    // Parse the SSE stream into events, then assemble a CompletionResponse.
    let mut byte_stream = http_resp.bytes_stream();
    let mut buf = String::new();
    let mut state = StreamingToolState::default();
    let mut events: Vec<StreamEvent> = Vec::new();

    while let Some(chunk) = byte_stream.next().await {
        let chunk = chunk?;
        buf.push_str(&String::from_utf8_lossy(&chunk));

        let mut offset = 0usize;
        while let Some(pos) = buf[offset..].find('\n') {
            let pos = offset + pos;
            let line = buf[offset..pos].trim();
            offset = pos + 1;

            if line.is_empty() {
                continue;
            }
            let Some(data) = line
                .strip_prefix("data: ")
                .or_else(|| line.strip_prefix("data:"))
            else {
                continue;
            };

            match process_openai_sse_line(data, &mut state) {
                SseLineResult::Done => {
                    extend_events_or_error(&mut events, finalize_stream(&mut state))?;
                    return Ok(stream_events_to_completion(events));
                },
                SseLineResult::Events(new_events) => {
                    extend_events_or_error(&mut events, new_events)?;
                },
                SseLineResult::Skip => {},
            }
        }
        if offset > 0 {
            buf.drain(..offset);
        }
    }

    // Process any trailing data in the buffer.
    let line = buf.trim();
    if !line.is_empty()
        && let Some(data) = line
            .strip_prefix("data: ")
            .or_else(|| line.strip_prefix("data:"))
    {
        match process_openai_sse_line(data, &mut state) {
            SseLineResult::Done => {
                extend_events_or_error(&mut events, finalize_stream(&mut state))?;
                return Ok(stream_events_to_completion(events));
            },
            SseLineResult::Events(new_events) => {
                extend_events_or_error(&mut events, new_events)?;
            },
            SseLineResult::Skip => {},
        }
    }
    extend_events_or_error(&mut events, finalize_stream(&mut state))?;
    Ok(stream_events_to_completion(events))
}

fn extend_events_or_error(
    events: &mut Vec<StreamEvent>,
    new_events: Vec<StreamEvent>,
) -> anyhow::Result<()> {
    for event in new_events {
        if let StreamEvent::Error(msg) = &event {
            anyhow::bail!("{msg}");
        }
        events.push(event);
    }
    Ok(())
}

async fn collect_streamed_responses_completion(
    client: &reqwest::Client,
    auth: &CopilotAuth,
    model: &str,
    messages: &[ChatMessage],
    tools: &[serde_json::Value],
    max_output_tokens: Option<u32>,
) -> anyhow::Result<CompletionResponse> {
    let (instructions, input) = split_responses_instructions_and_input(messages.to_vec());

    let mut body = serde_json::json!({
        "model": model,
        "stream": true,
        "input": input,
    });
    if let Some(instructions) = instructions {
        body["instructions"] = serde_json::Value::String(instructions);
    }
    if !tools.is_empty() {
        body["tools"] = serde_json::Value::Array(to_responses_api_tools(tools));
        body["tool_choice"] = serde_json::json!("auto");
    }
    if let Some(max_output_tokens) = max_output_tokens {
        body["max_output_tokens"] = serde_json::json!(max_output_tokens);
    }

    let http_resp = client
        .post(format!("{}/responses", auth.base_url))
        .header(
            "Authorization",
            format!("Bearer {}", auth.token.expose_secret()),
        )
        .header("content-type", "application/json")
        .header("Editor-Version", EDITOR_VERSION)
        .header("User-Agent", COPILOT_USER_AGENT)
        .json(&body)
        .send()
        .await?;

    let status = http_resp.status();
    if !status.is_success() {
        let retry_after_ms = super::super::retry_after_ms_from_headers(http_resp.headers());
        let body_text = http_resp.text().await.unwrap_or_default();
        warn!(status = %status, body = %body_text, "github-copilot enterprise responses API error");
        anyhow::bail!(
            "{}",
            super::super::with_retry_after_marker(
                format!("GitHub Copilot Responses API error HTTP {status}: {body_text}"),
                retry_after_ms,
            )
        );
    }

    let mut byte_stream = http_resp.bytes_stream();
    let mut buf = String::new();
    let mut state = ResponsesStreamState::default();
    let mut events: Vec<StreamEvent> = Vec::new();

    while let Some(chunk) = byte_stream.next().await {
        let chunk = chunk?;
        buf.push_str(&String::from_utf8_lossy(&chunk));

        let mut offset = 0usize;
        while let Some(pos) = buf[offset..].find('\n') {
            let pos = offset + pos;
            let line = buf[offset..pos].trim();
            offset = pos + 1;

            if line.is_empty() {
                continue;
            }

            let Some(data) = line
                .strip_prefix("data: ")
                .or_else(|| line.strip_prefix("data:"))
            else {
                continue;
            };

            match process_responses_sse_line(data, &mut state) {
                ResponsesSseLineResult::Completed(new_events) => {
                    extend_events_or_error(&mut events, new_events)?;
                    extend_events_or_error(&mut events, finalize_responses_stream(&mut state))?;
                    return Ok(stream_events_to_completion(events));
                },
                ResponsesSseLineResult::Failed(new_events) => {
                    extend_events_or_error(&mut events, new_events)?;
                    anyhow::bail!("Responses API stream failed without an error event");
                },
                ResponsesSseLineResult::Events(new_events) => {
                    extend_events_or_error(&mut events, new_events)?;
                },
                ResponsesSseLineResult::Skip => {},
            }
        }
        if offset > 0 {
            buf.drain(..offset);
        }
    }

    // Process any trailing data in the buffer.
    let line = buf.trim();
    if !line.is_empty()
        && let Some(data) = line
            .strip_prefix("data: ")
            .or_else(|| line.strip_prefix("data:"))
    {
        match process_responses_sse_line(data, &mut state) {
            ResponsesSseLineResult::Completed(new_events) => {
                extend_events_or_error(&mut events, new_events)?;
                extend_events_or_error(&mut events, finalize_responses_stream(&mut state))?;
                return Ok(stream_events_to_completion(events));
            },
            ResponsesSseLineResult::Failed(new_events) => {
                extend_events_or_error(&mut events, new_events)?;
                anyhow::bail!("Responses API stream failed without an error event");
            },
            ResponsesSseLineResult::Events(new_events) => {
                extend_events_or_error(&mut events, new_events)?;
            },
            ResponsesSseLineResult::Skip => {},
        }
    }

    extend_events_or_error(&mut events, finalize_responses_stream(&mut state))?;
    Ok(stream_events_to_completion(events))
}

/// Collapse a collected list of [`StreamEvent`]s into a [`CompletionResponse`].
fn stream_events_to_completion(events: Vec<StreamEvent>) -> CompletionResponse {
    let mut text_parts: Vec<String> = Vec::new();
    let mut tool_calls: Vec<ToolCall> = Vec::new();
    let mut usage = Usage::default();

    // Track in-progress tool calls by index.
    let mut pending_tools: Vec<(String, String, String)> = Vec::new(); // (id, name, args)

    for event in events {
        match event {
            StreamEvent::Delta(s) => text_parts.push(s),
            StreamEvent::ToolCallStart {
                id, name, index, ..
            } => {
                while pending_tools.len() <= index {
                    pending_tools.push((String::new(), String::new(), String::new()));
                }
                pending_tools[index].0 = id;
                pending_tools[index].1 = name;
            },
            StreamEvent::ToolCallArgumentsDelta { index, delta } => {
                if let Some(entry) = pending_tools.get_mut(index) {
                    entry.2.push_str(&delta);
                }
            },
            StreamEvent::ToolCallComplete { index } => {
                if let Some(entry) = pending_tools.get(index) {
                    let decoded = decode_tool_call_arguments_from_str(&entry.2);
                    tool_calls.push(ToolCall {
                        id: entry.0.clone(),
                        name: entry.1.clone(),
                        arguments: decoded.arguments,
                        argument_diagnostic: decoded.diagnostic,
                        metadata: None,
                    });
                }
            },
            StreamEvent::Done(u) => usage = u,
            StreamEvent::Error(_)
            | StreamEvent::ProviderRaw(_)
            | StreamEvent::ReasoningDelta(_) => {},
        }
    }

    let text = if text_parts.is_empty() {
        None
    } else {
        Some(text_parts.join(""))
    };

    CompletionResponse {
        text,
        tool_calls,
        usage,
    }
}

// ── Responses API helpers ────────────────────────────────────────────────────

// ── LlmProvider impl ────────────────────────────────────────────────────────

#[async_trait]
impl LlmProvider for GitHubCopilotProvider {
    fn name(&self) -> &str {
        PROVIDER_NAME
    }

    fn id(&self) -> &str {
        &self.model
    }

    fn supports_tools(&self) -> bool {
        false
    }

    async fn complete(
        &self,
        messages: &[ChatMessage],
        tools: &[serde_json::Value],
    ) -> anyhow::Result<CompletionResponse> {
        self.complete_with_options(messages, tools, &CompletionOptions::default())
            .await
    }

    async fn complete_with_options(
        &self,
        messages: &[ChatMessage],
        tools: &[serde_json::Value],
        options: &CompletionOptions,
    ) -> anyhow::Result<CompletionResponse> {
        options.reject_forced_tool_choice(self.name())?;
        if matches!(self.wire_api, WireApi::Responses) {
            return self
                .complete_responses(messages, tools, options.max_output_tokens)
                .await;
        }

        let auth = self.get_copilot_auth().await?;

        // Enterprise proxy only supports streaming — delegate to the
        // streaming path and collect the result.
        if auth.is_enterprise {
            return collect_streamed_completion(
                self.client,
                &auth,
                &self.model,
                messages,
                tools,
                options.max_output_tokens,
            )
            .await;
        }

        let openai_messages: Vec<serde_json::Value> =
            messages.iter().map(ChatMessage::to_openai_value).collect();
        let mut body = serde_json::json!({
            "model": self.model,
            "messages": openai_messages,
        });

        if !tools.is_empty() {
            body["tools"] = serde_json::Value::Array(to_openai_tools(tools, false));
        }
        if let Some(max_output_tokens) = options.max_output_tokens {
            body["max_completion_tokens"] = serde_json::json!(max_output_tokens);
        }

        debug!(
            model = %self.model,
            messages_count = messages.len(),
            tools_count = tools.len(),
            "github-copilot complete request"
        );
        trace!(body = %serde_json::to_string(&body).unwrap_or_default(), "github-copilot request body");

        let http_resp = self
            .client
            .post(format!("{}/chat/completions", auth.base_url))
            .header(
                "Authorization",
                format!("Bearer {}", auth.token.expose_secret()),
            )
            .header("content-type", "application/json")
            .header("Editor-Version", EDITOR_VERSION)
            .header("User-Agent", COPILOT_USER_AGENT)
            .json(&body)
            .send()
            .await?;

        let status = http_resp.status();
        if !status.is_success() {
            let retry_after_ms = super::super::retry_after_ms_from_headers(http_resp.headers());
            let body_text = http_resp.text().await.unwrap_or_default();

            warn!(status = %status, body = %body_text, "github-copilot API error");
            anyhow::bail!(
                "{}",
                super::super::with_retry_after_marker(
                    format!("GitHub Copilot API error HTTP {status}: {body_text}"),
                    retry_after_ms,
                )
            );
        }

        let resp = http_resp.json::<serde_json::Value>().await?;
        trace!(response = %resp, "github-copilot raw response");

        let message = &resp["choices"][0]["message"];

        let text = message["content"].as_str().map(|s| s.to_string());
        let tool_calls = parse_tool_calls(message);

        let usage = parse_openai_compat_usage_from_payload(&resp).unwrap_or_default();

        Ok(CompletionResponse {
            text,
            tool_calls,
            usage,
        })
    }

    #[allow(clippy::collapsible_if)]
    fn stream(
        &self,
        messages: Vec<ChatMessage>,
    ) -> Pin<Box<dyn Stream<Item = StreamEvent> + Send + '_>> {
        self.stream_with_tools(messages, vec![])
    }

    #[allow(clippy::collapsible_if)]
    fn stream_with_tools(
        &self,
        messages: Vec<ChatMessage>,
        tools: Vec<serde_json::Value>,
    ) -> Pin<Box<dyn Stream<Item = StreamEvent> + Send + '_>> {
        if matches!(self.wire_api, WireApi::Responses) {
            return self.stream_responses_api(messages, tools);
        }
        self.stream_chat_completions(messages, tools)
    }
}

impl GitHubCopilotProvider {
    /// Non-streaming completion via the Responses API (`/responses`).
    async fn complete_responses(
        &self,
        messages: &[ChatMessage],
        tools: &[serde_json::Value],
        max_output_tokens: Option<u32>,
    ) -> anyhow::Result<CompletionResponse> {
        let auth = self.get_copilot_auth().await?;

        if auth.is_enterprise {
            return collect_streamed_responses_completion(
                self.client,
                &auth,
                &self.model,
                messages,
                tools,
                max_output_tokens,
            )
            .await;
        }

        let (instructions, input) = split_responses_instructions_and_input(messages.to_vec());

        let mut body = serde_json::json!({
            "model": self.model,
            "input": input,
        });
        if let Some(instructions) = instructions {
            body["instructions"] = serde_json::Value::String(instructions);
        }
        if !tools.is_empty() {
            body["tools"] = serde_json::Value::Array(to_responses_api_tools(tools));
            body["tool_choice"] = serde_json::json!("auto");
        }
        if let Some(max_output_tokens) = max_output_tokens {
            body["max_output_tokens"] = serde_json::json!(max_output_tokens);
        }

        debug!(
            model = %self.model,
            messages_count = messages.len(),
            tools_count = tools.len(),
            "github-copilot complete_responses request"
        );
        trace!(body = %serde_json::to_string(&body).unwrap_or_default(), "github-copilot responses request body");

        let http_resp = self
            .client
            .post(format!("{}/responses", auth.base_url))
            .header(
                "Authorization",
                format!("Bearer {}", auth.token.expose_secret()),
            )
            .header("content-type", "application/json")
            .header("Editor-Version", EDITOR_VERSION)
            .header("User-Agent", COPILOT_USER_AGENT)
            .json(&body)
            .send()
            .await?;

        let status = http_resp.status();
        if !status.is_success() {
            let retry_after_ms = super::super::retry_after_ms_from_headers(http_resp.headers());
            let body_text = http_resp.text().await.unwrap_or_default();
            warn!(status = %status, body = %body_text, "github-copilot responses API error");
            anyhow::bail!(
                "{}",
                super::super::with_retry_after_marker(
                    format!("GitHub Copilot Responses API error HTTP {status}: {body_text}"),
                    retry_after_ms,
                )
            );
        }

        let resp = http_resp.json::<serde_json::Value>().await?;
        trace!(response = %resp, "github-copilot responses raw response");

        Ok(parse_responses_completion(&resp))
    }

    /// Streaming via the Responses API (`/responses`) with SSE.
    fn stream_responses_api(
        &self,
        messages: Vec<ChatMessage>,
        tools: Vec<serde_json::Value>,
    ) -> Pin<Box<dyn Stream<Item = StreamEvent> + Send + '_>> {
        Box::pin(async_stream::stream! {
            let auth = match self.get_copilot_auth().await {
                Ok(a) => a,
                Err(e) => {
                    yield StreamEvent::Error(e.to_string());
                    return;
                }
            };

            let (instructions, input) =
                split_responses_instructions_and_input(messages);

            let mut body = serde_json::json!({
                "model": self.model,
                "stream": true,
                "input": input,
            });
            if let Some(instructions) = instructions {
                body["instructions"] = serde_json::Value::String(instructions);
            }
            if !tools.is_empty() {
                body["tools"] = serde_json::Value::Array(to_responses_api_tools(&tools));
                body["tool_choice"] = serde_json::json!("auto");
            }

            debug!(
                model = %self.model,
                tools_count = tools.len(),
                "github-copilot stream_responses_api request"
            );
            trace!(body = %serde_json::to_string(&body).unwrap_or_default(), "github-copilot responses stream request body");

            let resp = match self
                .client
                .post(format!("{}/responses", auth.base_url))
                .header("Authorization", format!("Bearer {}", auth.token.expose_secret()))
                .header("content-type", "application/json")
                .header("Editor-Version", EDITOR_VERSION)
                .header("User-Agent", COPILOT_USER_AGENT)
                .json(&body)
                .send()
                .await
            {
                Ok(r) => {
                    if let Err(e) = r.error_for_status_ref() {
                        let status = e.status().map(|s| s.as_u16()).unwrap_or(0);
                        let retry_after_ms = super::super::retry_after_ms_from_headers(r.headers());
                        let body_text = r.text().await.unwrap_or_default();
                        yield StreamEvent::Error(super::super::with_retry_after_marker(
                            format!("HTTP {status}: {body_text}"),
                            retry_after_ms,
                        ));
                        return;
                    }
                    r
                }
                Err(e) => {
                    yield StreamEvent::Error(e.to_string());
                    return;
                }
            };

            let mut byte_stream = resp.bytes_stream();
            let mut buf = String::new();
            let mut state = ResponsesStreamState::default();

            while let Some(chunk) = byte_stream.next().await {
                let chunk = match chunk {
                    Ok(c) => c,
                    Err(e) => {
                        yield StreamEvent::Error(e.to_string());
                        return;
                    }
                };
                buf.push_str(&String::from_utf8_lossy(&chunk));

                while let Some(pos) = buf.find('\n') {
                    let line = buf[..pos].trim().to_string();
                    buf = buf[pos + 1..].to_string();

                    if line.is_empty() {
                        continue;
                    }

                    let Some(data) = line
                        .strip_prefix("data: ")
                        .or_else(|| line.strip_prefix("data:"))
                    else {
                        continue;
                    };

                    match process_responses_sse_line(data, &mut state) {
                        ResponsesSseLineResult::Completed(events) => {
                            for event in events {
                                yield event;
                            }
                            for event in finalize_responses_stream(&mut state) {
                                yield event;
                            }
                            return;
                        }
                        ResponsesSseLineResult::Failed(events) => {
                            for event in events {
                                yield event;
                            }
                            return;
                        }
                        ResponsesSseLineResult::Events(events) => {
                            for event in events {
                                yield event;
                            }
                        }
                        ResponsesSseLineResult::Skip => {}
                    }
                }
            }

            // Process any remaining data in the buffer.
            let line = buf.trim().to_string();
            if !line.is_empty()
                && let Some(data) = line
                    .strip_prefix("data: ")
                    .or_else(|| line.strip_prefix("data:"))
            {
                match process_responses_sse_line(data, &mut state) {
                    ResponsesSseLineResult::Completed(events) => {
                        for event in events {
                            yield event;
                        }
                        for event in finalize_responses_stream(&mut state) {
                            yield event;
                        }
                        return;
                    }
                    ResponsesSseLineResult::Failed(events) => {
                        for event in events {
                            yield event;
                        }
                        return;
                    }
                    ResponsesSseLineResult::Events(events) => {
                        for event in events {
                            yield event;
                        }
                    }
                    ResponsesSseLineResult::Skip => {}
                }
            }

            for event in finalize_responses_stream(&mut state) {
                yield event;
            }
        })
    }

    /// Streaming via the Chat Completions API (`/chat/completions`) with SSE.
    #[allow(clippy::collapsible_if)]
    fn stream_chat_completions(
        &self,
        messages: Vec<ChatMessage>,
        tools: Vec<serde_json::Value>,
    ) -> Pin<Box<dyn Stream<Item = StreamEvent> + Send + '_>> {
        Box::pin(async_stream::stream! {
            let auth = match self.get_copilot_auth().await {
                Ok(a) => a,
                Err(e) => {
                    yield StreamEvent::Error(e.to_string());
                    return;
                }
            };

            let openai_messages: Vec<serde_json::Value> =
                messages.iter().map(ChatMessage::to_openai_value).collect();
            let mut body = serde_json::json!({
                "model": self.model,
                "messages": openai_messages,
                "stream": true,
                "stream_options": { "include_usage": true },
            });

            if !tools.is_empty() {
                body["tools"] = serde_json::Value::Array(to_openai_tools(&tools, false));
            }

            debug!(
                model = %self.model,
                messages_count = openai_messages.len(),
                tools_count = tools.len(),
                "github-copilot stream_with_tools request"
            );
            trace!(body = %serde_json::to_string(&body).unwrap_or_default(), "github-copilot stream request body");

            let resp = match self
                .client
                .post(format!("{}/chat/completions", auth.base_url))
                .header("Authorization", format!("Bearer {}", auth.token.expose_secret()))
                .header("content-type", "application/json")
                .header("Editor-Version", EDITOR_VERSION)
                .header("User-Agent", COPILOT_USER_AGENT)
                .json(&body)
                .send()
                .await
            {
                Ok(r) => {
                    if let Err(e) = r.error_for_status_ref() {
                        let status = e.status().map(|s| s.as_u16()).unwrap_or(0);
                        let retry_after_ms = super::super::retry_after_ms_from_headers(r.headers());
                        let body_text = r.text().await.unwrap_or_default();

                        yield StreamEvent::Error(super::super::with_retry_after_marker(
                            format!("HTTP {status}: {body_text}"),
                            retry_after_ms,
                        ));
                        return;
                    }
                    r
                }
                Err(e) => {
                    yield StreamEvent::Error(e.to_string());
                    return;
                }
            };

            let mut byte_stream = resp.bytes_stream();
            let mut buf = String::new();
            let mut state = StreamingToolState::default();

            while let Some(chunk) = byte_stream.next().await {
                let chunk = match chunk {
                    Ok(c) => c,
                    Err(e) => {
                        yield StreamEvent::Error(e.to_string());
                        return;
                    }
                };
                buf.push_str(&String::from_utf8_lossy(&chunk));

                while let Some(pos) = buf.find('\n') {
                    let line = buf[..pos].trim().to_string();
                    buf = buf[pos + 1..].to_string();

                    if line.is_empty() {
                        continue;
                    }

                    let Some(data) = line
                        .strip_prefix("data: ")
                        .or_else(|| line.strip_prefix("data:"))
                    else {
                        continue;
                    };

                    match process_openai_sse_line(data, &mut state) {
                        SseLineResult::Done => {
                            for event in finalize_stream(&mut state) {
                                yield event;
                            }
                            return;
                        }
                        SseLineResult::Events(events) => {
                            for event in events {
                                yield event;
                            }
                        }
                        SseLineResult::Skip => {}
                    }
                }
            }

            let line = buf.trim().to_string();
            if !line.is_empty()
                && let Some(data) = line
                    .strip_prefix("data: ")
                    .or_else(|| line.strip_prefix("data:"))
            {
                match process_openai_sse_line(data, &mut state) {
                    SseLineResult::Done => {
                        for event in finalize_stream(&mut state) {
                            yield event;
                        }
                        return;
                    }
                    SseLineResult::Events(events) => {
                        for event in events {
                            yield event;
                        }
                    }
                    SseLineResult::Skip => {}
                }
            }

            for event in finalize_stream(&mut state) {
                yield event;
            }
        })
    }
}
