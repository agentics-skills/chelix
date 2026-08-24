//! Linkup search tool for Chelix.
//!
//! Exposes `linkup_search` through one shared client. The API token comes from
//! `tools.linkup.token`, and each HTTP request has the finite deadline configured
//! by `tools.linkup.request_timeout_secs`.

mod api;
mod client;
mod error;
mod metrics;
mod rate_limit;
mod registration;
mod tool;
mod usage_limit;

pub use {
    client::{LINKUP_API_BASE_URL, LinkupClient, LinkupResponse},
    error::{Error, Result},
    registration::register_tools,
    tool::LinkupSearchTool,
};
