mod catalog;
pub mod provider;

pub use {
    crate::DiscoveredModel,
    catalog::{
        available_models, default_model_catalog, fetch_models_from_api, live_models,
        start_model_discovery,
    },
};

use {crate::ModelCapabilities, chelix_agents::model::ModelMetadata};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CacheControlPolicy {
    None,
    OpenRouterAnthropic,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ResponsesWebSocketPolicy {
    Unsupported,
    OpenAiPlatform,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct OpenAiProviderCapabilities {
    pub(crate) default_strict_tools: bool,
    pub(crate) requires_gemini_tool_call_extra_content: bool,
    pub(crate) default_reasoning_content_on_tool_messages: bool,
    pub(crate) reasoning_content_model_prefixes: &'static [&'static str],
    pub(crate) qwen_models_require_single_leading_system: bool,
    pub(crate) cache_control_policy: CacheControlPolicy,
    pub(crate) responses_websocket_policy: ResponsesWebSocketPolicy,
}

impl OpenAiProviderCapabilities {
    pub(crate) const DEFAULT: Self = Self {
        default_strict_tools: true,
        requires_gemini_tool_call_extra_content: false,
        default_reasoning_content_on_tool_messages: false,
        reasoning_content_model_prefixes: &[],
        qwen_models_require_single_leading_system: false,
        cache_control_policy: CacheControlPolicy::None,
        responses_websocket_policy: ResponsesWebSocketPolicy::Unsupported,
    };
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SystemMessageRewriteStrategy {
    None,
    MergeLeadingSystem,
}

pub struct OpenAiProvider {
    api_key: secrecy::Secret<String>,
    model: String,
    base_url: String,
    provider_name: String,
    client: &'static reqwest::Client,
    stream_transport: chelix_config::schema::ProviderStreamTransport,
    wire_api: chelix_config::schema::WireApi,
    metadata_cache: tokio::sync::OnceCell<ModelMetadata>,
    tool_mode_override: Option<chelix_config::ToolMode>,
    /// Optional reasoning effort level for o-series models.
    reasoning_effort: Option<chelix_agents::model::ReasoningEffort>,
    /// Prompt cache retention policy (used for OpenRouter Anthropic passthrough).
    cache_retention: chelix_config::CacheRetention,
    /// Explicit override for strict tool schema mode. `None` = auto-detect.
    strict_tools_override: Option<bool>,
    /// Explicit override for reasoning_content requirement. `None` = auto-detect.
    reasoning_content_override: Option<bool>,
    /// Explicit provider behavior policies. Never inferred from provider name or URL.
    capabilities: OpenAiProviderCapabilities,
    /// Resolved model capabilities. Never inferred from provider name or URL.
    model_capabilities: ModelCapabilities,
    /// Global per-model context window overrides from `[models.<id>]` config.
    context_window_global: std::collections::HashMap<String, u32>,
    /// Provider-scoped per-model context window overrides from
    /// `[providers.<name>.model_overrides.<id>]` config.
    context_window_provider: std::collections::HashMap<String, u32>,
    /// Context window reported by live model discovery.
    discovered_context_window: Option<u32>,
    /// Optional override for the completion-based probe timeout (seconds).
    /// `None` uses the trait default (30s).
    probe_timeout_secs: Option<u64>,
}
