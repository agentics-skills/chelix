use std::collections::HashMap;

use {
    super::{ModelResolutionError, ProviderRegistry, registration::openai_builtin_capabilities},
    crate::openai::ResponsesWebSocketPolicy,
    chelix_agents::model::ReasoningEffort,
    chelix_config::{ChelixConfig, ToolMode},
};

fn resolution_registry() -> ProviderRegistry {
    let config: ChelixConfig = toml::from_str(
        r#"
[providers.custom-alpha]
api_key = "test-key"
base_url = "https://alpha.example.invalid/v1"

[providers.custom-alpha.models.shared]
context_length = 128000
max_input_tokens = 96000
max_output_tokens = 32000
input_modalities = ["text"]
output_modalities = ["text"]
tool_calling = true
streaming = true
zeroDataRetentionEnabled = false
reasoning_supported_efforts = ["low"]

[providers.custom-alpha.models.reasoning]
context_length = 128000
max_input_tokens = 96000
max_output_tokens = 32000
input_modalities = ["text"]
output_modalities = ["text"]
tool_calling = true
streaming = true
zeroDataRetentionEnabled = false
reasoning_supported_efforts = ["low", "high"]

[providers.custom-alpha.models.none-only]
context_length = 128000
max_input_tokens = 96000
max_output_tokens = 32000
input_modalities = ["text"]
output_modalities = ["text"]
tool_calling = true
streaming = true
zeroDataRetentionEnabled = false
reasoning_supported_efforts = ["none"]

[providers.custom-beta]
api_key = "test-key"
base_url = "https://beta.example.invalid/v1"

[providers.custom-beta.models.shared]
context_length = 128000
max_input_tokens = 96000
max_output_tokens = 32000
input_modalities = ["text"]
output_modalities = ["text"]
tool_calling = true
streaming = true
zeroDataRetentionEnabled = false
reasoning_supported_efforts = ["low"]
"#,
    )
    .unwrap_or_else(|error| panic!("resolution registry config should deserialize: {error}"));

    ProviderRegistry::from_config(&config.providers, &HashMap::new())
        .unwrap_or_else(|error| panic!("resolution registry should build: {error}"))
}

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
fn custom_model_config_preserves_metadata_for_runtime_provider() {
    const MODEL_ID: &str = "custom-ai-example::Combos/z.ai/glm";
    let config: ChelixConfig = toml::from_str(
        r#"
[providers.custom-ai-example]
api_key = "test-key"
base_url = "https://example.invalid/v1"

[providers.custom-ai-example.models."Combos/z.ai/glm"]
context_length = 400000
max_input_tokens = 272000
max_output_tokens = 128000
input_modalities = ["text", "image", "audio", "file"]
output_modalities = ["text"]
tool_calling = true
streaming = true
zeroDataRetentionEnabled = true
reasoning_supported_efforts = ["none", "minimal", "low", "medium", "high", "max"]
reasoning_summary = "detailed"
reasoning_include = ["encrypted_content"]
"#,
    )
    .unwrap_or_else(|error| {
        panic!("production custom-provider config should deserialize: {error}")
    });

    let registry = ProviderRegistry::from_config(&config.providers, &HashMap::new())
        .unwrap_or_else(|error| panic!("complete model config should build: {error}"));
    let models = registry.list_models();
    let listed = models
        .iter()
        .find(|model| model.id == MODEL_ID)
        .unwrap_or_else(|| panic!("configured model should be listed"));
    assert_eq!(
        listed
            .metadata
            .reasoning_supported_efforts
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

[providers.custom-ai-capability.models.chat-only]
context_length = 128000
max_input_tokens = 96000
max_output_tokens = 32000
input_modalities = ["text"]
output_modalities = ["text"]
tool_calling = false
streaming = true
zeroDataRetentionEnabled = false
reasoning_supported_efforts = ["low"]

[providers.custom-ai-capability.models.tool-capable]
context_length = 128000
max_input_tokens = 96000
max_output_tokens = 32000
input_modalities = ["text"]
output_modalities = ["text"]
tool_calling = true
streaming = true
zeroDataRetentionEnabled = false
reasoning_supported_efforts = ["low"]
"#,
    )
    .unwrap_or_else(|error| panic!("capability config should deserialize: {error}"));

    let registry = ProviderRegistry::from_config(&config.providers, &HashMap::new())
        .unwrap_or_else(|error| panic!("complete model config should build: {error}"));
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
reasoning_supported_efforts = ["low"]

[providers.custom-disabled-tools]
api_key = "test-key"
base_url = "https://example.invalid/v1"
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
reasoning_supported_efforts = ["low"]
"#,
    )
    .unwrap_or_else(|error| panic!("explicit tool mode config should deserialize: {error}"));

    let mut registry = ProviderRegistry::from_config(&config.providers, &HashMap::new())
        .unwrap_or_else(|error| panic!("complete model config should build: {error}"));
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

