//! Live integration tests for scenario-driven tool argument serialization
//! checks on OpenAI-compatible providers.
//!
//! Requires provider credentials in the environment. Run with:
//!   cargo test -p chelix-providers --test tool_arg_serialization_integration -- --ignored --nocapture

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::{collections::BTreeMap, time::Duration};

use {
    chelix_agents::model::{ChatMessage, LlmProvider},
    chelix_providers::openai::OpenAiProvider,
    secrecy::Secret,
    serde::Deserialize,
};

const SCENARIOS_JSON: &str = include_str!("e2e_scenarios/tool_arg_serialization.json");

#[derive(Debug, Deserialize)]
struct ScenarioSuite {
    scenarios: Vec<Scenario>,
}

#[derive(Debug, Deserialize)]
struct Scenario {
    id: String,
    prompt: String,
    tool: serde_json::Value,
    expected_arguments: serde_json::Value,
    providers: BTreeMap<String, ProviderExpectation>,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case")]
enum ProviderExpectation {
    MustPass,
}

#[derive(Debug)]
struct ProviderConfig {
    provider_name: &'static str,
    api_key_env: &'static str,
    base_url_env: Option<&'static str>,
    default_base_url: &'static str,
    model_env: &'static str,
    default_model: &'static str,
}

fn load_suite() -> ScenarioSuite {
    serde_json::from_str(SCENARIOS_JSON).expect("scenario file must be valid JSON")
}

fn optional_var(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.trim().is_empty())
}

fn is_absolute_http_url(value: &str) -> bool {
    value.starts_with("https://") || value.starts_with("http://")
}

fn configured_base_url_from_override(
    config: &ProviderConfig,
    env_base_url: Option<String>,
) -> Option<String> {
    if let Some(base_url) = env_base_url {
        if is_absolute_http_url(&base_url) {
            return Some(base_url);
        }
        eprintln!(
            "ignoring invalid {} override for {}: expected absolute http(s) URL",
            config.base_url_env.unwrap_or("base URL"),
            config.provider_name
        );
    }

    (!config.default_base_url.is_empty()).then(|| config.default_base_url.to_string())
}

fn configured_base_url(config: &ProviderConfig) -> Option<String> {
    configured_base_url_from_override(config, config.base_url_env.and_then(optional_var))
}

fn configured_provider(config: &ProviderConfig) -> Option<OpenAiProvider> {
    let api_key = optional_var(config.api_key_env)?;
    let base_url = configured_base_url(config)?;
    let model = optional_var(config.model_env).unwrap_or_else(|| config.default_model.to_string());

    Some(OpenAiProvider::new_with_name(
        Secret::new(api_key),
        model,
        base_url,
        config.provider_name.to_string(),
    ))
}

fn assert_provider_result(
    provider_name: &str,
    scenario: &Scenario,
    actual_arguments: &serde_json::Value,
) {
    let expectation = scenario.providers.get(provider_name).unwrap_or_else(|| {
        panic!(
            "scenario {} missing provider {}",
            scenario.id, provider_name
        )
    });

    match expectation {
        ProviderExpectation::MustPass => {
            assert_eq!(
                actual_arguments, &scenario.expected_arguments,
                "provider {provider_name} failed scenario {}",
                scenario.id
            );
        },
    }
}

fn is_transient_provider_error(error: &anyhow::Error) -> bool {
    let message = error.to_string().to_ascii_lowercase();
    message.contains("http 429")
        || message.contains("http 503")
        || message.contains("temporarily rate-limited")
        || message.contains("upstream connect error")
        || message.contains("connection termination")
}

fn is_account_unavailable_provider_error(provider_name: &str, error: &anyhow::Error) -> bool {
    if provider_name != "alibaba-coding" {
        return false;
    }

    let message = error.to_string().to_ascii_lowercase();
    message.contains("http 401")
        && (message.contains("invalid_api_key") || message.contains("invalid access token"))
}

async fn complete_scenario_with_retries(
    provider: &OpenAiProvider,
    provider_name: &str,
    scenario: &Scenario,
) -> Option<chelix_agents::model::CompletionResponse> {
    for attempt in 0..3 {
        let result = provider
            .complete(
                &[
                    ChatMessage::system(
                        "Call the provided tool exactly once. Do not answer with prose.",
                    ),
                    ChatMessage::user(scenario.prompt.clone()),
                ],
                std::slice::from_ref(&scenario.tool),
            )
            .await;

        match result {
            Ok(response) => return Some(response),
            Err(error) if attempt < 2 && is_transient_provider_error(&error) => {
                tokio::time::sleep(Duration::from_secs(2_u64.pow(attempt + 1))).await;
            },
            Err(error) if is_account_unavailable_provider_error(provider_name, &error) => {
                eprintln!(
                    "skipping {provider_name} serialization scenarios: live account unavailable: {error:#}"
                );
                return None;
            },
            Err(error) => {
                panic!(
                    "provider {provider_name} request failed for scenario {}: {error:#}",
                    scenario.id
                );
            },
        }
    }

    unreachable!("bounded retry loop returns or panics")
}

