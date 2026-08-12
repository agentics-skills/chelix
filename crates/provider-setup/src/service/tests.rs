#![allow(clippy::unwrap_used, clippy::expect_used)]

use {
    super::*,
    crate::KeyStore,
    chelix_config::schema::{
        ModelConfigMap, ModelModality, PartialModelMetadata, PartialReasoningMetadata,
        ProviderEntry, ProvidersConfig,
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
        reasoning: Some(PartialReasoningMetadata {
            supported_efforts: Some(Vec::new()),
            ..Default::default()
        }),
    }
}

fn complete_model_map(ids: &[&str]) -> ModelConfigMap {
    ids.iter()
        .map(|id| ((*id).to_string(), complete_model_metadata()))
        .collect()
}

fn discovered_model_record(
    id: &str,
    created: i64,
    output_modalities: &[&str],
) -> serde_json::Value {
    serde_json::json!({
        "id": id,
        "object": "model",
        "created": created,
        "context_length": 128_000,
        "max_input_tokens": 96_000,
        "max_output_tokens": 32_000,
        "input_modalities": ["text", "image"],
        "output_modalities": output_modalities,
        "tool_calling": true,
        "streaming": true,
        "zeroDataRetentionEnabled": true,
        "reasoning": { "supported_efforts": [] }
    })
}

#[tokio::test]
async fn noop_service_returns_empty() {
    let svc = NoopProviderSetupService;
    let result = svc.available().await.unwrap();
    assert_eq!(result, serde_json::json!([]));
}

#[tokio::test]
async fn remove_key_rejects_unknown_provider() {
    let registry = Arc::new(RwLock::new(ProviderRegistry::from_config(
        &ProvidersConfig::default(),
        &HashMap::new(),
    )));
    let svc = live_provider_setup_service(registry, ProvidersConfig::default(), None);
    let result = svc
        .remove_key(serde_json::json!({"provider": "nonexistent"}))
        .await;
    assert!(result.is_err());
}

#[tokio::test]
async fn remove_key_rejects_missing_params() {
    let registry = Arc::new(RwLock::new(ProviderRegistry::from_config(
        &ProvidersConfig::default(),
        &HashMap::new(),
    )));
    let svc = live_provider_setup_service(registry, ProvidersConfig::default(), None);
    assert!(svc.remove_key(serde_json::json!({})).await.is_err());
}

#[tokio::test]
async fn disabled_provider_is_not_reported_configured() {
    let registry = Arc::new(RwLock::new(ProviderRegistry::from_config(
        &ProvidersConfig::default(),
        &HashMap::new(),
    )));
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

    assert!(!svc.is_provider_configured(&provider, &config));
}

#[tokio::test]
async fn live_service_lists_providers() {
    let registry = Arc::new(RwLock::new(ProviderRegistry::from_config(
        &ProvidersConfig::default(),
        &HashMap::new(),
    )));
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
    let registry = Arc::new(RwLock::new(ProviderRegistry::from_config(
        &ProvidersConfig::default(),
        &HashMap::new(),
    )));
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
    let registry = Arc::new(RwLock::new(ProviderRegistry::from_config(
        &ProvidersConfig::default(),
        &HashMap::new(),
    )));
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
    let registry = Arc::new(RwLock::new(ProviderRegistry::from_config(
        &ProvidersConfig::default(),
        &HashMap::new(),
    )));
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
    let registry = Arc::new(RwLock::new(ProviderRegistry::from_config(
        &ProvidersConfig::default(),
        &HashMap::new(),
    )));
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
            Some(complete_model_map(&["gpt-5.2"])),
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
            ..Default::default()
        });

    let registry = Arc::new(RwLock::new(ProviderRegistry::from_config(
        &ProvidersConfig::default(),
        &HashMap::new(),
    )));
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
    let models = custom
        .get("models")
        .and_then(|value| value.as_object())
        .expect("models should use the canonical object-map contract");
    assert_eq!(models.keys().map(String::as_str).collect::<Vec<_>>(), vec![
        "gpt-5.2"
    ]);
    let metadata = models
        .get("gpt-5.2")
        .and_then(|value| value.as_object())
        .expect("model metadata should be an object");
    assert_eq!(
        metadata
            .get("context_length")
            .and_then(|value| value.as_u64()),
        Some(128_000)
    );
    assert_eq!(
        metadata
            .get("reasoning")
            .and_then(|value| value.get("supported_efforts"))
            .and_then(|value| value.as_array()),
        Some(&Vec::new())
    );
}

