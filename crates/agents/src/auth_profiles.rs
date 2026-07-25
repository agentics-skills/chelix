/// OAuth + API key credential management with token refresh, stored per-agent.
use secrecy::Secret;

pub struct AuthProfile {
    pub provider: String,
    pub credentials: Credentials,
}

pub enum Credentials {
    ApiKey(Secret<String>),
    OAuth {
        access_token: Secret<String>,
        refresh_token: Option<Secret<String>>,
        expires_at: Option<u64>,
    },
}

/// Refresh credentials if expired.
pub async fn refresh_if_needed(_profile: &mut AuthProfile) -> anyhow::Result<()> {
    todo!("check expiry, call provider token refresh endpoint")
}
