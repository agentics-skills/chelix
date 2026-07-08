//! Shared type definitions for the HTTP server module.

use std::{path::PathBuf, sync::Arc};

use axum::Router;

use chelix_gateway::{auth_webauthn::SharedWebAuthnRegistry, state::GatewayState};

// ── Shared app state ─────────────────────────────────────────────────────────

#[derive(Clone)]
pub struct AppState {
    pub gateway: Arc<GatewayState>,
    pub methods: Arc<chelix_gateway::methods::MethodRegistry>,
    pub request_throttle: Arc<crate::request_throttle::RequestThrottle>,
    pub webauthn_registry: Option<SharedWebAuthnRegistry>,
    #[cfg(feature = "push-notifications")]
    pub push_service: Option<Arc<chelix_gateway::push::PushService>>,
    #[cfg(feature = "graphql")]
    pub graphql_schema: chelix_graphql::ChelixSchema,
}

/// Function signature for adding extra routes (e.g. web-UI) to the gateway.
pub type RouteEnhancer = fn() -> Router<AppState>;

pub(crate) type GatewayBase = (Router<AppState>, AppState);

// ── Prepared gateway types ───────────────────────────────────────────────────

/// A fully wired gateway (app router + shared state), ready to be served.
///
/// Created by [`prepare_gateway`]. Callers bind their own TCP listener and
/// feed `app` to `axum::serve` (or an equivalent). Background tasks (metrics,
/// MCP health, cron, etc.) are already spawned on the current tokio runtime.
pub struct PreparedGateway {
    /// The composed application router.
    pub app: Router,
    /// Shared gateway state (sessions, services, config, etc.).
    pub state: Arc<GatewayState>,
    /// The port the gateway was configured for.
    pub port: u16,
    /// Metadata collected during setup, used by [`start_gateway`] for the
    /// startup banner. Not relevant for bridge callers.
    pub(crate) banner: BannerMeta,
    /// Network audit buffer for real-time streaming (present when
    /// the `trusted-network` feature is enabled and the proxy is active).
    #[cfg(feature = "trusted-network")]
    pub audit_buffer: Option<chelix_gateway::network_audit::NetworkAuditBuffer>,
    /// Keeps the trusted-network proxy alive for the server's full lifetime.
    /// Dropping this sender closes the watch channel, which is the proxy's
    /// shutdown signal.
    #[cfg(feature = "trusted-network")]
    pub _proxy_shutdown_tx: Option<tokio::sync::watch::Sender<bool>>,
}

/// Internal metadata for the startup banner printed by [`start_gateway`].
pub struct BannerMeta {
    pub provider_summary: String,
    pub mcp_configured_count: usize,
    pub method_count: usize,
    pub sandbox_backend_name: String,
    pub data_dir: PathBuf,
    pub setup_code_display: Option<String>,
    pub webauthn_registry: Option<SharedWebAuthnRegistry>,
    pub browser_for_lifecycle: Arc<dyn chelix_gateway::services::BrowserService>,
    pub browser_tool_for_warmup: Option<Arc<dyn chelix_agents::tool_registry::AgentTool>>,
    pub config: chelix_config::schema::ChelixConfig,
}