async fn run_provider_scenarios(provider_name: &str, provider: OpenAiProvider) {
    let suite = load_suite();
    assert!(
        !suite.scenarios.is_empty(),
        "serialization scenario suite must not be empty"
    );

    for scenario in &suite.scenarios {
        if !scenario.providers.contains_key(provider_name) {
            continue;
        }

        let Some(response) =
            complete_scenario_with_retries(&provider, provider_name, scenario).await
        else {
            return;
        };

        assert_eq!(
            response.tool_calls.len(),
            1,
            "provider {provider_name} scenario {} expected exactly one tool call, got text {:?}",
            scenario.id,
            response.text
        );

        let tool_call = &response.tool_calls[0];
        let expected_name = scenario
            .tool
            .get("name")
            .and_then(serde_json::Value::as_str)
            .expect("tool name must be present");
        assert_eq!(
            tool_call.name, expected_name,
            "provider {provider_name} scenario {} called wrong tool",
            scenario.id
        );

        assert_provider_result(provider_name, scenario, &tool_call.arguments);
    }
}

#[tokio::test]
#[ignore]
async fn zai_serialization_scenarios_non_streaming() {
    let config = ProviderConfig {
        provider_name: "zai",
        api_key_env: "Z_API_KEY",
        base_url_env: None,
        default_base_url: "https://api.z.ai/api/paas/v4",
        model_env: "SERIALIZATION_TEST_ZAI_MODEL",
        default_model: "glm-5",
    };
    let Some(provider) = configured_provider(&config) else {
        return;
    };

    run_provider_scenarios(config.provider_name, provider).await;
}

#[tokio::test]
#[ignore]
async fn alibaba_coding_serialization_scenarios_non_streaming() {
    let config = ProviderConfig {
        provider_name: "alibaba-coding",
        api_key_env: "ALIBABA_CODING_API_KEY",
        base_url_env: Some("ALIBABA_CODING_BASE_URL"),
        default_base_url: "https://coding-intl.dashscope.aliyuncs.com/v1",
        model_env: "SERIALIZATION_TEST_ALIBABA_MODEL",
        default_model: "qwen3.5-plus",
    };
    let Some(provider) = configured_provider(&config) else {
        return;
    };

    run_provider_scenarios(config.provider_name, provider).await;
}

#[tokio::test]
#[ignore]
async fn openrouter_google_serialization_scenarios_non_streaming() {
    let config = ProviderConfig {
        provider_name: "openrouter-google",
        api_key_env: "OPENROUTER_API_KEY",
        base_url_env: None,
        default_base_url: "https://openrouter.ai/api/v1",
        model_env: "SERIALIZATION_TEST_OPENROUTER_GOOGLE_MODEL",
        default_model: "google/gemini-2.5-flash",
    };
    let Some(provider) = configured_provider(&config) else {
        return;
    };

    run_provider_scenarios(config.provider_name, provider).await;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn alibaba_config() -> ProviderConfig {
        ProviderConfig {
            provider_name: "alibaba-coding",
            api_key_env: "ALIBABA_CODING_API_KEY",
            base_url_env: Some("ALIBABA_CODING_BASE_URL"),
            default_base_url: "https://coding-intl.dashscope.aliyuncs.com/v1",
            model_env: "SERIALIZATION_TEST_ALIBABA_MODEL",
            default_model: "qwen3.5-plus",
        }
    }

    #[test]
    fn configured_base_url_uses_absolute_override() {
        assert_eq!(
            configured_base_url_from_override(
                &alibaba_config(),
                Some("https://example.test/v1".to_string())
            ),
            Some("https://example.test/v1".to_string())
        );
    }

    #[test]
    fn configured_base_url_falls_back_from_relative_override() {
        assert_eq!(
            configured_base_url_from_override(&alibaba_config(), Some("/v1".to_string())),
            Some("https://coding-intl.dashscope.aliyuncs.com/v1".to_string())
        );
    }
}