#[test]
fn registry_lookup_and_unregister_require_exact_canonical_ids() {
    const ALPHA_MODEL_ID: &str = "custom-alpha::shared";
    const BETA_MODEL_ID: &str = "custom-beta::shared";
    let mut registry = resolution_registry();

    assert_eq!(
        registry
            .get(ALPHA_MODEL_ID)
            .unwrap_or_else(|| panic!("alpha model should resolve by its exact key"))
            .id(),
        ALPHA_MODEL_ID
    );
    assert_eq!(
        registry
            .get(BETA_MODEL_ID)
            .unwrap_or_else(|| panic!("beta model should resolve by its exact key"))
            .id(),
        BETA_MODEL_ID
    );
    assert!(registry.get("shared").is_none());
    assert!(!registry.unregister("shared"));
    assert!(registry.get(ALPHA_MODEL_ID).is_some());
    assert!(registry.get(BETA_MODEL_ID).is_some());

    assert!(registry.unregister(ALPHA_MODEL_ID));
    assert!(registry.get(ALPHA_MODEL_ID).is_none());
    assert!(registry.get(BETA_MODEL_ID).is_some());
}

#[test]
fn resolver_returns_typed_applied_reasoning_efforts() {
    let registry = resolution_registry();
    let low = ReasoningEffort::from("low");
    let shared = registry
        .resolve_model_reasoning(Some("custom-alpha::shared"), Some(&low))
        .unwrap_or_else(|error| panic!("single-effort model should resolve: {error}"));
    assert_eq!(shared.model_reasoning().model_id(), "custom-alpha::shared");
    assert_eq!(shared.model_reasoning().reasoning_effort(), &low);
    assert_eq!(shared.provider().reasoning_effort(), Some(low));

    let high = ReasoningEffort::from("high");
    let reasoning = registry
        .resolve_model_reasoning(Some("custom-alpha::reasoning"), Some(&high))
        .unwrap_or_else(|error| panic!("multi-effort model should resolve: {error}"));
    assert_eq!(reasoning.model_reasoning().reasoning_effort(), &high);
    assert_eq!(reasoning.provider().reasoning_effort(), Some(high));

    let none = ReasoningEffort::from("none");
    let none_only = registry
        .resolve_model_reasoning(Some("custom-alpha::none-only"), Some(&none))
        .unwrap_or_else(|error| panic!("provider-defined none effort should resolve: {error}"));
    assert_eq!(none_only.model_reasoning().reasoning_effort(), &none);
    assert_eq!(none_only.provider().reasoning_effort(), Some(none));
}

