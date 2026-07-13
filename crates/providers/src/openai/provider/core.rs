use std::{pin::Pin, time::Duration};

use {
    async_trait::async_trait,
    chelix_config::schema::{ProviderStreamTransport, WireApi},
    secrecy::ExposeSecret,
    tokio_stream::Stream,
};

use tracing::debug;

use crate::{ModelCapabilities, context_window_for_model_with_config};

use chelix_agents::model::{
    AgentToolControls, ChatMessage, CompletionResponse, LlmProvider, ModelMetadata, StreamEvent,
    ToolChoice,
};

use super::super::{OpenAiProvider, OpenAiProviderCapabilities};

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
        let model_capabilities = ModelCapabilities::infer(&model);
        let capabilities = default_capabilities_for_provider(&provider_name);
        Self {
            api_key,
            model,
            base_url,
            provider_name,
            client: crate::shared_http_client(),
            stream_transport: ProviderStreamTransport::Sse,
            wire_api: WireApi::ChatCompletions,
            metadata_cache: tokio::sync::OnceCell::new(),
            tool_mode_override: None,
            reasoning_effort: None,
            cache_retention: chelix_config::CacheRetention::Short,
            strict_tools_override: None,
            reasoning_content_override: None,
            capabilities,
            model_capabilities,
            context_window_global: std::collections::HashMap::new(),
            context_window_provider: std::collections::HashMap::new(),
            discovered_context_window: None,
            probe_timeout_secs: None,
        }
    }

    #[must_use]
    pub(crate) fn with_capabilities(mut self, capabilities: OpenAiProviderCapabilities) -> Self {
        self.capabilities = capabilities;
        self
    }

    #[must_use]
    pub(crate) fn with_model_capabilities(mut self, capabilities: ModelCapabilities) -> Self {
        self.model_capabilities = capabilities;
        self
    }

    #[must_use]
    pub(crate) fn with_discovered_context_window(mut self, context_window: Option<u32>) -> Self {
        self.discovered_context_window = context_window;
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
        self.tool_mode_override = Some(mode);
        self
    }

    #[must_use]
    pub fn with_wire_api(mut self, wire_api: WireApi) -> Self {
        self.wire_api = wire_api;
        self
    }

    #[must_use]
    pub fn with_strict_tools(mut self, strict: bool) -> Self {
        self.strict_tools_override = Some(strict);
        self
    }

    #[must_use]
    pub fn with_reasoning_content(mut self, required: bool) -> Self {
        self.reasoning_content_override = Some(required);
        self
    }

    /// Set the completion-based probe timeout override (seconds).
    #[must_use]
    pub fn with_probe_timeout_secs(mut self, secs: Option<u64>) -> Self {
        self.probe_timeout_secs = secs;
        self
    }

    /// Create a copy of this provider with a fresh metadata cache.
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
            metadata_cache: tokio::sync::OnceCell::new(),
            tool_mode_override: self.tool_mode_override,
            reasoning_effort: self.reasoning_effort,
            cache_retention: self.cache_retention,
            strict_tools_override: self.strict_tools_override,
            reasoning_content_override: self.reasoning_content_override,
            capabilities: self.capabilities,
            model_capabilities: self.model_capabilities,
            context_window_global: self.context_window_global.clone(),
            context_window_provider: self.context_window_provider.clone(),
            discovered_context_window: self.discovered_context_window,
            probe_timeout_secs: self.probe_timeout_secs,
        }
    }

    /// Set context window override maps extracted from config.
    ///
    /// `global` comes from `[models.<id>].context_window` and
    /// `provider` comes from `[providers.<name>.model_overrides.<id>].context_window`.
    #[must_use]
    pub fn with_context_window_overrides(
        mut self,
        global: std::collections::HashMap<String, u32>,
        provider: std::collections::HashMap<String, u32>,
    ) -> Self {
        self.context_window_global = global;
        self.context_window_provider = provider;
        self
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

    /// Return the reasoning effort string if configured.
    ///
    /// Uses the OpenAI reasoning-effort vocabulary.
    pub(crate) fn reasoning_effort_str(&self) -> Option<&'static str> {
        use chelix_agents::model::ReasoningEffort;
        self.reasoning_effort.map(|e| match e {
            ReasoningEffort::None => "none",
            ReasoningEffort::Minimal => "minimal",
            ReasoningEffort::Low => "low",
            ReasoningEffort::Medium => "medium",
            ReasoningEffort::High => "high",
            ReasoningEffort::ExtraHigh => "xhigh",
            ReasoningEffort::Max => "max",
        })
    }

    /// Apply `reasoning_effort` for the **Chat Completions** API (used by
    /// `complete()` and `stream_with_tools_sse()`).
    ///
    /// Format: `"reasoning_effort": "high"` (top-level string field).
    pub(crate) fn apply_reasoning_effort_chat(&self, body: &mut serde_json::Value) {
        if let Some(effort) = self.reasoning_effort_str() {
            body["reasoning_effort"] = serde_json::json!(effort);
        }
    }

    /// Apply `reasoning_effort` for the **Responses** API (used by
    /// `stream_with_tools_websocket()`).
    ///
    /// Format: `"reasoning": { "effort": "high" }` (nested object).
    pub(crate) fn apply_reasoning_effort_responses(&self, body: &mut serde_json::Value) {
        if let Some(effort) = self.reasoning_effort_str() {
            body["reasoning"] = serde_json::json!({ "effort": effort });
        }
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

fn default_capabilities_for_provider(provider_name: &str) -> OpenAiProviderCapabilities {
    if provider_name == "gemini" {
        return OpenAiProviderCapabilities {
            default_strict_tools: false,
            requires_gemini_tool_call_extra_content: true,
            ..OpenAiProviderCapabilities::DEFAULT
        };
    }

    OpenAiProviderCapabilities::DEFAULT
}

#[async_trait]
impl LlmProvider for OpenAiProvider {
    fn name(&self) -> &str {
        &self.provider_name
    }

    fn reasoning_effort(&self) -> Option<chelix_agents::model::ReasoningEffort> {
        self.reasoning_effort
    }

    fn with_reasoning_effort(
        self: std::sync::Arc<Self>,
        effort: chelix_agents::model::ReasoningEffort,
    ) -> Option<std::sync::Arc<dyn LlmProvider>> {
        let mut forked = self.fork();
        forked.reasoning_effort = Some(effort);
        Some(std::sync::Arc::new(forked))
    }

    fn id(&self) -> &str {
        &self.model
    }

    fn supports_tools(&self) -> bool {
        match self.tool_mode_override {
            Some(chelix_config::ToolMode::Native) => true,
            Some(chelix_config::ToolMode::Text | chelix_config::ToolMode::Off) => false,
            Some(chelix_config::ToolMode::Auto) | None => self.model_capabilities.tools,
        }
    }

    fn tool_mode(&self) -> Option<chelix_config::ToolMode> {
        self.tool_mode_override
    }

    fn context_window(&self) -> u32 {
        let normalized = crate::model_id::capability_model_id(&self.model);
        if let Some(&context_window) = self.context_window_provider.get(normalized) {
            return context_window;
        }
        if let Some(&context_window) = self.context_window_global.get(normalized) {
            return context_window;
        }
        self.discovered_context_window.unwrap_or_else(|| {
            context_window_for_model_with_config(
                &self.model,
                &self.context_window_global,
                &self.context_window_provider,
            )
        })
    }

    fn supports_vision(&self) -> bool {
        self.model_capabilities.vision
    }

    async fn model_metadata(&self) -> anyhow::Result<ModelMetadata> {
        let meta = self
            .metadata_cache
            .get_or_try_init(|| async {
                let url = format!("{}/models/{}", self.base_url, self.model);
                debug!(url = %url, model = %self.model, "fetching model metadata");

                let resp = self
                    .client
                    .get(&url)
                    .header(
                        "Authorization",
                        format!("Bearer {}", self.api_key.expose_secret()),
                    )
                    .send()
                    .await?;

                if !resp.status().is_success() {
                    anyhow::bail!(
                        "model metadata API returned HTTP {}",
                        resp.status().as_u16()
                    );
                }

                let body: serde_json::Value = resp.json().await?;

                // OpenAI uses "context_window", some compat providers use "context_length".
                let context_length = body
                    .get("context_window")
                    .or_else(|| body.get("context_length"))
                    .and_then(|v| v.as_u64())
                    .map(|v| v as u32)
                    .unwrap_or_else(|| self.context_window());

                Ok(ModelMetadata {
                    id: body
                        .get("id")
                        .and_then(|v| v.as_str())
                        .unwrap_or(&self.model)
                        .to_string(),
                    context_length,
                })
            })
            .await?;
        Ok(meta.clone())
    }

    async fn complete(
        &self,
        messages: &[ChatMessage],
        tools: &[serde_json::Value],
    ) -> anyhow::Result<CompletionResponse> {
        self.complete_with_options(messages, tools, &AgentToolControls::default())
            .await
    }

    async fn complete_with_options(
        &self,
        messages: &[ChatMessage],
        tools: &[serde_json::Value],
        options: &AgentToolControls,
    ) -> anyhow::Result<CompletionResponse> {
        if matches!(self.wire_api, WireApi::Responses) {
            return self.complete_responses(messages, tools, options).await;
        }
        self.complete_chat(messages, tools, options).await
    }

    #[allow(clippy::collapsible_if)]
    fn stream(
        &self,
        messages: Vec<ChatMessage>,
    ) -> Pin<Box<dyn Stream<Item = StreamEvent> + Send + '_>> {
        self.stream_with_tools(messages, vec![])
    }

    async fn probe(&self) -> anyhow::Result<()> {
        match self.wire_api {
            WireApi::Responses => self.probe_responses().await,
            WireApi::ChatCompletions => self.probe_chat_completions().await,
        }
    }

    fn probe_timeout(&self) -> Duration {
        self.probe_timeout_duration()
    }

    async fn check_availability(&self) -> anyhow::Result<()> {
        self.check_model_in_catalog().await
    }

    #[allow(clippy::collapsible_if)]
    fn stream_with_tools(
        &self,
        messages: Vec<ChatMessage>,
        tools: Vec<serde_json::Value>,
    ) -> Pin<Box<dyn Stream<Item = StreamEvent> + Send + '_>> {
        self.stream_with_tools_and_options(messages, tools, AgentToolControls::default())
    }

    fn stream_with_tools_and_options(
        &self,
        messages: Vec<ChatMessage>,
        tools: Vec<serde_json::Value>,
        options: AgentToolControls,
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
    options: &AgentToolControls,
) -> anyhow::Result<()> {
    match options.tool_choice.as_ref() {
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
    options: &AgentToolControls,
) -> anyhow::Result<()> {
    match options.tool_choice.as_ref() {
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
mod tests {
    use {super::*, chelix_agents::model::ReasoningEffort, std::sync::Arc};

    #[test]
    fn reasoning_effort_can_be_set_on_openai_compatible_provider() {
        let provider = Arc::new(OpenAiProvider::new_with_name(
            secrecy::Secret::new("test-key".to_string()),
            "gpt-5.2".to_string(),
            "https://api.openai.com/v1".to_string(),
            "openai".to_string(),
        ));

        assert!(
            provider
                .with_reasoning_effort(ReasoningEffort::High)
                .is_some(),
            "OpenAI-compatible providers accept the reasoning_effort field"
        );
    }
}
