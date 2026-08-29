pub mod provider;

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
    pub(crate) requires_single_leading_system_message: bool,
    pub(crate) cache_control_policy: CacheControlPolicy,
    pub(crate) responses_websocket_policy: ResponsesWebSocketPolicy,
}

impl OpenAiProviderCapabilities {
    pub(crate) const DEFAULT: Self = Self {
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

#[derive(Debug, Clone)]
struct OpenAiReasoningMetadata {
    supported_efforts: Vec<chelix_common::ReasoningEffort>,
    summary: Option<chelix_common::ReasoningSummary>,
    include: Option<Vec<chelix_common::ReasoningInclude>>,
}

pub struct OpenAiProvider {
    api_key: secrecy::Secret<String>,
    model: String,
    base_url: String,
    provider_name: String,
    client: &'static reqwest::Client,
    stream_transport: chelix_config::schema::ProviderStreamTransport,
    wire_api: chelix_config::schema::WireApi,
    tool_mode: chelix_config::ToolMode,
    /// Exact configured metadata used to build a selected request state.
    reasoning_metadata: Option<OpenAiReasoningMetadata>,
    /// Complete reasoning state after an effort has been selected.
    reasoning_request: Option<chelix_common::ReasoningRequestState>,
    /// Prompt cache retention policy (used for OpenRouter Anthropic passthrough).
    cache_retention: chelix_config::CacheRetention,
    /// Explicit provider behavior policies. Never inferred from provider name or URL.
    capabilities: OpenAiProviderCapabilities,
}
