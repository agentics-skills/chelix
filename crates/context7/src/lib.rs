//! Context7 tool set for Chelix.
//!
//! Exposes the `context7_*` agent tools backed by one shared HTTP client. API
//! tokens come from `tools.context7.token` in priority order. Every Context7
//! request has the finite deadline configured by
//! `tools.context7.request_timeout_secs`.

mod client;
mod error;
mod metrics;
mod rate_limit;
mod registration;
mod tokens;
mod tools;

pub use {
    client::{CONTEXT7_API_BASE_URL, Context7Client, Context7Response, RequestOptions},
    error::{Error, Result},
    registration::register_tools,
    tools::{Context7GetLibraryDocsTool, Context7ResolveLibraryIdTool},
};
