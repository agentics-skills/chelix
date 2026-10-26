//! DuckDuckGo web search tool for Chelix.

pub mod config_validation;

#[cfg(feature = "runtime")]
mod client;
#[cfg(feature = "runtime")]
mod error;
#[cfg(feature = "runtime")]
mod metrics;
#[cfg(feature = "runtime")]
mod parser;
#[cfg(feature = "runtime")]
mod rate_limit;
#[cfg(feature = "runtime")]
mod registration;
#[cfg(feature = "runtime")]
mod tool;
#[cfg(feature = "runtime")]
mod transport;

#[cfg(feature = "runtime")]
pub use registration::register_tools;