#[tokio::test]
async fn available_serializes_config_first_ordered_model_map() {
    let dir = tempfile::tempdir().expect("temp dir");
    let key_store = KeyStore::with_path(dir.path().join("provider_keys.json"));
    let mut saved_models = complete_model_map(&["saved-only", "gpt-5", "gpt-4o"]);
    saved_models
        .get_mut("gpt-5")
        .expect("saved gpt-5 metadata")
        .context_length = Some(128_000);
    key_store
        .save_config("openai", Some("sk-test".into()), None, Some(saved_models))
        .expect("save provider config");

    let mut configured_models = ModelConfigMap::new();
    configured_models.insert("gpt-5".into(), PartialModelMetadata {
        context_length: Some(256_000),
        ..Default::default()
    });
    configured_models.insert("gpt-4o".into(), PartialModelMetadata {
        tool_calling: Some(false),
        ..Default::default()
    });
    let mut config = ProvidersConfig::default();
    config.providers.insert("openai".into(), ProviderEntry {
        models: configured_models,
        ..Default::default()
    });

    let registry = Arc::new(RwLock::new(ProviderRegistry::from_config(
        &ProvidersConfig::default(),
        &HashMap::new(),
    )));
    let mut svc = live_provider_setup_service(registry, config, None);
    svc.key_store = key_store;

    let result = svc.available().await.expect("providers.available");
    let openai = result
        .as_array()
        .and_then(|providers| {
            providers.iter().find(|provider| {
                provider.get("name").and_then(|value| value.as_str()) == Some("openai")
            })
        })
        .expect("openai should be present");
    let models = openai
        .get("models")
        .and_then(|value| value.as_object())
        .expect("models should be an object map");

    assert_eq!(models.keys().map(String::as_str).collect::<Vec<_>>(), vec![
        "gpt-5", "gpt-4o"
    ]);
    assert_eq!(
        models
            .get("gpt-5")
            .and_then(|value| value.get("context_length"))
            .and_then(|value| value.as_u64()),
        Some(256_000)
    );
    assert_eq!(
        models
            .get("gpt-5")
            .and_then(|value| value.get("max_input_tokens"))
            .and_then(|value| value.as_u64()),
        Some(96_000)
    );
    assert_eq!(
        models
            .get("gpt-4o")
            .and_then(|value| value.get("tool_calling"))
            .and_then(|value| value.as_bool()),
        Some(false)
    );
    assert!(models.get("saved-only").is_none());
}

#[tokio::test]
async fn available_includes_default_base_urls() {
    let registry = Arc::new(RwLock::new(ProviderRegistry::from_config(
        &ProvidersConfig::default(),
        &HashMap::new(),
    )));
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
    let registry = Arc::new(RwLock::new(ProviderRegistry::from_config(
        &ProvidersConfig::default(),
        &HashMap::new(),
    )));
    let svc = live_provider_setup_service(registry, ProvidersConfig::default(), None);
    let result = svc
        .save_key(serde_json::json!({"provider": "nonexistent", "apiKey": "test"}))
        .await;
    assert!(result.is_err());
}

#[tokio::test]
async fn save_key_rejects_missing_params() {
    let registry = Arc::new(RwLock::new(ProviderRegistry::from_config(
        &ProvidersConfig::default(),
        &HashMap::new(),
    )));
    let svc = live_provider_setup_service(registry, ProvidersConfig::default(), None);
    assert!(svc.save_key(serde_json::json!({})).await.is_err());
    assert!(
        svc.save_key(serde_json::json!({"provider": "openai"}))
            .await
            .is_err()
    );
}

