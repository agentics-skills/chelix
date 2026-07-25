use std::sync::Arc;

use tracing::{debug, info, warn};

use crate::{auth_webauthn::SharedWebAuthnRegistry, state::GatewayState};

// ── Browser warmup ───────────────────────────────────────────────────────────

fn spawn_post_listener_warmups(
    browser_service: Arc<dyn crate::services::BrowserService>,
    browser_tool: Option<Arc<dyn chelix_agents::tool_registry::AgentTool>>,
) {
    // Warm the container CLI OnceLock off the async worker threads.
    tokio::task::spawn_blocking(|| {
        let cli = chelix_tools::sandbox::container_cli();
        debug!(cli, "container CLI detected");
    });

    if !super::helpers::env_flag_enabled("CHELIX_BROWSER_WARMUP") {
        debug!("startup browser warmup disabled (set CHELIX_BROWSER_WARMUP=1 to enable)");
        return;
    }

    tokio::spawn(async move {
        browser_service.warmup().await;
        if let Some(tool) = browser_tool
            && let Err(error) = tool.warmup().await
        {
            warn!(%error, "browser tool warmup failed");
        }
    });
}

/// Start browser warmup after the transport listener is ready.
pub fn start_browser_warmup_after_listener(
    browser_service: Arc<dyn crate::services::BrowserService>,
    browser_tool: Option<Arc<dyn chelix_agents::tool_registry::AgentTool>>,
) {
    spawn_post_listener_warmups(browser_service, browser_tool);
}

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

// ── Feature-gated UI helpers ─────────────────────────────────────────────────

#[cfg(feature = "claude-import")]
pub fn claude_detected_for_ui() -> bool {
    chelix_claude_import::detect::detect().is_some()
}

#[cfg(not(feature = "claude-import"))]
pub fn claude_detected_for_ui() -> bool {
    false
}

#[cfg(feature = "codex-import")]
pub fn codex_detected_for_ui() -> bool {
    chelix_codex_import::detect::detect().is_some()
}

#[cfg(not(feature = "codex-import"))]
pub fn codex_detected_for_ui() -> bool {
    false
}
