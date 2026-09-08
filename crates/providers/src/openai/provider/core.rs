use std::pin::Pin;

use {
    chelix_common::{
        ModelMetadata, ReasoningEffort, ReasoningPolicyDecision, ReasoningRequestState,
        resolve_reasoning_policy,
    },
    chelix_config::schema::{ProviderStreamTransport, WireApi},
    secrecy::ExposeSecret,
    tokio_stream::Stream,
};

use chelix_agents::model::{ChatMessage, CompletionOptions, LlmProvider, StreamEvent, ToolChoice};

use super::super::{OpenAiProvider, OpenAiProviderCapabilities, OpenAiReasoningMetadata};

impl OpenAiProvider {
    pub fn new(api_key: secrecy::Secret<String>, model: String, base_url: String) -> Self {
        Self::new_with_name(api_key, model, base_url, "openai".into()).with_capabilities(
            OpenAiProviderCapabilities {
                responses_websocket_policy: super::super::ResponsesWebSocketPolicy::OpenAiPlatform,
                ..OpenAiProviderCapabilities::DEFAULT
            },
        )
    }

    pub fn new_with_name(
        api_key: secrecy::Secret<String>,
        model: String,
        base_url: String,
        provider_name: String,
    ) -> Self {
        Self {
            api_key,
            model,
            base_url,
            provider_name,
            client: crate::shared_http_client(),
            stream_transport: ProviderStreamTransport::Sse,
            wire_api: WireApi::ChatCompletions,
            tool_mode: chelix_config::ToolMode::default(),
            reasoning_metadata: None,
            reasoning_request: None,
            cache_retention: chelix_config::CacheRetention::Short,
            capabilities: OpenAiProviderCapabilities::DEFAULT,
        }
    }

    #[must_use]
    pub(crate) fn with_capabilities(mut self, capabilities: OpenAiProviderCapabilities) -> Self {
        self.capabilities = capabilities;
        self
    }

    #[must_use]
    pub fn with_cache_retention(mut self, cache_retention: chelix_config::CacheRetention) -> Self {
        self.cache_retention = cache_retention;
        self
    }

    #[must_use]
    pub fn with_stream_transport(mut self, stream_transport: ProviderStreamTransport) -> Self {
        self.stream_transport = stream_transport;
        self
    }

    #[must_use]
    pub fn with_tool_mode(mut self, mode: chelix_config::ToolMode) -> Self {
        self.tool_mode = mode;
        self
    }

    #[must_use]
    pub fn with_wire_api(mut self, wire_api: WireApi) -> Self {
        self.wire_api = wire_api;
        self
    }

    /// Apply the fully resolved per-model reasoning metadata.
    #[must_use]
    pub fn with_reasoning_metadata(mut self, metadata: &ModelMetadata) -> Self {
        self.reasoning_metadata = Some(OpenAiReasoningMetadata {
            supported_efforts: metadata.reasoning_supported_efforts.clone(),
            summary: metadata.reasoning_summary,
            include: metadata.reasoning_include.clone(),
        });
        self.reasoning_request = None;
        self
    }

    fn with_selected_reasoning_effort(mut self, effort: ReasoningEffort) -> Option<Self> {
        let metadata = self.reasoning_metadata.as_ref()?;
        if !metadata.supported_efforts.contains(&effort) {
            return None;
        }
        self.reasoning_request = Some(ReasoningRequestState::new(
            effort,
            metadata.supported_efforts.clone(),
            metadata.summary,
            metadata.include.clone(),
        ));
        Some(self)
    }

    /// Create a copy of this provider.
    ///
    /// Centralises the field-by-field copy so callers like
    /// `with_reasoning_effort` stay in sync when new fields are added.
    fn fork(&self) -> Self {
        Self {
            api_key: self.api_key.clone(),
            model: self.model.clone(),
            base_url: self.base_url.clone(),
            provider_name: self.provider_name.clone(),
            client: self.client,
            stream_transport: self.stream_transport,
            wire_api: self.wire_api,
            tool_mode: self.tool_mode,
            reasoning_metadata: self.reasoning_metadata.clone(),
            reasoning_request: self.reasoning_request.clone(),
            cache_retention: self.cache_retention,
            capabilities: self.capabilities,
        }
    }

