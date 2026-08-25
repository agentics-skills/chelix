use std::collections::HashMap;

use {
    super::{ProviderRegistry, registration::openai_builtin_capabilities},
    crate::openai::ResponsesWebSocketPolicy,
    chelix_agents::model::ReasoningEffort,
    chelix_config::{ChelixConfig, ToolMode},
};

#[test]
fn openai_default_base_url_enables_responses_websocket() {
    assert_eq!(
        openai_builtin_capabilities(false).responses_websocket_policy,
        ResponsesWebSocketPolicy::OpenAiPlatform,
    );
}

#[test]
fn openai_custom_base_url_disables_responses_websocket() {
    assert_eq!(
        openai_builtin_capabilities(true).responses_websocket_policy,
        ResponsesWebSocketPolicy::Unsupported,
    );
}

#[test]
fn custom_model_config_preserves_max_for_runtime_provider() {
    const MODEL_ID: &str = "custom-ai-example::Combos/z.ai/glm";
    let config: ChelixConfig = toml::from_str(
        r#"
[providers.custom-ai-example]
api_key = "test-key"
base_url = "https://example.invalid/v1"
fetch_models = false

[providers.custom-ai-example.models."Combos/z.ai/glm"]
context_length = 400000
max_input_tokens = 272000
max_output_tokens = 128000
input_modalities = ["text", "image", "audio", "file"]
output_modalities = ["text"]
tool_calling = true
streaming = true
zeroDataRetentionEnabled = true

[providers.custom-ai-example.models."Combos/z.ai/glm".reasoning]
supported_efforts = ["none", "minimal", "low", "medium", "high", "max"]
summary = "detailed"
include = ["reasoning.encrypted_content"]
"#,
    )
    .unwrap_or_else(|error| {
        panic!("production custom-provider config should deserialize: {error}")
    });

    let registry = ProviderRegistry::from_config(&config.providers, &HashMap::new());
    let models = registry.list_models();
    let listed = models
        .iter()
        .find(|model| model.id == MODEL_ID)
        .unwrap_or_else(|| panic!("configured model should be listed"));
    assert_eq!(
        listed
            .metadata
            .reasoning
            .supported_efforts
            .iter()
            .map(ReasoningEffort::as_str)
            .collect::<Vec<_>>(),
        vec!["none", "minimal", "low", "medium", "high", "max"]
    );

    let provider = registry
        .get(MODEL_ID)
        .unwrap_or_else(|| panic!("configured model should have a runtime provider"));
    let configured = provider
        .with_reasoning_effort(ReasoningEffort::from("max"))
        .unwrap_or_else(|| panic!("runtime provider should accept configured max effort"));
    assert_eq!(configured.context_window(), Some(400_000));
    assert_eq!(configured.max_input_tokens(), Some(272_000));
    assert_eq!(configured.max_output_tokens(), Some(128_000));
    assert_eq!(
        configured
            .reasoning_effort()
            .as_ref()
            .map(ReasoningEffort::as_str),
        Some("max")
    );
}

#[test]
fn model_tool_capability_remains_separate_from_native_mode() {
    const CHAT_ONLY_MODEL_ID: &str = "custom-ai-capability::chat-only";
    const TOOL_MODEL_ID: &str = "custom-ai-capability::tool-capable";
    let config: ChelixConfig = toml::from_str(
        r#"
[providers.custom-ai-capability]
api_key = "test-key"
base_url = "https://example.invalid/v1"
fetch_models = false

[providers.custom-ai-capability.models.chat-only]
context_length = 128000
max_input_tokens = 96000
max_output_tokens = 32000
input_modalities = ["text"]
output_modalities = ["text"]
tool_calling = false
streaming = true

[providers.custom-ai-capability.models.chat-only.reasoning]
supported_efforts = ["none"]

[providers.custom-ai-capability.models.tool-capable]
context_length = 128000
max_input_tokens = 96000
max_output_tokens = 32000
input_modalities = ["text"]
output_modalities = ["text"]
tool_calling = true
streaming = true

[providers.custom-ai-capability.models.tool-capable.reasoning]
supported_efforts = ["none"]
"#,
    )
    .unwrap_or_else(|error| panic!("capability config should deserialize: {error}"));

    let registry = ProviderRegistry::from_config(&config.providers, &HashMap::new());
    let chat_only = registry
        .get(CHAT_ONLY_MODEL_ID)
        .unwrap_or_else(|| panic!("chat-only model should be registered"));
    let tool_capable = registry
        .get(TOOL_MODEL_ID)
        .unwrap_or_else(|| panic!("tool-capable model should be registered"));

    assert_eq!(chat_only.tool_mode(), ToolMode::Native);
    assert_eq!(tool_capable.tool_mode(), ToolMode::Native);
    assert!(!chat_only.supports_tools());
    assert!(tool_capable.supports_tools());
    assert_eq!(
        registry
            .first_with_tools()
            .unwrap_or_else(|| panic!("tool-capable model should be selected"))
            .id(),
        TOOL_MODEL_ID
    );
}

#[test]
fn first_with_tools_honors_explicit_modes_without_fallback() {
    const TEXT_MODEL_ID: &str = "openai::text-mode";
    const OFF_MODEL_ID: &str = "custom-disabled-tools::off-mode";
    let config: ChelixConfig = toml::from_str(
        r#"
[providers.openai]
api_key = "test-key"
fetch_models = false
tool_mode = "text"

[providers.openai.models.text-mode]
context_length = 128000
max_input_tokens = 96000
max_output_tokens = 32000
input_modalities = ["text"]
output_modalities = ["text"]
tool_calling = false
streaming = true
zeroDataRetentionEnabled = true

[providers.openai.models.text-mode.reasoning]
supported_efforts = ["none"]

[providers.custom-disabled-tools]
api_key = "test-key"
base_url = "https://example.invalid/v1"
fetch_models = false
tool_mode = "off"

[providers.custom-disabled-tools.models.off-mode]
context_length = 128000
max_input_tokens = 96000
max_output_tokens = 32000
input_modalities = ["text"]
output_modalities = ["text"]
tool_calling = true
streaming = true
zeroDataRetentionEnabled = true

[providers.custom-disabled-tools.models.off-mode.reasoning]
supported_efforts = ["none"]
"#,
    )
    .unwrap_or_else(|error| panic!("explicit tool mode config should deserialize: {error}"));

    let mut registry = ProviderRegistry::from_config(&config.providers, &HashMap::new());
    let text_provider = registry
        .get(TEXT_MODEL_ID)
        .unwrap_or_else(|| panic!("text-mode model should be registered"));
    let off_provider = registry
        .get(OFF_MODEL_ID)
        .unwrap_or_else(|| panic!("off-mode model should be registered"));

    assert_eq!(text_provider.tool_mode(), ToolMode::Text);
    assert!(!text_provider.supports_tools());
    assert_eq!(off_provider.tool_mode(), ToolMode::Off);
    assert!(off_provider.supports_tools());
    assert_eq!(
        registry
            .first_with_tools()
            .unwrap_or_else(|| panic!("explicit text-mode model should be selected"))
            .id(),
        TEXT_MODEL_ID
    );

    assert!(registry.unregister(TEXT_MODEL_ID));
    assert_eq!(
        registry
            .first()
            .unwrap_or_else(|| panic!("off-mode model should remain registered"))
            .id(),
        OFF_MODEL_ID
    );
    assert!(registry.first_with_tools().is_none());
}
