//! Metadata-owned provider registry storage and lookup.

use std::{
    collections::{HashMap, HashSet},
    pin::Pin,
    sync::Arc,
};

use {
    chelix_agents::model::{
        AgentToolControls, ChatMessage, CompletionOptions, CompletionResponse, LlmProvider,
        ReasoningEffort, StreamEvent,
    },
    chelix_common::{ModelMetadata, ModelModality},
    tokio_stream::Stream,
};

use crate::{
    config_helpers::subscription_preference_rank,
    model_capabilities::ModelInfo,
    model_id::{namespaced_model_id, raw_model_id},
};

struct RegistryModelProvider {
    model_id: String,
    metadata: ModelMetadata,
    inner: Arc<dyn LlmProvider>,
}

#[async_trait::async_trait]
impl LlmProvider for RegistryModelProvider {
    fn name(&self) -> &str {
        self.inner.name()
    }

    fn id(&self) -> &str {
        &self.model_id
    }

    async fn complete(
        &self,
        messages: &[ChatMessage],
        tools: &[serde_json::Value],
    ) -> anyhow::Result<CompletionResponse> {
        self.inner.complete(messages, tools).await
    }

    async fn complete_with_options(
        &self,
        messages: &[ChatMessage],
        tools: &[serde_json::Value],
        options: &CompletionOptions,
    ) -> anyhow::Result<CompletionResponse> {
        self.inner
            .complete_with_options(messages, tools, options)
            .await
    }

    fn supports_tools(&self) -> bool {
        match self.inner.tool_mode() {
            Some(chelix_config::ToolMode::Native) => true,
            Some(chelix_config::ToolMode::Text | chelix_config::ToolMode::Off) => false,
            Some(chelix_config::ToolMode::Auto) | None => self.metadata.tool_calling,
        }
    }

    fn tool_mode(&self) -> Option<chelix_config::ToolMode> {
        self.inner.tool_mode()
    }

    fn context_window(&self) -> Option<u32> {
        Some(self.metadata.context_length)
    }

    fn max_input_tokens(&self) -> Option<u32> {
        Some(self.metadata.max_input_tokens)
    }

    fn max_output_tokens(&self) -> Option<u32> {
        Some(self.metadata.max_output_tokens)
    }

    fn supports_vision(&self) -> bool {
        self.metadata.supports_input(ModelModality::Image)
    }

    fn stream(
        &self,
        messages: Vec<ChatMessage>,
    ) -> Pin<Box<dyn Stream<Item = StreamEvent> + Send + '_>> {
        self.inner.stream(messages)
    }

    fn stream_with_tools(
        &self,
        messages: Vec<ChatMessage>,
        tools: Vec<serde_json::Value>,
    ) -> Pin<Box<dyn Stream<Item = StreamEvent> + Send + '_>> {
        self.inner.stream_with_tools(messages, tools)
    }

    fn stream_with_tools_and_options(
        &self,
        messages: Vec<ChatMessage>,
        tools: Vec<serde_json::Value>,
        options: AgentToolControls,
    ) -> Pin<Box<dyn Stream<Item = StreamEvent> + Send + '_>> {
        self.inner
            .stream_with_tools_and_options(messages, tools, options)
    }

    fn reasoning_effort(&self) -> Option<ReasoningEffort> {
        self.inner.reasoning_effort()
    }

    fn with_reasoning_effort(
        self: Arc<Self>,
        effort: ReasoningEffort,
    ) -> Option<Arc<dyn LlmProvider>> {
        if !self.metadata.reasoning.supported_efforts.contains(&effort) {
            return None;
        }
        let new_inner = Arc::clone(&self.inner).with_reasoning_effort(effort)?;
        Some(Arc::new(Self {
            model_id: self.model_id.clone(),
            metadata: self.metadata.clone(),
            inner: new_inner,
        }))
    }
}

/// Registry of available LLM providers, keyed by namespaced model ID.
pub struct ProviderRegistry {
    pub(crate) providers: HashMap<String, Arc<dyn LlmProvider>>,
    pub(crate) models: Vec<ModelInfo>,
}

impl ProviderRegistry {
    #[must_use]
    pub fn empty() -> Self {
        Self {
            providers: HashMap::new(),
            models: Vec::new(),
        }
    }

