use std::collections::HashMap;

use {
    super::{ProviderRegistry, registration::openai_builtin_capabilities},
    crate::openai::ResponsesWebSocketPolicy,
    chelix_agents::model::ReasoningEffort,
    chelix_config::ChelixConfig,
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
    const MODEL_ID: &str = "custom-ai-0xff-dad::Combos/z.ai/glm";
    let config: ChelixConfig = toml::from_str(
        r#"
[providers.custom-ai-0xff-dad]
api_key = "test-key"
base_url = "https://example.invalid/v1"
fetch_models = false

[providers.custom-ai-0xff-dad.models."Combos/z.ai/glm"]
context_length = 400000
max_input_tokens = 272000
max_output_tokens = 128000
input_modalities = ["text", "image", "audio", "file"]
output_modalities = ["text"]
tool_calling = true
streaming = true
zeroDataRetentionEnabled = true

[providers.custom-ai-0xff-dad.models."Combos/z.ai/glm".reasoning]
supported_efforts = ["none", "minimal", "low", "medium", "high", "max"]
summary = "detailed"
include = ["reasoning.encrypted_content"]
"#,
    )
    .expect("production custom-provider config should deserialize");

    let registry = ProviderRegistry::from_config(&config.providers, &HashMap::new());
    let listed = registry
        .list_models()
        .iter()
        .find(|model| model.id == MODEL_ID)
        .expect("configured model should be listed");
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
        .expect("configured model should have a runtime provider");
    let configured = provider
        .with_reasoning_effort(ReasoningEffort::from("max"))
        .expect("runtime provider should accept configured max effort");
    assert_eq!(
        configured
            .reasoning_effort()
            .as_ref()
            .map(ReasoningEffort::as_str),
        Some("max")
    );
}