#[test]
fn resolver_rejects_invalid_model_reasoning_selections() {
    let registry = resolution_registry();
    let cases = vec![
        (None, None, ModelResolutionError::MissingModel),
        (
            Some("custom-alpha::reasoning"),
            None,
            ModelResolutionError::MissingReasoningEffort {
                model_id: "custom-alpha::reasoning".to_string(),
            },
        ),
        (
            Some("custom-alpha::reasoning"),
            Some(ReasoningEffort::from("")),
            ModelResolutionError::EmptyReasoningEffort {
                model_id: "custom-alpha::reasoning".to_string(),
            },
        ),
        (
            Some("custom-alpha::missing"),
            None,
            ModelResolutionError::UnknownModel {
                model_id: "custom-alpha::missing".to_string(),
            },
        ),
        (
            Some("reasoning"),
            Some(ReasoningEffort::from("high")),
            ModelResolutionError::NonCanonicalModelId {
                model_id: "reasoning".to_string(),
                canonical_model_id: "custom-alpha::reasoning".to_string(),
            },
        ),
        (
            Some("shared"),
            None,
            ModelResolutionError::AmbiguousModelId {
                model_id: "shared".to_string(),
                canonical_model_ids: vec![
                    "custom-alpha::shared".to_string(),
                    "custom-beta::shared".to_string(),
                ],
            },
        ),
        (
            Some("custom-alpha::reasoning"),
            Some(ReasoningEffort::from("medium")),
            ModelResolutionError::UnsupportedReasoningEffort {
                model_id: "custom-alpha::reasoning".to_string(),
                reasoning_effort: "medium".to_string(),
            },
        ),
    ];

    for (model_id, reasoning_effort, expected) in cases {
        let error = registry
            .resolve_model_reasoning(model_id, reasoning_effort.as_ref())
            .err()
            .unwrap_or_else(|| panic!("invalid selection {model_id:?} should fail"));
        assert_eq!(error, expected, "unexpected error for {model_id:?}");
    }
}

#[test]
fn offered_allowlist_excludes_unselected_provider_from_enabled_set() {
    let config: ChelixConfig = toml::from_str(
        r#"
[providers]
offered = ["openai"]

[providers.openai]
api_key = "test-key"

[providers.openai.models.selected]
context_length = 128000
max_input_tokens = 96000
max_output_tokens = 32000
input_modalities = ["text"]
output_modalities = ["text"]
tool_calling = true
streaming = true
zeroDataRetentionEnabled = false
reasoning_supported_efforts = ["low"]

[providers.openrouter]
api_key = "test-key"

[providers.openrouter.models.excluded]
context_length = 128000
max_input_tokens = 96000
max_output_tokens = 32000
input_modalities = ["text"]
output_modalities = ["text"]
tool_calling = true
streaming = true
zeroDataRetentionEnabled = false
reasoning_supported_efforts = ["low"]
"#,
    )
    .unwrap_or_else(|error| panic!("provider config should deserialize: {error}"));

    assert!(config.providers.is_enabled("openai"));
    assert!(!config.providers.is_enabled("openrouter"));
    let registry = ProviderRegistry::from_config(&config.providers, &HashMap::new())
        .unwrap_or_else(|error| panic!("filtered registry should build: {error}"));
    assert!(registry.get("openai::selected").is_some());
    assert!(registry.get("openrouter::excluded").is_none());
    assert_eq!(registry.list_models().len(), 1);
}

#[test]
fn invalid_model_error_identifies_provider_and_model() {
    let config: ChelixConfig = toml::from_str(
        r#"
[providers.openai]
api_key = "test-key"

[providers.openai.models.incomplete]
context_length = 128000
"#,
    )
    .unwrap_or_else(|error| panic!("partial boundary config should deserialize: {error}"));

    let error = ProviderRegistry::from_config(&config.providers, &HashMap::new())
        .err()
        .unwrap_or_else(|| panic!("incomplete model should fail"));
    let message = error.to_string();
    assert!(message.contains("provider `openai` model `incomplete`"));
    assert!(message.contains("max_input_tokens"));
}

#[test]
fn registry_build_fails_as_a_unit_when_any_model_is_invalid() {
    let config: ChelixConfig = toml::from_str(
        r#"
[providers.openai]
api_key = "test-key"

[providers.openai.models.valid]
context_length = 128000
max_input_tokens = 96000
max_output_tokens = 32000
input_modalities = ["text"]
output_modalities = ["text"]
tool_calling = true
streaming = true
zeroDataRetentionEnabled = false
reasoning_supported_efforts = ["low"]

[providers.openai.models.invalid]
context_length = 128000
"#,
    )
    .unwrap_or_else(|error| panic!("partial boundary config should deserialize: {error}"));

    assert!(ProviderRegistry::from_config(&config.providers, &HashMap::new()).is_err());
}
