//! LLM provider implementations and registry.

pub mod anthropic;
pub mod async_openai_provider;
mod client;
pub mod config_helpers;
pub mod discovered_model;
pub mod error;
#[cfg(feature = "provider-genai")]
pub mod genai_provider;
#[cfg(feature = "provider-github-copilot")]
pub mod github_copilot;
pub mod http;
#[cfg(feature = "provider-kimi-code")]
pub mod kimi_code;
pub mod model_capabilities;
pub mod model_catalogs;
pub mod model_id;
pub mod openai;
#[cfg(feature = "provider-openai-codex")]
pub mod openai_codex;
#[cfg(feature = "provider-openai-codex")]
pub mod openai_codex_image;
pub mod openai_compat;
pub mod registry;
pub mod ws_pool;

#[cfg(test)]
pub mod contract;

pub use client::{init_shared_http_client, shared_http_client};

#[allow(unused_imports)]
pub(crate) use config_helpers::{
    configured_models_for_provider, env_value, normalize_unique_models, oauth_discovery_enabled,
    resolve_api_key, should_fetch_models, subscription_preference_rank,
};
#[allow(unused_imports)]
pub(crate) use http::{retry_after_ms_from_headers, with_retry_after_marker};
#[allow(unused_imports)]
pub(crate) use model_id::{MODEL_ID_NAMESPACE_SEP, namespaced_model_id, raw_model_id};
pub use {
    discovered_model::{DiscoveredModel, ResolvedModel, resolve_models},
    model_capabilities::ModelInfo,
    registry::{DiscoveryResult, ProviderRegistry, discover_models},
};
