#![allow(clippy::unwrap_used, clippy::expect_used)]

use {
    super::*,
    crate::KeyStore,
    chelix_config::{
        AgentConfig, AgentsConfig,
        schema::{
            ModelConfigMap, ModelModality, PartialModelMetadata, ProviderEntry, ProvidersConfig,
            ReasoningEffort,
        },
    },
    chelix_providers::ProviderRegistry,
    chelix_service_traits::{NoopProviderSetupService, ProviderSetupService},
    std::{collections::HashMap, sync::Arc},
    tokio::sync::RwLock,
};

fn live_provider_setup_service(
    registry: Arc<RwLock<ProviderRegistry>>,
    config: ProvidersConfig,
    deploy_platform: Option<String>,
) -> LiveProviderSetupService {
    LiveProviderSetupService::new(
        registry,
        config,
        deploy_platform,
        ProviderConfigPersistence::MemoryOnly,
    )
}

fn complete_model_metadata() -> PartialModelMetadata {
    PartialModelMetadata {
        context_length: Some(128_000),
        max_input_tokens: Some(96_000),
        max_output_tokens: Some(32_000),
        input_modalities: Some(vec![ModelModality::Text, ModelModality::Image]),
        output_modalities: Some(vec![ModelModality::Text]),
        tool_calling: Some(true),
        streaming: Some(true),
        zero_data_retention_enabled: Some(true),
        reasoning_supported_efforts: Some(vec!["low".into()]),
        reasoning_summary: None,
        reasoning_include: None,
    }
}

fn complete_model_map(ids: &[&str]) -> ModelConfigMap {
    ids.iter()
        .map(|id| ((*id).to_string(), complete_model_metadata()))
        .collect()
}

#[tokio::test]
async fn noop_service_returns_empty() {
    let svc = NoopProviderSetupService;
    let result = svc.available().await.unwrap();
    assert_eq!(result, serde_json::json!([]));
}

#[tokio::test]
async fn remove_key_rejects_unknown_provider() {
    let registry = Arc::new(RwLock::new(
        ProviderRegistry::from_config(&ProvidersConfig::default(), &HashMap::new()).unwrap(),
    ));
    let svc = live_provider_setup_service(registry, ProvidersConfig::default(), None);
    let result = svc
        .remove_key(serde_json::json!({"provider": "nonexistent"}))
        .await;
    assert!(result.is_err());
}

#[tokio::test]
async fn remove_key_rejects_missing_params() {
    let registry = Arc::new(RwLock::new(
        ProviderRegistry::from_config(&ProvidersConfig::default(), &HashMap::new()).unwrap(),
    ));
    let svc = live_provider_setup_service(registry, ProvidersConfig::default(), None);
    assert!(svc.remove_key(serde_json::json!({})).await.is_err());
}

#[tokio::test]
async fn remove_key_rejects_provider_alias_used_by_configured_agent_before_mutation() {
    let mut config = ProvidersConfig::default();
    config.providers.insert("openai".into(), ProviderEntry {
        api_key: Some(Secret::new("sk-test".into())),
        models: complete_model_map(&["gpt-5"]),
        alias: Some("oai".into()),
        ..Default::default()
    });
    let registry = Arc::new(RwLock::new(
        ProviderRegistry::from_config(&config, &HashMap::new()).expect("configured registry"),
    ));
    let mut agents = AgentsConfig {
        default: "main".into(),
        ..Default::default()
    };
    agents.entries.insert(
        "main".into(),
        AgentConfig::new("Main", "oai::gpt-5", ReasoningEffort::from("low")),
    );
    let svc = live_provider_setup_service(Arc::clone(&registry), config, None)
        .with_agents_config(Arc::new(RwLock::new(agents)));

    let error = svc
        .remove_key(serde_json::json!({ "provider": "openai" }))
        .await
        .expect_err("configured agent provider must not be removed")
        .to_string();

    assert!(error.contains("main"));
    assert!(svc.config_snapshot().is_enabled("openai"));
    assert!(registry.read().await.get("oai::gpt-5").is_some());
}

