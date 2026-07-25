//! HTTP/WebSocket transport layer for the chelix gateway.
//!
//! This crate provides the HTTP server, WebSocket upgrade handler,
//! authentication middleware, and all HTTP route handlers. It depends
//! on `chelix-gateway` for core business logic but never the reverse.
//!
//! Non-HTTP consumers (TUI, tests) can depend on `chelix-gateway`
//! directly without pulling in the HTTP stack.

pub mod auth_middleware;
pub mod auth_routes;
pub mod channel_webhook_middleware;
pub mod data_routes;
pub mod env_routes;
pub mod error;
pub mod login_guard;
pub mod request_throttle;
pub mod server;
pub mod ssh_routes;
pub mod tools_routes;
pub mod upload_routes;
pub mod ws;

pub use error::Error;

#[cfg(feature = "graphql")]
pub mod graphql_routes;
#[cfg(feature = "metrics")]
pub mod metrics_middleware;
#[cfg(feature = "metrics")]
pub mod metrics_routes;
#[cfg(feature = "push-notifications")]
pub mod push_routes;

// Re-export key types for consumers.
#[cfg(feature = "tls")]
pub use chelix_tls as tls;
pub use server::{AppState, PreparedGateway, RouteEnhancer, prepare_httpd_embedded, start_gateway};
