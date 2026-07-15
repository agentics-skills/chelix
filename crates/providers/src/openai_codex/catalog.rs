use std::time::Duration;

use {
    chelix_oauth::TokenStore,
    secrecy::{ExposeSecret, Secret},
    tracing::{debug, info},
};

use super::OpenAiCodexProvider;

const CODEX_MODELS_ENDPOINT: &str = "https://chatgpt.com/backend-api/codex/models";
/// Report a client version that satisfies the Codex API's
/// `minimal_client_version` filter so all available models are returned.
/// Using the crate's own version (0.x) caused the API to hide newer models
/// that require >= 0.98.0. See <https://github.com/agentics-skills/chelix/issues/354>.
///
/// **DO NOT** change this to `env!("CARGO_PKG_VERSION")` — the crate version
/// is unrelated to the Codex client version and will break model discovery.
pub(super) const CODEX_MODELS_CLIENT_VERSION: &str = "1.0.0";

/// Parse tokens from Codex CLI auth.json content.
pub(super) fn parse_codex_cli_tokens(data: &str) -> Option<chelix_oauth::OAuthTokens> {
    let json: serde_json::Value = serde_json::from_str(data).ok()?;
    let tokens = json.get("tokens")?;
    let access_token = tokens.get("access_token")?.as_str()?.to_string();
    let id_token = tokens
        .get("id_token")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let account_id = tokens
        .get("account_id")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let refresh_token = tokens
        .get("refresh_token")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    Some(chelix_oauth::OAuthTokens {
        access_token: Secret::new(access_token),
        refresh_token: refresh_token.map(Secret::new),
        id_token: id_token.map(Secret::new),
        account_id,
        expires_at: None,
    })
}

/// Try to load tokens from the Codex CLI file at `~/.codex/auth.json`.
pub(super) fn load_codex_cli_tokens() -> Option<chelix_oauth::OAuthTokens> {
    let home = std::env::var("HOME").ok()?;
    let path = std::path::PathBuf::from(home)
        .join(".codex")
        .join("auth.json");
    let data = std::fs::read_to_string(path).ok()?;
    parse_codex_cli_tokens(&data)
}

pub fn has_stored_tokens() -> bool {
    TokenStore::new().load("openai-codex").is_some() || load_codex_cli_tokens().is_some()
}

pub(super) fn parse_models_payload(value: &serde_json::Value) -> Vec<crate::DiscoveredModel> {
    crate::openai::parse_models_value(value).unwrap_or_default()
}

async fn fetch_models_from_api(
    access_token: String,
    account_id: String,
) -> anyhow::Result<Vec<crate::DiscoveredModel>> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(8))
        .build()?;
    let url = format!("{CODEX_MODELS_ENDPOINT}?client_version={CODEX_MODELS_CLIENT_VERSION}");
    let response = client
        .get(url)
        .header("Authorization", format!("Bearer {access_token}"))
        .header("chatgpt-account-id", account_id)
        .header("originator", "pi")
        .header("accept", "application/json")
        .send()
        .await?;
    let status = response.status();
    let body = response.text().await?;
    if !status.is_success() {
        anyhow::bail!("codex models API error HTTP {status}");
    }
    let payload: serde_json::Value = serde_json::from_str(&body)?;
    let models = parse_models_payload(&payload);
    if models.is_empty() {
        anyhow::bail!("codex models API returned no models");
    }
    Ok(models)
}

fn load_access_token_and_account_id() -> anyhow::Result<(String, String)> {
    let tokens = TokenStore::new()
        .load("openai-codex")
        .or_else(load_codex_cli_tokens)
        .ok_or_else(|| {
            debug!("openai-codex tokens not found in token store or codex CLI auth");
            anyhow::anyhow!("openai-codex tokens not found")
        })?;

    let access_token = tokens.access_token.expose_secret().clone();
    let account_id = OpenAiCodexProvider::resolve_account_id(&tokens)?;
    Ok((access_token, account_id))
}

/// Fetch the current Codex model records without static fallback catalogs.
pub async fn fetch_models() -> anyhow::Result<Vec<crate::DiscoveredModel>> {
    let (access_token, account_id) = load_access_token_and_account_id()?;
    let models = fetch_models_from_api(access_token, account_id).await?;
    info!(
        model_count = models.len(),
        "loaded openai-codex live models"
    );
    Ok(models)
}