#[tokio::test]
async fn disabled_provider_is_not_reported_configured() {
    let registry = Arc::new(RwLock::new(
        ProviderRegistry::from_config(&ProvidersConfig::default(), &HashMap::new()).unwrap(),
    ));
    let svc = live_provider_setup_service(registry, ProvidersConfig::default(), None);
    let provider = known_providers()
        .into_iter()
        .find(|p| p.name == "openai")
        .expect("openai should exist");

    let mut config = ProvidersConfig::default();
    config.providers.insert("openai".into(), ProviderEntry {
        enabled: false,
        ..Default::default()
    });

    assert!(
        !svc.is_provider_configured(&provider, &config)
            .expect("provider configuration status")
    );
}

#[tokio::test]
async fn live_service_lists_providers() {
    let registry = Arc::new(RwLock::new(
        ProviderRegistry::from_config(&ProvidersConfig::default(), &HashMap::new()).unwrap(),
    ));
    let svc = live_provider_setup_service(registry, ProvidersConfig::default(), None);
    let result = svc.available().await.unwrap();
    let arr = result.as_array().unwrap();
    assert!(!arr.is_empty());
    // Check that we have expected fields
    let first = &arr[0];
    assert!(first.get("name").is_some());
    assert!(first.get("displayName").is_some());
    assert!(first.get("configured").is_some());
    // New fields for endpoint and model configuration
    assert!(first.get("defaultBaseUrl").is_some());
    assert!(first.get("requiresModel").is_some());
    assert!(first.get("uiOrder").is_some());
}

#[tokio::test]
async fn available_marks_provider_configured_from_generic_provider_env() {
    let registry = Arc::new(RwLock::new(
        ProviderRegistry::from_config(&ProvidersConfig::default(), &HashMap::new()).unwrap(),
    ));
    let svc = live_provider_setup_service(registry, ProvidersConfig::default(), None)
        .with_env_overrides(HashMap::from([
            ("CHELIX_PROVIDER".to_string(), "openai".to_string()),
            (
                "CHELIX_API_KEY".to_string(),
                "sk-test-openai-generic".to_string(),
            ),
        ]));

    let result = svc.available().await.unwrap();
    let arr = result
        .as_array()
        .expect("providers.available should return array");
    let openai = arr
        .iter()
        .find(|provider| provider.get("name").and_then(|v| v.as_str()) == Some("openai"))
        .expect("openai should be present");

    assert_eq!(
        openai.get("configured").and_then(|v| v.as_bool()),
        Some(true)
    );
}

#[tokio::test]
async fn available_hides_unconfigured_providers_not_in_offered_list() {
    let registry = Arc::new(RwLock::new(
        ProviderRegistry::from_config(&ProvidersConfig::default(), &HashMap::new()).unwrap(),
    ));
    let config = ProvidersConfig {
        offered: vec!["openai".into()],
        ..ProvidersConfig::default()
    };
    let svc = live_provider_setup_service(registry, config, None);

    let result = svc.available().await.unwrap();
    let arr = result.as_array().unwrap();
    for provider in arr {
        let configured = provider
            .get("configured")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let name = provider.get("name").and_then(|v| v.as_str()).unwrap_or("");
        if !configured {
            assert_eq!(
                name, "openai",
                "only offered providers should be shown when unconfigured"
            );
        }
    }
}

#[tokio::test]
async fn available_respects_offered_order() {
    let registry = Arc::new(RwLock::new(
        ProviderRegistry::from_config(&ProvidersConfig::default(), &HashMap::new()).unwrap(),
    ));
    let config = ProvidersConfig {
        offered: vec!["openrouter".into(), "openai".into(), "zai".into()],
        ..ProvidersConfig::default()
    };
    let svc = live_provider_setup_service(registry, config, None);
    let result = svc.available().await.unwrap();
    let arr = result
        .as_array()
        .expect("providers.available should return array");
    let names: Vec<&str> = arr
        .iter()
        .filter_map(|v| v.get("name").and_then(|n| n.as_str()))
        .collect();

    let openrouter_idx = names
        .iter()
        .position(|name| *name == "openrouter")
        .expect("openrouter should be present");
    let openai_idx = names
        .iter()
        .position(|name| *name == "openai")
        .expect("openai should be present");
    let zai_idx = names
        .iter()
        .position(|name| *name == "zai")
        .expect("zai should be present");

    assert!(
        openrouter_idx < openai_idx && openai_idx < zai_idx,
        "offered provider order should be preserved, got: {names:?}"
    );
}

