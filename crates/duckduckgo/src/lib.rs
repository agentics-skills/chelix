//! DuckDuckGo web search tool for Chelix.

mod client;
mod error;
mod metrics;
mod parser;
mod rate_limit;
mod registration;
mod tool;
mod transport;

pub use registration::register_tools;
