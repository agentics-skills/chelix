//! HTTP server entry points, middleware stack, and router construction.
//!
//! This module contains the HTTP-specific layer of the chelix gateway:
//! `AppState`, router building, middleware, handlers, and server startup.
//! Core business logic lives in `chelix-gateway`; this crate depends on it
//! but never the reverse.

use std::{net::SocketAddr, path::PathBuf, sync::Arc};

use {
    axum::{
        Router,
        extract::{ConnectInfo, State, WebSocketUpgrade},
        http::StatusCode,
        response::{IntoResponse, Json},
    },
    tracing::{info, warn},
};

use chelix_protocol::TICK_INTERVAL_MS;

use chelix_sessions::session_events::{SessionEvent, SessionEventBus};

use chelix_gateway::{
    auth,
    auth_webauthn::SharedWebAuthnRegistry,
    broadcast::{BroadcastOpts, broadcast, broadcast_tick},
    state::GatewayState,
    update_check::{UPDATE_CHECK_INTERVAL, fetch_update_availability, resolve_releases_url},
};

#[cfg(test)]
use chelix_gateway::methods::MethodRegistry;

use crate::ws::handle_connection;

#[cfg(feature = "tls")]
use chelix_tls::CertManager;

// ── Submodules ───────────────────────────────────────────────────────────────

mod builder;
mod gateway;
mod handlers;
mod middleware;
mod runtime;
mod types;

// ── Re-exports ───────────────────────────────────────────────────────────────
//
// Keep the public API surface identical to the original single-file module.

pub use {
    builder::{build_gateway_app, build_gateway_base, finalize_gateway_app},
    gateway::prepare_gateway,
    runtime::{prepare_httpd_embedded, start_gateway},
    types::*,
};

pub use handlers::is_same_origin;

// Bring submodule internals into scope so that `handlers.rs` and `runtime.rs`
// (which use `use super::*;`) can access peer-module items via glob import.
//
// `handlers::*` — runtime.rs relies on `resolve_outbound_ip`, `tls_runtime_sans`,
//   `startup_bind_line`, `startup_passkey_origin_lines`, `startup_setup_code_lines`.
#[allow(unused_imports)]
use handlers::*;
