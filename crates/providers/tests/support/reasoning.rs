use std::sync::Arc;

use {
    chelix_agents::model::LlmProvider,
    chelix_common::{ModelMetadata, ModelModality, ReasoningEffort},
    chelix_providers::openai::OpenAiProvider,
};

pub fn configure(
    provider: OpenAiProvider,
    supported_efforts: Vec<ReasoningEffort>,
    selected_effort: ReasoningEffort,
) -> Arc<dyn LlmProvider> {
    let provider = provider.with_reasoning_metadata(&ModelMetadata {
        context_length: 128_000,
        max_input_tokens: 96_000,
        max_output_tokens: 32_000,
        input_modalities: vec![ModelModality::Text],
        output_modalities: vec![ModelModality::Text],
        tool_calling: true,
        streaming: true,
        zero_data_retention_enabled: false,
        reasoning_supported_efforts: supported_efforts,
        reasoning_summary: None,
        reasoning_include: None,
    });
    Arc::new(provider)
        .with_reasoning_effort(selected_effort)
        .expect("integration fixture effort must belong to its model metadata")
}