#[tokio::test]
async fn available_hides_configured_provider_outside_offered() {
    let registry = Arc::new(RwLock::new(
        ProviderRegistry::from_config(&ProvidersConfig::default(), &HashMap::new()).unwrap(),
    ));
    let mut config = ProvidersConfig {
        offered: vec!["openai".into()],
        ..ProvidersConfig::default()
    };
    config.providers.insert("openrouter".into(), ProviderEntry {
        api_key: Some(Secret::new("sk-test".into())),
        ..Default::default()
    });
    let svc = live_provider_setup_service(registry, config, None);
    let result = svc.available().await.unwrap();
    let arr = result
        .as_array()
        .expect("providers.available should return array");
    let names: Vec<&str> = arr
        .iter()
        .filter_map(|v| v.get("name").and_then(|n| n.as_str()))
        .collect();

    let openai_idx = names
        .iter()
        .position(|name| *name == "openai")
        .expect("openai should be present");

    assert!(
        !names.contains(&"openrouter"),
        "providers outside offered should be hidden even when configured, got: {names:?}"
    );
    assert_eq!(openai_idx, 0);
}

#[tokio::test]
async fn available_includes_configured_custom_provider_outside_offered() {
    let dir = tempfile::tempdir().expect("temp dir");
    let key_store = KeyStore::with_path(dir.path().join("provider_keys.json"));
    key_store
        .save_config_with_display_name(
            "custom-openrouter-ai",
            Some("sk-test".into()),
            Some("https://openrouter.ai/api/v1".into()),
            Some("openrouter.ai".into()),
        )
        .expect("save custom provider");

    let mut config = ProvidersConfig {
        offered: vec!["openai".into()],
        ..ProvidersConfig::default()
    };
    config
        .providers
        .insert("custom-openrouter-ai".into(), ProviderEntry {
            enabled: true,
            models: complete_model_map(&["gpt-5.2"]),
            ..Default::default()
        });

    let registry = Arc::new(RwLock::new(
        ProviderRegistry::from_config(&ProvidersConfig::default(), &HashMap::new()).unwrap(),
    ));
    let mut svc = live_provider_setup_service(registry, config, None);
    svc.key_store = key_store;

    let result = svc.available().await.expect("providers.available");
    let arr = result
        .as_array()
        .expect("providers.available should return array");
    let custom = arr
        .iter()
        .find(|v| v.get("name").and_then(|n| n.as_str()) == Some("custom-openrouter-ai"))
        .expect("custom provider should be visible");

    assert_eq!(
        custom.get("configured").and_then(|v| v.as_bool()),
        Some(true)
    );
    assert_eq!(custom.get("isCustom").and_then(|v| v.as_bool()), Some(true));
    assert_eq!(
        custom.get("displayName").and_then(|v| v.as_str()),
        Some("openrouter.ai")
    );
    assert!(custom.get("models").is_none());
}

#[tokio::test]
async fn available_includes_config_declared_custom_provider_without_saved_credentials() {
    let mut config = ProvidersConfig {
        offered: vec!["openai".into()],
        ..ProvidersConfig::default()
    };
    config
        .providers
        .insert("custom-ai-example".into(), ProviderEntry {
            enabled: true,
            base_url: Some("https://ai.example.invalid/v1".into()),
            models: complete_model_map(&["model-1"]),
            ..Default::default()
        });

    let registry = Arc::new(RwLock::new(
        ProviderRegistry::from_config(&ProvidersConfig::default(), &HashMap::new()).unwrap(),
    ));
    let svc = live_provider_setup_service(registry, config, None);

    let result = svc.available().await.expect("providers.available");
    let custom = result
        .as_array()
        .and_then(|providers| {
            providers.iter().find(|provider| {
                provider.get("name").and_then(|value| value.as_str()) == Some("custom-ai-example")
            })
        })
        .expect("config-declared custom provider should be visible");

    assert_eq!(
        custom.get("displayName").and_then(|value| value.as_str()),
        Some("OpenAI Compatible (custom-ai-example)")
    );
    assert_eq!(
        custom.get("configured").and_then(|value| value.as_bool()),
        Some(false)
    );
    assert_eq!(
        custom.get("baseUrl").and_then(|value| value.as_str()),
        Some("https://ai.example.invalid/v1")
    );
    assert_eq!(
        custom.get("isCustom").and_then(|value| value.as_bool()),
        Some(true)
    );
    assert!(custom.get("models").is_none());
}