#[tokio::test]
async fn save_key_rejects_completion_endpoint_base_url_for_any_provider() {
    let registry = Arc::new(RwLock::new(ProviderRegistry::from_config(
        &ProvidersConfig::default(),
        &HashMap::new(),
    )));
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
    let registry = Arc::new(RwLock::new(ProviderRegistry::from_config(
        &ProvidersConfig::default(),
        &HashMap::new(),
    )));
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
async fn add_custom_rejects_completion_endpoint_base_url() {
    let registry = Arc::new(RwLock::new(ProviderRegistry::from_config(
        &ProvidersConfig::default(),
        &HashMap::new(),
    )));
    let svc = live_provider_setup_service(registry, ProvidersConfig::default(), None);

    let error = svc
        .add_custom(serde_json::json!({
            "apiKey": "sk-test",
            "baseUrl": "https://api.deepinfra.com/v1/openai/chat/completions",
        }))
        .await
        .expect_err("custom completion endpoint should be rejected")
        .to_string();

    assert!(error.contains("API base URL"));
    assert!(error.contains("https://api.deepinfra.com/v1/openai"));
}

#[tokio::test]
async fn validate_key_rejects_custom_completion_endpoint_base_url() {
    let registry = Arc::new(RwLock::new(ProviderRegistry::from_config(
        &ProvidersConfig::default(),
        &HashMap::new(),
    )));
    let svc = live_provider_setup_service(registry, ProvidersConfig::default(), None);

    let error = svc
        .validate_key(serde_json::json!({
            "provider": "custom-deepinfra-com",
            "apiKey": "sk-test",
            "baseUrl": "https://api.deepinfra.com/v1/openai/chat/completions",
        }))
        .await
        .expect_err("custom completion endpoint validation should be rejected")
        .to_string();

    assert!(error.contains("API base URL"));
    assert!(error.contains("https://api.deepinfra.com/v1/openai"));
}

#[tokio::test]
async fn save_key_accepts_new_providers() {
    let registry = Arc::new(RwLock::new(ProviderRegistry::from_config(
        &ProvidersConfig::default(),
        &HashMap::new(),
    )));
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
    let registry = Arc::new(RwLock::new(ProviderRegistry::from_config(
        &ProvidersConfig::default(),
        &HashMap::new(),
    )));
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

#[tokio::test]
async fn validate_key_rejects_unknown_provider() {
    let registry = Arc::new(RwLock::new(ProviderRegistry::from_config(
        &ProvidersConfig::default(),
        &HashMap::new(),
    )));
    let svc = live_provider_setup_service(registry, ProvidersConfig::default(), None);
    let result = svc
        .validate_key(serde_json::json!({"provider": "nonexistent", "apiKey": "sk-test"}))
        .await;
    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("unknown provider"));
}

#[tokio::test]
async fn validate_key_rejects_missing_provider_param() {
    let registry = Arc::new(RwLock::new(ProviderRegistry::from_config(
        &ProvidersConfig::default(),
        &HashMap::new(),
    )));
    let svc = live_provider_setup_service(registry, ProvidersConfig::default(), None);
    let result = svc.validate_key(serde_json::json!({})).await;
    assert!(result.is_err());
    assert!(
        result
            .unwrap_err()
            .to_string()
            .contains("missing 'provider'")
    );
}

#[tokio::test]
async fn validate_key_rejects_missing_api_key_for_api_key_provider() {
    let registry = Arc::new(RwLock::new(ProviderRegistry::from_config(
        &ProvidersConfig::default(),
        &HashMap::new(),
    )));
    let svc = live_provider_setup_service(registry, ProvidersConfig::default(), None);
    let result = svc
        .validate_key(serde_json::json!({"provider": "openai"}))
        .await;
    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("missing 'apiKey'"));
}

