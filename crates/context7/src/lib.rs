//! Context7 tool set for Chelix.
//!
//! Exposes the `context7_*` agent tools backed by one shared HTTP client. API
//! tokens come from `tools.context7.token` in priority order. Every Context7
//! request has the finite deadline configured by
//! `tools.context7.request_timeout_secs`.

pub mod config_validation;
#[cfg(all(test, feature = "runtime"))]
mod test_support;

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
mod tokens;
#[cfg(feature = "runtime")]
mod tools;

#[cfg(feature = "runtime")]
pub use {
    client::{CONTEXT7_API_BASE_URL, Context7Client, Context7Response, RequestOptions},
    error::{Error, Result},
    registration::register_tools,
    tools::{Context7GetLibraryDocsTool, Context7ResolveLibraryIdTool},
};