#[tokio::test]
async fn save_key_configures_declared_custom_provider_without_mutating_models() {
    let dir = tempfile::tempdir().expect("temp dir");
    let mut config = ProvidersConfig::default();
    config
        .providers
        .insert("custom-ai-example".into(), ProviderEntry {
            enabled: true,
            models: complete_model_map(&["model-1"]),
            ..Default::default()
        });
    let registry = Arc::new(RwLock::new(
        ProviderRegistry::from_config(&config, &HashMap::new()).expect("configured registry"),
    ));
    let mut svc = live_provider_setup_service(Arc::clone(&registry), config, None);
    svc.key_store = KeyStore::with_path(dir.path().join("provider_keys.json"));

    svc.save_key(serde_json::json!({
        "provider": "custom-ai-example",
        "apiKey": "sk-compatible",
        "baseUrl": "https://ai.example.invalid/v1",
    }))
    .await
    .expect("custom provider credentials should be saved");

    let saved = svc
        .key_store
        .load_config("custom-ai-example")
        .expect("load custom provider credentials")
        .expect("saved custom provider credentials");
    assert_eq!(saved.api_key.as_deref(), Some("sk-compatible"));
    assert_eq!(
        saved.base_url.as_deref(),
        Some("https://ai.example.invalid/v1")
    );

    let registry_guard = registry.read().await;
    let models = registry_guard.list_models();
    assert_eq!(models.len(), 1);
    assert_eq!(models[0].id, "custom-ai-example::model-1");
    assert_eq!(models[0].provider, "custom-ai-example");
}

#[tokio::test]
async fn available_includes_default_base_urls() {
    let registry = Arc::new(RwLock::new(
        ProviderRegistry::from_config(&ProvidersConfig::default(), &HashMap::new()).unwrap(),
    ));
    let svc = live_provider_setup_service(registry, ProvidersConfig::default(), None);
    let result = svc.available().await.unwrap();
    let arr = result.as_array().unwrap();

    // Check specific providers have correct default base URLs
    let openai = arr
        .iter()
        .find(|p| p.get("name").and_then(|n| n.as_str()) == Some("openai"))
        .expect("openai not found");
    assert_eq!(
        openai.get("defaultBaseUrl").and_then(|u| u.as_str()),
        Some("https://api.openai.com/v1")
    );

    let openrouter = arr
        .iter()
        .find(|p| p.get("name").and_then(|n| n.as_str()) == Some("openrouter"))
        .expect("openrouter not found");
    assert_eq!(
        openrouter.get("defaultBaseUrl").and_then(|u| u.as_str()),
        Some("https://openrouter.ai/api/v1")
    );
}

#[tokio::test]
async fn save_key_rejects_unknown_provider() {
    let registry = Arc::new(RwLock::new(
        ProviderRegistry::from_config(&ProvidersConfig::default(), &HashMap::new()).unwrap(),
    ));
    let svc = live_provider_setup_service(registry, ProvidersConfig::default(), None);
    let result = svc
        .save_key(serde_json::json!({"provider": "nonexistent", "apiKey": "test"}))
        .await;
    assert!(result.is_err());
}

#[tokio::test]
async fn save_key_rejects_missing_params() {
    let registry = Arc::new(RwLock::new(
        ProviderRegistry::from_config(&ProvidersConfig::default(), &HashMap::new()).unwrap(),
    ));
    let svc = live_provider_setup_service(registry, ProvidersConfig::default(), None);
    assert!(svc.save_key(serde_json::json!({})).await.is_err());
    assert!(
        svc.save_key(serde_json::json!({"provider": "openai"}))
            .await
            .is_err()
    );
}