#[tokio::test]
async fn validate_key_custom_provider_without_model_returns_discovered_models() {
    use axum::{Json, Router, routing::get};

    let app = Router::new().route(
        "/models",
        get(|| async {
            Json(serde_json::json!({
                "data": [
                    discovered_model_record("gpt-4o", 1_700_000_000, &["text"]),
                    discovered_model_record("gpt-4o-mini", 1_700_000_001, &["text"]),
                    discovered_model_record("image-output", 1_700_000_002, &["image"])
                ]
            }))
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind listener");
    let addr = listener.local_addr().expect("local addr");
    let server = tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });

    let registry = Arc::new(RwLock::new(ProviderRegistry::from_config(
        &ProvidersConfig::default(),
        &HashMap::new(),
    )));
    let svc = live_provider_setup_service(registry, ProvidersConfig::default(), None);
    let result = svc
        .validate_key(serde_json::json!({
            "provider": "custom-test-server",
            "apiKey": "sk-test",
            "baseUrl": format!("http://{addr}")
        }))
        .await
        .expect("validate_key should return payload");
    server.abort();

    assert_eq!(result.get("valid").and_then(|v| v.as_bool()), Some(true));
    let models = result
        .get("models")
        .and_then(|v| v.as_array())
        .expect("models array should be present");
    assert!(
        models
            .iter()
            .any(|m| m.get("id").and_then(|v| v.as_str()) == Some("custom-test-server::gpt-4o"))
    );
    assert!(
        models.iter().any(
            |m| m.get("id").and_then(|v| v.as_str()) == Some("custom-test-server::gpt-4o-mini")
        )
    );
    assert!(
        !models
            .iter()
            .any(|m| m.get("id").and_then(|v| v.as_str())
                == Some("custom-test-server::image-output"))
    );
}

#[tokio::test]
async fn validate_key_custom_provider_uses_saved_base_url_when_request_omits_it() {
    use axum::{Json, Router, routing::get};

    let app = Router::new().route(
        "/models",
        get(|| async {
            Json(serde_json::json!({
                "data": [
                    discovered_model_record("gpt-4o-mini", 1_700_000_001, &["text"])
                ]
            }))
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind listener");
    let addr = listener.local_addr().expect("local addr");
    let server = tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });

    let registry = Arc::new(RwLock::new(ProviderRegistry::from_config(
        &ProvidersConfig::default(),
        &HashMap::new(),
    )));
    let svc = live_provider_setup_service(registry, ProvidersConfig::default(), None);
    svc.key_store
        .save_config(
            "custom-test-server",
            Some("sk-saved".into()),
            Some(format!("http://{addr}")),
            None,
        )
        .expect("save custom provider config");

    let result = svc
        .validate_key(serde_json::json!({
            "provider": "custom-test-server",
            "apiKey": "sk-test"
        }))
        .await
        .expect("validate_key should return payload");
    server.abort();

    assert_eq!(result.get("valid").and_then(|v| v.as_bool()), Some(true));
    let models = result
        .get("models")
        .and_then(|v| v.as_array())
        .expect("models array should be present");
    assert!(
        models.iter().any(
            |m| m.get("id").and_then(|v| v.as_str()) == Some("custom-test-server::gpt-4o-mini")
        ),
        "expected discovered model via saved base_url, got: {models:?}"
    );
}

#[tokio::test]
async fn validate_key_custom_provider_discovery_error_returns_invalid() {
    use axum::{Router, http::StatusCode, routing::get};

    let app = Router::new().route(
        "/models",
        get(|| async { StatusCode::INTERNAL_SERVER_ERROR }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind listener");
    let addr = listener.local_addr().expect("local addr");
    let server = tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });

    let registry = Arc::new(RwLock::new(ProviderRegistry::from_config(
        &ProvidersConfig::default(),
        &HashMap::new(),
    )));
    let svc = live_provider_setup_service(registry, ProvidersConfig::default(), None);
    let result = svc
        .validate_key(serde_json::json!({
            "provider": "custom-test-server",
            "apiKey": "sk-test",
            "baseUrl": format!("http://{addr}")
        }))
        .await
        .expect("validate_key should return payload");
    server.abort();

    assert_eq!(result.get("valid").and_then(|v| v.as_bool()), Some(false));
    let error = result.get("error").and_then(|v| v.as_str()).unwrap_or("");
    assert!(
        error.contains("Failed to discover models"),
        "unexpected error: {error}"
    );
}

#[tokio::test]
async fn validate_key_custom_provider_returns_discovered_models_without_probing() {
    use {
        axum::{
            Json, Router,
            http::StatusCode,
            routing::{get, post},
        },
        std::sync::atomic::{AtomicBool, Ordering},
    };

    let completions_called = Arc::new(AtomicBool::new(false));
    let cc1 = completions_called.clone();
    let cc2 = completions_called.clone();

    let app = Router::new()
        .route(
            "/models",
            get(|| async {
                Json(serde_json::json!({
                    "data": [
                        discovered_model_record("llama-3.1-70b", 1_700_000_000, &["text"]),
                    ]
                }))
            }),
        )
        .route(
            "/chat/completions",
            post(move || async move {
                cc1.store(true, Ordering::SeqCst);
                StatusCode::INTERNAL_SERVER_ERROR
            }),
        )
        .route(
            "/v1/chat/completions",
            post(move || async move {
                cc2.store(true, Ordering::SeqCst);
                StatusCode::INTERNAL_SERVER_ERROR
            }),
        );

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind listener");
    let addr = listener.local_addr().expect("local addr");
    let server = tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });

    let registry = Arc::new(RwLock::new(ProviderRegistry::from_config(
        &ProvidersConfig::default(),
        &HashMap::new(),
    )));
    let svc = live_provider_setup_service(registry, ProvidersConfig::default(), None);
    let result = svc
        .validate_key(serde_json::json!({
            "provider": "custom-test-server",
            "apiKey": "sk-test",
            "baseUrl": format!("http://{addr}")
        }))
        .await
        .expect("validate_key should return payload");
    server.abort();

    assert_eq!(result.get("valid").and_then(|v| v.as_bool()), Some(true));
    assert!(
        result.get("models").and_then(|v| v.as_array()).is_some(),
        "should return discovered models"
    );
    assert!(
        !completions_called.load(Ordering::SeqCst),
        "chat completions endpoint must NOT be called when model is unset — \
         the discovery path should return models directly (issue #502)"
    );
}

#[tokio::test]
async fn validate_key_custom_provider_connection_refused_returns_error() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind listener");
    let addr = listener.local_addr().expect("local addr");
    drop(listener);

    let registry = Arc::new(RwLock::new(ProviderRegistry::from_config(
        &ProvidersConfig::default(),
        &HashMap::new(),
    )));
    let svc = live_provider_setup_service(registry, ProvidersConfig::default(), None);
    let result = svc
        .validate_key(serde_json::json!({
            "provider": "custom-test-server",
            "apiKey": "sk-test",
            "baseUrl": format!("http://{addr}")
        }))
        .await
        .expect("validate_key should return payload");

    assert_eq!(result.get("valid").and_then(|v| v.as_bool()), Some(false));
    let error = result.get("error").and_then(|v| v.as_str()).unwrap_or("");
    assert!(
        error.contains("Failed to discover models"),
        "should report discovery failure, got: {error}"
    );
}