    pub(crate) fn has_provider_model(&self, provider: &str, model_id: &str) -> bool {
        self.providers
            .contains_key(&namespaced_model_id(provider, model_id))
    }

    pub(crate) fn resolve_registry_model_id(
        &self,
        model_id: &str,
        provider_hint: Option<&str>,
    ) -> Option<String> {
        if self.providers.contains_key(model_id) {
            return Some(model_id.to_string());
        }

        let raw = raw_model_id(model_id);
        self.models
            .iter()
            .enumerate()
            .filter(|(_, model)| raw_model_id(&model.id) == raw)
            .filter(|(_, model)| provider_hint.is_none_or(|hint| model.provider == hint))
            .min_by_key(|(index, model)| (subscription_preference_rank(&model.provider), *index))
            .map(|(_, model)| model.id.clone())
    }

    /// Register one fully resolved model and its wire transport.
    pub fn register(&mut self, mut info: ModelInfo, provider: Arc<dyn LlmProvider>) {
        let model_id = raw_model_id(&info.id).to_string();
        let registry_model_id = namespaced_model_id(&info.provider, &model_id);
        info.id = registry_model_id.clone();
        let wrapped: Arc<dyn LlmProvider> = Arc::new(RegistryModelProvider {
            model_id: registry_model_id.clone(),
            metadata: info.metadata.clone(),
            inner: provider,
        });
        self.providers.insert(registry_model_id, wrapped);
        self.models.push(info);
    }

    /// Remove one model. Returns whether it was registered.
    pub fn unregister(&mut self, model_id: &str) -> bool {
        let resolved_id = self.resolve_registry_model_id(model_id, None);
        let removed = resolved_id
            .as_deref()
            .and_then(|id| self.providers.remove(id))
            .is_some();
        if removed && let Some(id) = resolved_id {
            self.models.retain(|model| model.id != id);
        }
        removed
    }

    pub(crate) fn remove_provider(&mut self, provider_name: &str) {
        let model_ids: HashSet<String> = self
            .models
            .iter()
            .filter(|model| model.provider == provider_name)
            .map(|model| model.id.clone())
            .collect();
        self.models.retain(|model| model.provider != provider_name);
        self.providers
            .retain(|model_id, _| !model_ids.contains(model_id));
    }

    pub fn get(&self, model_id: &str) -> Option<Arc<dyn LlmProvider>> {
        self.resolve_registry_model_id(model_id, None)
            .as_deref()
            .and_then(|id| self.providers.get(id))
            .cloned()
    }

    pub fn first(&self) -> Option<Arc<dyn LlmProvider>> {
        self.models
            .iter()
            .enumerate()
            .min_by_key(|(index, model)| (subscription_preference_rank(&model.provider), *index))
            .map(|(_, model)| model)
            .and_then(|model| self.providers.get(&model.id))
            .cloned()
    }

    /// Return the first provider supporting tools, or the first provider overall.
    pub fn first_with_tools(&self) -> Option<Arc<dyn LlmProvider>> {
        self.models
            .iter()
            .enumerate()
            .filter_map(|(index, model)| {
                self.providers
                    .get(&model.id)
                    .map(|provider| (index, model, provider))
            })
            .filter(|(_, _, provider)| provider.supports_tools())
            .min_by_key(|(index, model, _)| (subscription_preference_rank(&model.provider), *index))
            .map(|(_, _, provider)| Arc::clone(provider))
            .or_else(|| self.first())
    }

    pub fn list_models(&self) -> &[ModelInfo] {
        &self.models
    }

    pub fn all_providers(&self) -> Vec<Arc<dyn LlmProvider>> {
        self.models
            .iter()
            .filter_map(|model| self.providers.get(&model.id).cloned())
            .collect()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.providers.is_empty()
    }

    #[must_use]
    pub fn provider_summary(&self) -> String {
        if self.providers.is_empty() {
            return "no LLM providers configured".into();
        }
        let provider_count = self
            .models
            .iter()
            .map(|model| model.provider.as_str())
            .collect::<HashSet<_>>()
            .len();
        let model_count = self.models.len();
        format!(
            "{} provider{}, {} model{}",
            provider_count,
            if provider_count == 1 {
                ""
            } else {
                "s"
            },
            model_count,
            if model_count == 1 {
                ""
            } else {
                "s"
            },
        )
    }
}
