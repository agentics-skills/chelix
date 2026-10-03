pub mod provider;

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
}
