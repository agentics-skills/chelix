//! Linkup search tool for Chelix.
//!
//! Exposes `linkup_search` through one shared client. The API token comes from
//! `tools.linkup.token`, and each HTTP request has the finite deadline configured
//! by `tools.linkup.request_timeout_secs`.

pub mod config_validation;

#[cfg(feature = "runtime")]
mod api;
#[cfg(feature = "runtime")]
mod client;
#[cfg(feature = "runtime")]
mod error;
#[cfg(feature = "runtime")]
mod metrics;
#[cfg(feature = "runtime")]
mod rate_limit;
#[cfg(feature = "runtime")]
mod registration;
#[cfg(feature = "runtime")]
mod tool;
#[cfg(feature = "runtime")]
mod usage_limit;

#[cfg(feature = "runtime")]
pub use {
    client::{LINKUP_API_BASE_URL, LinkupClient, LinkupResponse},
    error::{Error, Result},
    registration::register_tools,
    tool::LinkupSearchTool,
};