    pub(crate) async fn send_chat_completions_request(
        &self,
        body: &serde_json::Value,
    ) -> reqwest::Result<reqwest::Response> {
        let url = self.chat_completions_url();
        self.client
            .post(&url)
            .header("Authorization", self.bearer_auth_header())
            .header("content-type", "application/json")
            .json(body)
            .send()
            .await
    }

    pub(crate) fn chat_completions_url(&self) -> String {
        format!(
            "{}/chat/completions",
            self.base_url.trim().trim_end_matches('/')
        )
    }

    pub(crate) fn bearer_auth_header(&self) -> String {
        format!("Bearer {}", self.api_key.expose_secret().trim())
    }

    pub(crate) fn selected_reasoning_effort(&self) -> Option<&ReasoningEffort> {
        self.reasoning_request
            .as_ref()
            .map(ReasoningRequestState::selected_effort)
    }

    fn reasoning_policy(&self) -> anyhow::Result<ReasoningPolicyDecision<'_>> {
        let state = self.reasoning_request.as_ref().ok_or_else(|| {
            anyhow::anyhow!(
                "provider `{}` model `{}` requires a selected reasoning effort and model metadata",
                self.provider_name,
                self.model,
            )
        })?;
        Ok(resolve_reasoning_policy(state))
    }

    /// Apply `reasoning_effort` to the Chat Completions streaming request.
    ///
    /// Format: `"reasoning_effort": "high"` (top-level string field).
    pub(crate) fn apply_reasoning_effort_chat(
        &self,
        body: &mut serde_json::Value,
    ) -> anyhow::Result<()> {
        if let ReasoningPolicyDecision::Send { effort, .. } = self.reasoning_policy()? {
            body["reasoning_effort"] = serde_json::json!(effort.as_str());
        }
        Ok(())
    }

    /// Apply the resolved reasoning options for the Responses API only.
    pub(crate) fn apply_reasoning_responses(
        &self,
        body: &mut serde_json::Value,
    ) -> anyhow::Result<()> {
        let ReasoningPolicyDecision::Send {
            effort,
            summary,
            include,
        } = self.reasoning_policy()?
        else {
            return Ok(());
        };

        let mut reasoning = serde_json::Map::new();
        reasoning.insert("effort".to_string(), serde_json::json!(effort.as_str()));
        if let Some(summary) = summary {
            reasoning.insert("summary".to_string(), serde_json::json!(summary.as_str()));
        }
        body["reasoning"] = serde_json::Value::Object(reasoning);
        if let Some(include) = include {
            body["include"] = serde_json::Value::Array(
                include
                    .iter()
                    .map(|value| serde_json::json!(value.as_str()))
                    .collect(),
            );
        }
        Ok(())
    }

    /// Build the HTTP URL for the Responses API (`/responses`).
    ///
    /// If the base URL already ends with `/responses`, use it as-is.
    /// Otherwise derive it as a sibling of `/chat/completions`, ensuring
    /// `/v1` is present — matching the normalization in
    /// `responses_websocket_url`.
    pub(crate) fn responses_sse_url(&self) -> String {
        let base = self.base_url.trim().trim_end_matches('/');
        if base.ends_with("/responses") {
            return base.to_string();
        }
        if let Some(prefix) = base.strip_suffix("/chat/completions") {
            return format!("{prefix}/responses");
        }
        // Ensure /v1 is present, consistent with responses_websocket_url.
        if base.ends_with("/v1") {
            format!("{base}/responses")
        } else {
            format!("{base}/v1/responses")
        }
    }
}

impl LlmProvider for OpenAiProvider {
    fn name(&self) -> &str {
        &self.provider_name
    }

    fn reasoning_effort(&self) -> Option<ReasoningEffort> {
        self.selected_reasoning_effort().cloned()
    }

