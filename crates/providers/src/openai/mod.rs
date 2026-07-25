mod catalog;
pub mod provider;

pub(crate) use catalog::parse_models_value;
pub use {crate::DiscoveredModel, catalog::fetch_models_from_api};

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
    pub(crate) requires_gemini_tool_call_extra_content: bool,
    pub(crate) default_reasoning_content_on_tool_messages: bool,
    pub(crate) requires_single_leading_system_message: bool,
    pub(crate) cache_control_policy: CacheControlPolicy,
    pub(crate) responses_websocket_policy: ResponsesWebSocketPolicy,
}

impl OpenAiProviderCapabilities {
    pub(crate) const DEFAULT: Self = Self {
        requires_gemini_tool_call_extra_content: false,
        default_reasoning_content_on_tool_messages: false,
        requires_single_leading_system_message: false,
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
    tool_mode_override: Option<chelix_config::ToolMode>,
    /// Optional reasoning effort level for o-series models.
    reasoning_effort: Option<chelix_agents::model::ReasoningEffort>,
    /// Prompt cache retention policy (used for OpenRouter Anthropic passthrough).
    cache_retention: chelix_config::CacheRetention,
    /// Explicit override for reasoning_content requirement. `None` = auto-detect.
    reasoning_content_override: Option<bool>,
    /// Explicit provider behavior policies. Never inferred from provider name or URL.
    capabilities: OpenAiProviderCapabilities,
    /// Optional override for the completion-based probe timeout (seconds).
    /// `None` uses the trait default (30s).
    probe_timeout_secs: Option<u64>,
}
