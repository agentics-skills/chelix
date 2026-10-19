use tracing::{debug, info, warn};

use crate::{auth_webauthn::SharedWebAuthnRegistry, state::GatewayState};

// ── WebAuthn runtime sync ────────────────────────────────────────────────────

/// Register a runtime-discovered host in the WebAuthn registry.
///
/// Returns a user-facing warning when the host is newly registered and
/// existing passkeys may need to be re-added for that hostname.
pub async fn sync_runtime_webauthn_host_and_notice(
    gateway: &GatewayState,
    registry: Option<&SharedWebAuthnRegistry>,
    hostname: Option<&str>,
    origin_override: Option<&str>,
    source: &str,
) -> Option<String> {
    let hostname = hostname?;
    let normalized = crate::auth_webauthn::normalize_host(hostname);
    if normalized.is_empty() {
        return None;
    }

    let registry = registry?;
    if registry.read().await.contains_host(&normalized) {
        return None;
    }

    let origin = if let Some(origin_override) = origin_override {
        origin_override.to_string()
    } else {
        let scheme = if gateway.tls_active {
            "https"
        } else {
            "http"
        };
        format!("{scheme}://{normalized}:{}", gateway.port)
    };

    let origin_url = match webauthn_rs::prelude::Url::parse(&origin) {
        Ok(url) => url,
        Err(error) => {
            warn!(
                host = %normalized,
                origin = %origin,
                %error,
                "invalid runtime WebAuthn origin from {source}"
            );
            return None;
        },
    };
    if !crate::auth_webauthn::is_origin_potentially_trustworthy(&origin_url) {
        debug!(
            host = %normalized,
            origin = %origin,
            "skipping runtime WebAuthn origin that is not potentially trustworthy"
        );
        return None;
    }
    let webauthn = match crate::auth_webauthn::WebAuthnState::new(&normalized, &origin_url, &[]) {
        Ok(webauthn) => webauthn,
        Err(error) => {
            warn!(
                host = %normalized,
                origin = %origin,
                %error,
                "failed to initialize runtime WebAuthn RP from {source}"
            );
            return None;
        },
    };

    {
        let mut reg = registry.write().await;
        if reg.contains_host(&normalized) {
            return None;
        }
        reg.add(normalized.clone(), webauthn);
        info!(
            host = %normalized,
            origin = %origin,
            origins = ?reg.get_all_origins(),
            "WebAuthn RP registered from {source}"
        );
    }

    let has_passkeys = if let Some(store) = gateway.credential_store.as_ref() {
        store.has_passkeys().await.unwrap_or(false)
    } else {
        false
    };

    if has_passkeys {
        gateway.add_passkey_host_update_pending(&normalized).await;
        Some(format!(
            "New host detected ({normalized}). Existing passkeys may not work on this host. Sign in with password, then add a new passkey in Settings > Authentication."
        ))
    } else {
        None
    }
}