    fn with_reasoning_effort(
        self: std::sync::Arc<Self>,
        effort: ReasoningEffort,
    ) -> Option<std::sync::Arc<dyn LlmProvider>> {
        let forked = self.fork().with_selected_reasoning_effort(effort)?;
        Some(std::sync::Arc::new(forked))
    }

    fn id(&self) -> &str {
        &self.model
    }

    fn supports_tools(&self) -> bool {
        matches!(self.tool_mode, chelix_config::ToolMode::Native)
    }

    fn tool_mode(&self) -> chelix_config::ToolMode {
        self.tool_mode
    }

    #[allow(clippy::collapsible_if)]
    fn stream(
        &self,
        messages: Vec<ChatMessage>,
    ) -> Pin<Box<dyn Stream<Item = StreamEvent> + Send + '_>> {
        self.stream_with_tools(messages, vec![])
    }

    #[allow(clippy::collapsible_if)]
    fn stream_with_tools(
        &self,
        messages: Vec<ChatMessage>,
        tools: Vec<serde_json::Value>,
    ) -> Pin<Box<dyn Stream<Item = StreamEvent> + Send + '_>> {
        self.stream_with_tools_and_options(messages, tools, CompletionOptions::default())
    }

    fn stream_with_tools_and_options(
        &self,
        messages: Vec<ChatMessage>,
        tools: Vec<serde_json::Value>,
        options: CompletionOptions,
    ) -> Pin<Box<dyn Stream<Item = StreamEvent> + Send + '_>> {
        match (self.wire_api, self.stream_transport) {
            (WireApi::Responses, ProviderStreamTransport::Sse) => {
                self.stream_responses_sse(messages, tools, options)
            },
            (WireApi::Responses, _) => {
                // WebSocket / Auto both go through the WS path which already
                // uses the responses format.
                self.stream_with_tools_websocket(
                    messages,
                    tools,
                    matches!(self.stream_transport, ProviderStreamTransport::Auto),
                    options,
                    true,
                )
            },
            (WireApi::ChatCompletions, ProviderStreamTransport::Sse) => {
                self.stream_with_tools_sse(messages, tools, options)
            },
            (WireApi::ChatCompletions, ProviderStreamTransport::Websocket) => {
                // WebSocket always uses Responses wire format; SSE fallback
                // uses Chat Completions SSE.
                self.stream_with_tools_websocket(messages, tools, false, options, false)
            },
            (WireApi::ChatCompletions, ProviderStreamTransport::Auto) => {
                self.stream_with_tools_websocket(messages, tools, true, options, false)
            },
        }
    }
}

pub(crate) fn apply_openai_responses_tool_choice(
    body: &mut serde_json::Value,
    tool_choice: Option<&ToolChoice>,
) -> anyhow::Result<()> {
    match tool_choice {
        None | Some(ToolChoice::Auto) => {
            if body.get("tools").is_some() {
                body["tool_choice"] = serde_json::json!("auto");
            }
        },
        Some(ToolChoice::Any) => {
            if body.get("tools").is_some() {
                body["tool_choice"] = serde_json::json!("required");
            }
        },
        Some(ToolChoice::None) => {
            if let Some(obj) = body.as_object_mut() {
                obj.remove("tools");
            }
        },
        Some(ToolChoice::Tool { name }) => {
            if name.trim().is_empty() {
                anyhow::bail!("forced OpenAI tool_choice requires a tool name");
            }
            if body.get("tools").is_none() {
                anyhow::bail!("forced OpenAI tool_choice requires at least one active tool");
            }
            body["tool_choice"] = serde_json::json!({
                "type": "function",
                "name": name,
            });
        },
    }
    Ok(())
}

