use std::sync::Arc;

use {
    chelix_auth::{AuthMode, CredentialStore, ResolvedAuth},
    sqlx::SqlitePool,
};

#[tokio::test]
async fn sync_runtime_webauthn_host_registers_new_origin() {
    let pool = SqlitePool::connect("sqlite::memory:").await.unwrap();
    let credential_store = Arc::new(CredentialStore::new(pool).await.unwrap());
    let mut config = chelix_config::ChelixConfig::default();
    config.sandbox.mode = chelix_config::schema::SandboxMode::Off;
    let gateway = crate::state::GatewayState::with_options(
        ResolvedAuth {
            mode: AuthMode::Token,
            token: None,
            password: None,
        },
        crate::services::GatewayServices::noop(),
        config,
        Arc::new(chelix_tools::sandbox::SandboxRouter::disabled()),
        Some(Arc::clone(&credential_store)),
        None,
        false,
        false,
        false,
        None,
        None,
        Arc::new(chelix_code_index::CodeIndex::config_only(
            chelix_code_index::CodeIndexConfig::default(),
        )),
        18789,
        false,
        None,
        None,
        #[cfg(feature = "metrics")]
        None,
        #[cfg(feature = "metrics")]
        None,
        #[cfg(feature = "vault")]
        None,
    );
    let registry = Arc::new(tokio::sync::RwLock::new(
        crate::auth_webauthn::WebAuthnRegistry::new(),
    ));

    let notice = crate::server::startup::sync_runtime_webauthn_host_and_notice(
        &gateway,
        Some(&registry),
        Some("gateway.example.com"),
        Some("https://gateway.example.com"),
        "test",
    )
    .await;

    assert!(notice.is_none(), "unexpected notice: {notice:?}");
    assert!(registry.read().await.contains_host("gateway.example.com"));
    assert!(
        gateway.passkey_host_update_pending().await.is_empty(),
        "passkey warning should not be queued without existing passkeys"
    );
}

#[tokio::test]
async fn sync_runtime_webauthn_host_rejects_invalid_origin() {
    let gateway = crate::state::GatewayState::new(
        ResolvedAuth {
            mode: AuthMode::Token,
            token: None,
            password: None,
        },
        crate::services::GatewayServices::noop(),
    );
    let registry = Arc::new(tokio::sync::RwLock::new(
        crate::auth_webauthn::WebAuthnRegistry::new(),
    ));

    let notice = crate::server::startup::sync_runtime_webauthn_host_and_notice(
        &gateway,
        Some(&registry),
        Some("gateway.example.com"),
        Some("not a url"),
        "test",
    )
    .await;

    assert!(notice.is_none());
    assert!(!registry.read().await.contains_host("gateway.example.com"));
}

#[tokio::test]
async fn sync_runtime_webauthn_host_skips_insecure_http_origin() {
    let gateway = crate::state::GatewayState::new(
        ResolvedAuth {
            mode: AuthMode::Token,
            token: None,
            password: None,
        },
        crate::services::GatewayServices::noop(),
    );
    let registry = Arc::new(tokio::sync::RwLock::new(
        crate::auth_webauthn::WebAuthnRegistry::new(),
    ));

    let notice = crate::server::startup::sync_runtime_webauthn_host_and_notice(
        &gateway,
        Some(&registry),
        Some("chelix.local"),
        None,
        "test",
    )
    .await;

    assert!(notice.is_none());
    assert!(!registry.read().await.contains_host("chelix.local"));
}
