//! LLM provider implementations and registry.

mod client;
pub mod config_helpers;
pub mod error;
pub mod http;
pub mod model_capabilities;
pub mod model_catalogs;
pub mod model_id;
pub mod openai;
pub mod openai_compat;
pub mod registry;
pub mod ws_pool;

#[cfg(test)]
pub mod contract;

pub use client::{init_shared_http_client, shared_http_client};

#[allow(unused_imports)]
pub(crate) use http::{retry_after_ms_from_headers, with_retry_after_marker};
#[allow(unused_imports)]
pub(crate) use model_id::{MODEL_ID_NAMESPACE_SEP, namespaced_model_id, raw_model_id};
pub use {model_capabilities::ModelInfo, registry::ProviderRegistry};