/// Apply `tool_choice` for the OpenAI Chat Completions wire format.
///
/// The Chat Completions API uses `{"type": "function", "function": {"name": "..."}}`
/// instead of the Responses API's `{"type": "function", "name": "..."}`.
pub(crate) fn apply_openai_chat_tool_choice(
    body: &mut serde_json::Value,
    tool_choice: Option<&ToolChoice>,
) -> anyhow::Result<()> {
    match tool_choice {
        None | Some(ToolChoice::Auto) => {
            // Chat Completions doesn't require an explicit tool_choice for auto.
        },
        Some(ToolChoice::Any) => {
            if body.get("tools").is_some() {
                body["tool_choice"] = serde_json::json!("required");
            }
        },
        Some(ToolChoice::None) => {
            if body.get("tools").is_some() {
                body["tool_choice"] = serde_json::json!("none");
            }
        },
        Some(ToolChoice::Tool { name }) => {
            if name.trim().is_empty() {
                anyhow::bail!("forced OpenAI tool_choice requires a tool name");
            }
            if body.get("tools").is_none() {
                anyhow::bail!("forced OpenAI tool_choice requires at least one active tool");
            }
            body["tool_choice"] = serde_json::json!({
                "type": "function",
                "function": { "name": name },
            });
        },
    }
    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
pub(super) mod tests {
    use {
        super::*,
        chelix_common::{ModelModality, ReasoningInclude, ReasoningSummary},
    };

    fn test_provider() -> OpenAiProvider {
        OpenAiProvider::new_with_name(
            secrecy::Secret::new("test-key".to_string()),
            "gpt-5.2".to_string(),
            "https://api.openai.com/v1".to_string(),
            "openai".to_string(),
        )
    }

    pub(crate) fn configure_reasoning(
        provider: OpenAiProvider,
        supported_efforts: Vec<ReasoningEffort>,
        selected_effort: ReasoningEffort,
    ) -> OpenAiProvider {
        provider
            .with_reasoning_metadata(&ModelMetadata {
                context_length: 128_000,
                max_input_tokens: 96_000,
                max_output_tokens: 32_000,
                input_modalities: vec![ModelModality::Text],
                output_modalities: vec![ModelModality::Text],
                tool_calling: true,
                zero_data_retention_enabled: false,
                reasoning_supported_efforts: supported_efforts,
                reasoning_summary: Some(ReasoningSummary::Detailed),
                reasoning_include: Some(vec![ReasoningInclude::EncryptedContent]),
            })
            .with_selected_reasoning_effort(selected_effort)
            .expect("configured effort should be accepted")
    }

    #[test]
    fn reasoning_boundary_requires_complete_request_state() {
        let mut configured = configure_reasoning(test_provider(), vec!["low".into()], "low".into());
        configured.reasoning_request = None;
        for provider in [test_provider(), configured] {
            let error = provider.reasoning_policy().unwrap_err();
            assert!(
                error
                    .to_string()
                    .contains("requires a selected reasoning effort and model metadata")
            );
        }
    }

    #[test]
    fn chat_completions_serializer_encodes_reasoning_policy() {
        for (supported, selected, expected) in [
            (vec!["off".into()], "off", serde_json::json!({})),
            (
                vec!["off".into(), "low".into()],
                "off",
                serde_json::json!({"reasoning_effort": "off"}),
            ),
            (
                vec!["low".into()],
                "low",
                serde_json::json!({"reasoning_effort": "low"}),
            ),
        ] {
            let mut body = serde_json::json!({});
            configure_reasoning(test_provider(), supported, selected.into())
                .apply_reasoning_effort_chat(&mut body)
                .unwrap();
            assert_eq!(body, expected);
        }
    }

    #[test]
    fn responses_serializer_encodes_reasoning_policy() {
        for (supported, selected, expected) in [
            (vec!["off".into()], "off", serde_json::json!({})),
            (
                vec!["off".into(), "low".into()],
                "off",
                serde_json::json!({
                    "reasoning": {"effort": "off", "summary": "detailed"},
                    "include": ["reasoning.encrypted_content"],
                }),
            ),
            (
                vec!["low".into()],
                "low",
                serde_json::json!({
                    "reasoning": {"effort": "low", "summary": "detailed"},
                    "include": ["reasoning.encrypted_content"],
                }),
            ),
        ] {
            let mut body = serde_json::json!({});
            configure_reasoning(test_provider(), supported, selected.into())
                .apply_reasoning_responses(&mut body)
                .unwrap();
            assert_eq!(body, expected);
        }
    }
}
