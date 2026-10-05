#![allow(clippy::unwrap_used, clippy::expect_used)]

use {
    super::*,
    chelix_config::{
        AgentConfig, AgentsConfig,
        schema::{
            ModelConfigMap, ModelModality, PartialModelMetadata, ProviderEntry, ProvidersConfig,
            ReasoningEffort,
        },
    },
    chelix_providers::ProviderRegistry,
    chelix_service_traits::{NoopProviderSetupService, ProviderSetupService},
    secrecy::{ExposeSecret, Secret},
    std::{collections::HashMap, sync::Arc},
    tokio::sync::RwLock,
};

fn live_provider_setup_service(
    registry: Arc<RwLock<ProviderRegistry>>,
    config: ProvidersConfig,
) -> LiveProviderSetupService {
    LiveProviderSetupService::new(registry, config, ProviderConfigPersistence::MemoryOnly)
}

fn complete_model_metadata() -> PartialModelMetadata {
    PartialModelMetadata {
        context_length: Some(128_000),
        max_input_tokens: Some(96_000),
        max_output_tokens: Some(32_000),
        input_modalities: Some(vec![ModelModality::Text, ModelModality::Image]),
        output_modalities: Some(vec![ModelModality::Text]),
        tool_calling: Some(true),
        zero_data_retention_enabled: Some(true),
        reasoning_supported_efforts: Some(vec!["low".into()]),
        reasoning_summary: None,
        reasoning_include: None,
        enabled: true,
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
    let svc = live_provider_setup_service(registry, ProvidersConfig::default());
    let result = svc
        .remove_key(serde_json::json!({"provider": "voice-openai"}))
        .await;
    assert!(result.is_err());
}

#[tokio::test]
async fn remove_key_rejects_missing_params() {
    let registry = Arc::new(RwLock::new(
        ProviderRegistry::from_config(&ProvidersConfig::default(), &HashMap::new()).unwrap(),
    ));
    let svc = live_provider_setup_service(registry, ProvidersConfig::default());
    assert!(svc.remove_key(serde_json::json!({})).await.is_err());
}

#[tokio::test]
async fn remove_key_rejects_provider_alias_used_by_configured_agent_before_mutation() {
    let mut config = ProvidersConfig::default();
    config.providers.insert("openai".into(), ProviderEntry {
        api_key: Some(Secret::new("sk-test".into())),
        base_url: Some("https://api.example.invalid/v1".into()),
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
        AgentConfig::new("Main", "openai::gpt-5", ReasoningEffort::from("low")),
    );
    let svc = live_provider_setup_service(Arc::clone(&registry), config)
        .with_agents_config(Arc::new(RwLock::new(agents)));

    let error = svc
        .remove_key(serde_json::json!({ "provider": "openai" }))
        .await
        .expect_err("configured agent provider must not be removed")
        .to_string();

    assert!(error.contains("main"));
    assert!(svc.config_snapshot().is_enabled("openai"));
    assert!(registry.read().await.get("openai::gpt-5").is_some());
}

#[tokio::test]
async fn live_service_lists_providers() {
    let registry = Arc::new(RwLock::new(
        ProviderRegistry::from_config(&ProvidersConfig::default(), &HashMap::new()).unwrap(),
    ));
    let svc = live_provider_setup_service(registry, ProvidersConfig::default());
    let result = svc.available().await.unwrap();
    let arr = result.as_array().unwrap();
    assert!(arr.is_empty());
}

#[tokio::test]
async fn available_respects_offered_order() {
    let mut config = ProvidersConfig {
        offered: vec!["openrouter".into(), "openai".into(), "zai".into()],
        ..ProvidersConfig::default()
    };
    for name in ["openrouter", "openai", "zai"] {
        config.providers.insert(name.into(), ProviderEntry {
            base_url: Some(format!("https://{name}.example.invalid/v1")),
            ..Default::default()
        });
    }
    let registry = Arc::new(RwLock::new(
        ProviderRegistry::from_config(&config, &HashMap::new()).unwrap(),
    ));
    let svc = live_provider_setup_service(registry, config);
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
async fn available_includes_configured_openai_compatible_provider_outside_offered() {
    let mut config = ProvidersConfig {
        offered: vec!["openai".into()],
        ..ProvidersConfig::default()
    };
    config
        .providers
        .insert("openrouter-ai".into(), ProviderEntry {
            enabled: true,
            api_key: Some(Secret::new("sk-test".into())),
            base_url: Some("https://openrouter.ai/api/v1".into()),
            models: complete_model_map(&["gpt-5.2"]),
            ..Default::default()
        });

    let registry = Arc::new(RwLock::new(
        ProviderRegistry::from_config(&ProvidersConfig::default(), &HashMap::new()).unwrap(),
    ));
    let svc = live_provider_setup_service(registry, config);

    let result = svc.available().await.expect("providers.available");
    let arr = result
        .as_array()
        .expect("providers.available should return array");
    let provider = arr
        .iter()
        .find(|v| v.get("name").and_then(|n| n.as_str()) == Some("openrouter-ai"))
        .expect("provider should be visible");

    assert_eq!(
        provider.get("configured").and_then(|v| v.as_bool()),
        Some(true)
    );
    assert_eq!(
        provider.get("isOpenAiCompatible").and_then(|v| v.as_bool()),
        Some(true)
    );
    assert_eq!(
        provider.get("displayName").and_then(|v| v.as_str()),
        Some("openrouter-ai")
    );
    assert!(provider.get("models").is_none());
}

#[tokio::test]
async fn available_includes_config_declared_openai_compatible_provider_without_saved_credentials() {
    let mut config = ProvidersConfig {
        offered: vec!["openai".into()],
        ..ProvidersConfig::default()
    };
    config.providers.insert("ai-example".into(), ProviderEntry {
        enabled: true,
        base_url: Some("https://ai.example.invalid/v1".into()),
        models: complete_model_map(&["model-1"]),
        ..Default::default()
    });

    let registry = Arc::new(RwLock::new(
        ProviderRegistry::from_config(&ProvidersConfig::default(), &HashMap::new()).unwrap(),
    ));
    let svc = live_provider_setup_service(registry, config);

    let result = svc.available().await.expect("providers.available");
    let provider = result
        .as_array()
        .and_then(|providers| {
            providers.iter().find(|provider| {
                provider.get("name").and_then(|value| value.as_str()) == Some("ai-example")
            })
        })
        .expect("config-declared provider should be visible");

    assert_eq!(
        provider.get("displayName").and_then(|value| value.as_str()),
        Some("ai-example")
    );
    assert_eq!(
        provider.get("configured").and_then(|value| value.as_bool()),
        Some(false)
    );
    assert_eq!(
        provider.get("baseUrl").and_then(|value| value.as_str()),
        Some("https://ai.example.invalid/v1")
    );
    assert_eq!(
        provider
            .get("isOpenAiCompatible")
            .and_then(|value| value.as_bool()),
        Some(true)
    );
    assert!(provider.get("models").is_none());
}

#[tokio::test]
async fn save_key_configures_declared_openai_compatible_provider_without_mutating_models() {
    let mut config = ProvidersConfig::default();
    config.providers.insert("ai-example".into(), ProviderEntry {
        enabled: true,
        models: complete_model_map(&["model-1"]),
        ..Default::default()
    });
    let registry = Arc::new(RwLock::new(
        ProviderRegistry::from_config(&config, &HashMap::new()).expect("configured registry"),
    ));
    let svc = live_provider_setup_service(Arc::clone(&registry), config);

    svc.save_key(serde_json::json!({
        "provider": "ai-example",
        "apiKey": "sk-compatible",
        "baseUrl": "https://ai.example.invalid/v1",
    }))
    .await
    .expect("custom provider credentials should be saved");

    let saved = svc
        .config_snapshot()
        .get("ai-example")
        .expect("saved custom provider credentials")
        .clone();
    assert_eq!(
        saved
            .api_key
            .as_ref()
            .map(|key| key.expose_secret().as_str()),
        Some("sk-compatible")
    );
    assert_eq!(
        saved.base_url.as_deref(),
        Some("https://ai.example.invalid/v1")
    );

    let registry_guard = registry.read().await;
    let models = registry_guard.list_models();
    assert_eq!(models.len(), 1);
    assert_eq!(models[0].id, "ai-example::model-1");
    assert_eq!(models[0].provider, "ai-example");
}

#[tokio::test]
async fn save_key_rejects_unknown_provider() {
    let registry = Arc::new(RwLock::new(
        ProviderRegistry::from_config(&ProvidersConfig::default(), &HashMap::new()).unwrap(),
    ));
    let svc = live_provider_setup_service(registry, ProvidersConfig::default());
    let result = svc
        .save_key(serde_json::json!({"provider": "voice-openai", "apiKey": "test"}))
        .await;
    assert!(result.is_err());
}

#[tokio::test]
async fn save_key_rejects_missing_params() {
    let registry = Arc::new(RwLock::new(
        ProviderRegistry::from_config(&ProvidersConfig::default(), &HashMap::new()).unwrap(),
    ));
    let svc = live_provider_setup_service(registry, ProvidersConfig::default());
    assert!(svc.save_key(serde_json::json!({})).await.is_err());
    assert!(
        svc.save_key(serde_json::json!({"provider": "openai"}))
            .await
            .is_err()
    );
}

#[tokio::test]
async fn save_key_rejects_invalid_candidate_before_mutating_state() {
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
    let svc = live_provider_setup_service(Arc::clone(&registry), config);

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
    let snapshot = svc.config_snapshot();
    let openai = snapshot
        .get("openai")
        .expect("original provider config should remain");
    assert!(openai.api_key.is_none());
    assert_eq!(openai.models, invalid_models);
    assert!(registry.read().await.list_models().is_empty());
}

#[tokio::test]
async fn set_model_preferences_replaces_unique_raw_priority_without_mutating_registry() {
    let mut config = ProvidersConfig::default();
    config.providers.insert("openai".into(), ProviderEntry {
        api_key: Some(Secret::new("sk-test".into())),
        base_url: Some("https://api.example.invalid/v1".into()),
        models: complete_model_map(&["gpt-5", "gpt-4o"]),
        ..Default::default()
    });
    let registry = Arc::new(RwLock::new(
        ProviderRegistry::from_config(&config, &HashMap::new()).expect("configured registry"),
    ));
    let mut svc = live_provider_setup_service(Arc::clone(&registry), config);
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
            base_url: Some("https://api.example.invalid/v1".into()),
            models: complete_model_map(&["shared-model"]),
            ..Default::default()
        });
    }
    let registry = Arc::new(RwLock::new(
        ProviderRegistry::from_config(&config, &HashMap::new()).expect("configured registry"),
    ));
    let mut svc = live_provider_setup_service(registry, config);
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
        base_url: Some("https://api.example.invalid/v1".into()),
        models: complete_model_map(&["gpt-5"]),
        ..Default::default()
    });
    let registry = Arc::new(RwLock::new(
        ProviderRegistry::from_config(&config, &HashMap::new()).expect("configured registry"),
    ));
    let mut svc = live_provider_setup_service(registry, config);
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
    let svc = live_provider_setup_service(registry, ProvidersConfig::default());

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
    let svc = live_provider_setup_service(registry, ProvidersConfig::default());

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
