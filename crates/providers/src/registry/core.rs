//! Metadata-owned provider registry storage and lookup.

use std::{
    collections::{HashMap, HashSet},
    pin::Pin,
    sync::Arc,
};

use {
    chelix_agents::model::{
        ChatMessage, CompletionOptions, CompletionResponse, LlmProvider, ReasoningEffort,
        StreamEvent, ToolChoice,
    },
    chelix_common::{ModelMetadata, ModelModality, ResolvedModelReasoningError},
    tokio_stream::Stream,
};

use crate::{
    model_capabilities::ModelInfo,
    model_id::{namespaced_model_id, raw_model_id},
};

use super::ResolvedModelReasoning;

/// Runtime provider resolved from one validated model/reasoning pair.
#[derive(Clone)]
pub struct ResolvedModel {
    model_reasoning: ResolvedModelReasoning,
    provider: Arc<dyn LlmProvider>,
}

impl ResolvedModel {
    /// Validated canonical model/reasoning pair.
    #[must_use]
    pub const fn model_reasoning(&self) -> &ResolvedModelReasoning {
        &self.model_reasoning
    }

    /// Provider configured with the validated reasoning effort when applicable.
    #[must_use]
    pub const fn provider(&self) -> &Arc<dyn LlmProvider> {
        &self.provider
    }
}

/// Semantic errors returned by strict model/reasoning resolution.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ModelResolutionError {
    #[error("model is required")]
    MissingModel,
    #[error("model `{model_id}` is not registered")]
    UnknownModel { model_id: String },
    #[error("model ID `{model_id}` is not canonical; use `{canonical_model_id}`")]
    NonCanonicalModelId {
        model_id: String,
        canonical_model_id: String,
    },
    #[error("raw model ID `{model_id}` is ambiguous; use one of {canonical_model_ids:?}")]
    AmbiguousModelId {
        model_id: String,
        canonical_model_ids: Vec<String>,
    },
    #[error("reasoning effort is required for model `{model_id}`")]
    MissingReasoningEffort { model_id: String },
    #[error("reasoning effort must not be empty for model `{model_id}`")]
    EmptyReasoningEffort { model_id: String },
    #[error("model `{model_id}` does not support reasoning effort `{reasoning_effort}`")]
    UnsupportedReasoningEffort {
        model_id: String,
        reasoning_effort: String,
    },
    #[error(
        "provider for model `{model_id}` could not apply reasoning effort `{reasoning_effort}`"
    )]
    ReasoningEffortApplicationFailed {
        model_id: String,
        reasoning_effort: String,
    },
}

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
        self.metadata.tool_calling
    }

    fn tool_mode(&self) -> chelix_config::ToolMode {
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
        tool_choice: Option<ToolChoice>,
    ) -> Pin<Box<dyn Stream<Item = StreamEvent> + Send + '_>> {
        self.inner
            .stream_with_tools_and_options(messages, tools, tool_choice)
    }

    fn reasoning_effort(&self) -> Option<ReasoningEffort> {
        self.inner.reasoning_effort()
    }

    fn with_reasoning_effort(
        self: Arc<Self>,
        effort: ReasoningEffort,
    ) -> Option<Arc<dyn LlmProvider>> {
        if !self.metadata.reasoning_supported_efforts.contains(&effort) {
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

    fn model_lookup_error(&self, model_id: &str) -> ModelResolutionError {
        if raw_model_id(model_id) != model_id {
            return ModelResolutionError::UnknownModel {
                model_id: model_id.to_string(),
            };
        }

        let mut canonical_model_ids = self
            .models
            .iter()
            .filter(|model| raw_model_id(&model.id) == model_id)
            .map(|model| model.id.clone())
            .collect::<Vec<_>>();
        canonical_model_ids.sort();

        match canonical_model_ids.as_slice() {
            [] => ModelResolutionError::UnknownModel {
                model_id: model_id.to_string(),
            },
            [canonical_model_id] => ModelResolutionError::NonCanonicalModelId {
                model_id: model_id.to_string(),
                canonical_model_id: canonical_model_id.clone(),
            },
            _ => ModelResolutionError::AmbiguousModelId {
                model_id: model_id.to_string(),
                canonical_model_ids,
            },
        }
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

    /// Remove one exact canonical model key. Returns whether it was registered.
    pub fn unregister(&mut self, model_id: &str) -> bool {
        let removed = self.providers.remove(model_id).is_some();
        if removed {
            self.models.retain(|model| model.id != model_id);
        }
        removed
    }

    /// Return the provider registered under one exact canonical model key.
    pub fn get(&self, model_id: &str) -> Option<Arc<dyn LlmProvider>> {
        self.providers.get(model_id).cloned()
    }

    /// Resolve and apply one complete model/reasoning selection.
    pub fn resolve_model_reasoning(
        &self,
        model_id: Option<&str>,
        reasoning_effort: Option<&ReasoningEffort>,
    ) -> Result<ResolvedModel, ModelResolutionError> {
        let model_id = model_id.ok_or(ModelResolutionError::MissingModel)?;
        let model = self
            .models
            .iter()
            .find(|model| model.id == model_id)
            .ok_or_else(|| self.model_lookup_error(model_id))?;
        let provider = self
            .providers
            .get(model_id)
            .cloned()
            .ok_or_else(|| self.model_lookup_error(model_id))?;

        let effort =
            reasoning_effort.ok_or_else(|| ModelResolutionError::MissingReasoningEffort {
                model_id: model.id.clone(),
            })?;
        let model_reasoning = ResolvedModelReasoning::try_new(model.id.clone(), effort.clone())
            .map_err(|error| match error {
                ResolvedModelReasoningError::EmptyModelId => ModelResolutionError::UnknownModel {
                    model_id: model.id.clone(),
                },
                ResolvedModelReasoningError::EmptyReasoningEffort => {
                    ModelResolutionError::EmptyReasoningEffort {
                        model_id: model.id.clone(),
                    }
                },
            })?;
        if !model
            .metadata
            .reasoning_supported_efforts
            .contains(model_reasoning.reasoning_effort())
        {
            return Err(ModelResolutionError::UnsupportedReasoningEffort {
                model_id: model.id.clone(),
                reasoning_effort: model_reasoning.reasoning_effort().as_str().to_string(),
            });
        }
        let provider = Arc::clone(&provider)
            .with_reasoning_effort(model_reasoning.reasoning_effort().clone())
            .ok_or_else(|| ModelResolutionError::ReasoningEffortApplicationFailed {
                model_id: model.id.clone(),
                reasoning_effort: model_reasoning.reasoning_effort().as_str().to_string(),
            })?;

        Ok(ResolvedModel {
            model_reasoning,
            provider,
        })
    }

    pub fn first(&self) -> Option<Arc<dyn LlmProvider>> {
        self.models
            .first()
            .and_then(|model| self.providers.get(&model.id))
            .cloned()
    }

    /// Return the first provider that can run tools with its configured tool mode.
    pub fn first_with_tools(&self) -> Option<Arc<dyn LlmProvider>> {
        self.models
            .iter()
            .filter_map(|model| self.providers.get(&model.id))
            .find(|provider| match provider.tool_mode() {
                chelix_config::ToolMode::Native => provider.supports_tools(),
                chelix_config::ToolMode::Text => true,
                chelix_config::ToolMode::Off => false,
            })
            .cloned()
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