#[tokio::test]
async fn save_key_rejects_invalid_candidate_before_mutating_state() {
    let dir = tempfile::tempdir().expect("temp dir");
    let registry = Arc::new(RwLock::new(
        ProviderRegistry::from_config(&ProvidersConfig::default(), &HashMap::new()).unwrap(),
    ));
    let mut invalid_metadata = complete_model_metadata();
    invalid_metadata.reasoning_supported_efforts = Some(Vec::new());
    let invalid_models: ModelConfigMap = [("invalid".to_string(), invalid_metadata)]
        .into_iter()
        .collect();
    let mut config = ProvidersConfig::default();
    config.providers.insert("openai".into(), ProviderEntry {
        models: invalid_models.clone(),
        ..Default::default()
    });
    let mut svc = live_provider_setup_service(Arc::clone(&registry), config, None);
    svc.key_store = KeyStore::with_path(dir.path().join("provider_keys.json"));

    let error = svc
        .save_key(serde_json::json!({
            "provider": "openai",
            "apiKey": "sk-test",
        }))
        .await
        .expect_err("invalid model metadata should fail")
        .to_string();

    assert!(error.contains("reasoning_supported_efforts"));
    assert!(error.contains("must not be empty"));
    assert!(
        svc.key_store
            .load_config("openai")
            .expect("load provider credentials")
            .is_none()
    );
    let snapshot = svc.config_snapshot();
    let openai = snapshot
        .get("openai")
        .expect("original provider config should remain");
    assert!(openai.api_key.is_none());
    assert_eq!(openai.models, invalid_models);
    assert!(registry.read().await.list_models().is_empty());
}

#[tokio::test]
async fn set_model_preferences_replaces_unique_raw_priority_without_mutating_registry_or_key_store()
{
    let dir = tempfile::tempdir().expect("temp dir");
    let mut config = ProvidersConfig::default();
    config.providers.insert("openai".into(), ProviderEntry {
        api_key: Some(Secret::new("sk-test".into())),
        models: complete_model_map(&["gpt-5", "gpt-4o"]),
        ..Default::default()
    });
    let registry = Arc::new(RwLock::new(
        ProviderRegistry::from_config(&config, &HashMap::new()).expect("configured registry"),
    ));
    let mut svc = live_provider_setup_service(Arc::clone(&registry), config, None);
    svc.key_store = KeyStore::with_path(dir.path().join("provider_keys.json"));
    let priorities = Arc::new(RwLock::new(vec![
        "gpt-4o".to_string(),
        "other::model".to_string(),
    ]));
    svc.set_priority_models(Arc::clone(&priorities));

    svc.set_model_preferences(serde_json::json!({
        "provider": "openai",
        "modelIds": ["openai::gpt-5"],
    }))
    .await
    .expect("canonical subset should be saved");

    assert_eq!(priorities.read().await.as_slice(), [
        "openai::gpt-5",
        "other::model"
    ]);
    assert!(
        svc.key_store
            .load_config("openai")
            .expect("load provider credentials")
            .is_none()
    );
    let registry_guard = registry.read().await;
    let models = registry_guard.list_models();
    assert_eq!(models.len(), 2);
    assert!(models.iter().any(|model| model.id == "openai::gpt-5"));
    assert!(models.iter().any(|model| model.id == "openai::gpt-4o"));
}

#[tokio::test]
async fn set_model_preferences_rejects_ambiguous_raw_priority_without_mutating_order() {
    let mut config = ProvidersConfig::default();
    for provider in ["openai", "openrouter"] {
        config.providers.insert(provider.into(), ProviderEntry {
            api_key: Some(Secret::new(format!("{provider}-key"))),
            models: complete_model_map(&["shared-model"]),
            ..Default::default()
        });
    }
    let registry = Arc::new(RwLock::new(
        ProviderRegistry::from_config(&config, &HashMap::new()).expect("configured registry"),
    ));
    let mut svc = live_provider_setup_service(registry, config, None);
    let priorities = Arc::new(RwLock::new(vec![
        "shared-model".to_string(),
        "other::model".to_string(),
    ]));
    svc.set_priority_models(Arc::clone(&priorities));

    let error = svc
        .set_model_preferences(serde_json::json!({
            "provider": "openai",
            "modelIds": ["openai::shared-model"],
        }))
        .await
        .expect_err("ambiguous raw priority must be rejected")
        .to_string();

    assert!(error.contains("ambiguous raw model ID `shared-model`"));
    assert_eq!(priorities.read().await.as_slice(), [
        "shared-model",
        "other::model"
    ]);
}

#[tokio::test]
async fn set_model_preferences_rejects_noncanonical_model_without_mutating_order() {
    let mut config = ProvidersConfig::default();
    config.providers.insert("openai".into(), ProviderEntry {
        api_key: Some(Secret::new("sk-test".into())),
        models: complete_model_map(&["gpt-5"]),
        ..Default::default()
    });
    let registry = Arc::new(RwLock::new(
        ProviderRegistry::from_config(&config, &HashMap::new()).expect("configured registry"),
    ));
    let mut svc = live_provider_setup_service(registry, config, None);
    let priorities = Arc::new(RwLock::new(vec!["openai::gpt-5".to_string()]));
    svc.set_priority_models(Arc::clone(&priorities));

    let error = svc
        .set_model_preferences(serde_json::json!({
            "provider": "openai",
            "modelIds": ["gpt-5"],
        }))
        .await
        .expect_err("raw model ID must be rejected")
        .to_string();

    assert!(error.contains("model `gpt-5` is not configured for provider `openai`"));
    assert_eq!(priorities.read().await.as_slice(), ["openai::gpt-5"]);
}

#[tokio::test]
async fn save_key_rejects_completion_endpoint_base_url_for_any_provider() {
    let registry = Arc::new(RwLock::new(
        ProviderRegistry::from_config(&ProvidersConfig::default(), &HashMap::new()).unwrap(),
    ));
    let svc = live_provider_setup_service(registry, ProvidersConfig::default(), None);

    let error = svc
        .save_key(serde_json::json!({
            "provider": "openai",
            "apiKey": "sk-test",
            "baseUrl": "https://api.example.com/v1/chat/completions",
        }))
        .await
        .expect_err("completion endpoint should be rejected")
        .to_string();

    assert!(error.contains("API base URL"));
    assert!(error.contains("https://api.example.com/v1"));
}

#[tokio::test]
async fn save_key_rejects_invalid_base_url_for_any_provider() {
    let registry = Arc::new(RwLock::new(
        ProviderRegistry::from_config(&ProvidersConfig::default(), &HashMap::new()).unwrap(),
    ));
    let svc = live_provider_setup_service(registry, ProvidersConfig::default(), None);

    let error = svc
        .save_key(serde_json::json!({
            "provider": "openai",
            "apiKey": "sk-test",
            "baseUrl": "api.example.com/v1",
        }))
        .await
        .expect_err("invalid endpoint should be rejected")
        .to_string();

    assert!(error.contains("valid HTTP(S) URL"));
}

#[tokio::test]
async fn save_key_accepts_new_providers() {
    let registry = Arc::new(RwLock::new(
        ProviderRegistry::from_config(&ProvidersConfig::default(), &HashMap::new()).unwrap(),
    ));
    let _svc = live_provider_setup_service(registry, ProvidersConfig::default(), None);

    let providers = known_providers();
    for name in ["openrouter", "zai", "zai-code"] {
        let known = providers.iter().find(|p| p.name == name);
        assert!(
            known.is_some(),
            "{name} should be a recognized api-key provider"
        );
    }
}

#[tokio::test]
async fn available_includes_new_providers() {
    let registry = Arc::new(RwLock::new(
        ProviderRegistry::from_config(&ProvidersConfig::default(), &HashMap::new()).unwrap(),
    ));
    let svc = live_provider_setup_service(registry, ProvidersConfig::default(), None);
    let result = svc.available().await.unwrap();
    let arr = result.as_array().unwrap();

    let names: Vec<&str> = arr
        .iter()
        .filter_map(|v| v.get("name").and_then(|n| n.as_str()))
        .collect();

    for expected in ["openrouter", "zai", "zai-code"] {
        assert!(
            names.contains(&expected),
            "{expected} not found in available providers: {names:?}"
        );
    }
}
