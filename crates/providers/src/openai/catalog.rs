//! OpenAI-compatible model discovery.

use std::time::Duration;

use {
    chelix_common::{
        ModelModality, PartialModelMetadata, PartialReasoningMetadata, ReasoningEffort,
        ReasoningInclude, ReasoningSummary,
    },
    secrecy::ExposeSecret,
    serde::Deserialize,
};

use crate::{DiscoveredModel, discovered_model::deduplicate_discovered};

const OPENAI_MODELS_ENDPOINT_PATH: &str = "/models";

#[derive(Debug, Deserialize)]
struct ModelEntry {
    #[serde(default, alias = "slug", alias = "model")]
    id: Option<String>,
    #[serde(
        default,
        alias = "display_name",
        alias = "displayName",
        alias = "title"
    )]
    name: Option<String>,
    #[serde(default, alias = "created_at")]
    created: Option<i64>,
    #[serde(default)]
    recommended: Option<bool>,
    #[serde(default)]
    context_length: Option<u32>,
    #[serde(default)]
    max_input_tokens: Option<u32>,
    #[serde(default)]
    max_output_tokens: Option<u32>,
    #[serde(default)]
    input_modalities: Option<Vec<ModelModality>>,
    #[serde(default)]
    output_modalities: Option<Vec<ModelModality>>,
    #[serde(default)]
    tool_calling: Option<bool>,
    #[serde(default)]
    streaming: Option<bool>,
    #[serde(default, rename = "zeroDataRetentionEnabled")]
    zero_data_retention_enabled: Option<bool>,
    #[serde(default)]
    reasoning: Option<ReasoningMetadata>,
    #[serde(default)]
    capabilities: Option<CapiCapabilities>,
    #[serde(default)]
    architecture: Option<OpenRouterArchitecture>,
    #[serde(default)]
    supported_parameters: Option<Vec<String>>,
    #[serde(default)]
    top_provider: Option<OpenRouterProviderMetadata>,
}

#[derive(Debug, Deserialize)]
struct ReasoningMetadata {
    #[serde(default)]
    supported_efforts: Option<Vec<ReasoningEffort>>,
    #[serde(default)]
    summary: Option<ReasoningSummary>,
    #[serde(default)]
    include: Option<Vec<ReasoningInclude>>,
}

#[derive(Debug, Deserialize)]
struct CapiCapabilities {
    #[serde(default, rename = "type")]
    capability_type: Option<String>,
    #[serde(default)]
    limits: Option<CapiLimits>,
    #[serde(default)]
    supports: Option<CapiSupports>,
}

#[derive(Debug, Deserialize)]
struct CapiLimits {
    #[serde(default)]
    max_prompt_tokens: Option<u32>,
    #[serde(default)]
    max_output_tokens: Option<u32>,
    #[serde(default)]
    max_context_window_tokens: Option<u32>,
}

#[derive(Debug, Deserialize)]
struct CapiSupports {
    #[serde(default)]
    tool_calls: Option<bool>,
    #[serde(default)]
    streaming: Option<bool>,
    #[serde(default)]
    vision: Option<bool>,
    #[serde(default)]
    thinking: Option<bool>,
}

#[derive(Debug, Deserialize)]
struct OpenRouterArchitecture {
    #[serde(default)]
    input_modalities: Option<Vec<ModelModality>>,
    #[serde(default)]
    output_modalities: Option<Vec<ModelModality>>,
}

#[derive(Debug, Deserialize)]
struct OpenRouterProviderMetadata {
    #[serde(default)]
    context_length: Option<u32>,
    #[serde(default)]
    max_completion_tokens: Option<u32>,
}

impl ModelEntry {
    fn into_discovered(self) -> Option<DiscoveredModel> {
        let id = self.id?.trim().to_string();
        if id.is_empty() {
            return None;
        }

        if self
            .capabilities
            .as_ref()
            .and_then(|capabilities| capabilities.capability_type.as_deref())
            .is_some_and(|capability_type| capability_type != "chat")
        {
            return None;
        }

        let limits = self
            .capabilities
            .as_ref()
            .and_then(|capabilities| capabilities.limits.as_ref());
        let supports = self
            .capabilities
            .as_ref()
            .and_then(|capabilities| capabilities.supports.as_ref());
        let architecture = self.architecture.as_ref();

        let context_length = self
            .context_length
            .or_else(|| limits.and_then(|limits| limits.max_context_window_tokens))
            .or_else(|| {
                self.top_provider
                    .as_ref()
                    .and_then(|provider| provider.context_length)
            });
        let max_input_tokens = self
            .max_input_tokens
            .or_else(|| limits.and_then(|limits| limits.max_prompt_tokens));
        let max_output_tokens = self
            .max_output_tokens
            .or_else(|| limits.and_then(|limits| limits.max_output_tokens))
            .or_else(|| {
                self.top_provider
                    .as_ref()
                    .and_then(|provider| provider.max_completion_tokens)
            });
        let input_modalities = self
            .input_modalities
            .or_else(|| architecture.and_then(|value| value.input_modalities.clone()))
            .or_else(|| supports.and_then(input_modalities_from_capi));
        let output_modalities = self
            .output_modalities
            .or_else(|| architecture.and_then(|value| value.output_modalities.clone()));
        let tool_calling = self
            .tool_calling
            .or_else(|| supports.and_then(|supports| supports.tool_calls))
            .or_else(|| {
                self.supported_parameters
                    .as_ref()
                    .map(|parameters| parameters.iter().any(|parameter| parameter == "tools"))
            });
        let streaming = self
            .streaming
            .or_else(|| supports.and_then(|supports| supports.streaming));
        let reasoning = self
            .reasoning
            .map(|reasoning| PartialReasoningMetadata {
                supported_efforts: reasoning.supported_efforts,
                summary: reasoning.summary,
                include: reasoning.include,
            })
            .or_else(|| supports.and_then(reasoning_from_capi));

        let display_name = self
            .name
            .as_deref()
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .unwrap_or(&id)
            .to_string();

        Some(
            DiscoveredModel::new(id, display_name)
                .with_created_at(self.created)
                .with_recommended(self.recommended.unwrap_or(false))
                .with_metadata(PartialModelMetadata {
                    context_length,
                    max_input_tokens,
                    max_output_tokens,
                    input_modalities,
                    output_modalities,
                    tool_calling,
                    streaming,
                    zero_data_retention_enabled: self.zero_data_retention_enabled,
                    reasoning,
                }),
        )
    }
}

fn input_modalities_from_capi(supports: &CapiSupports) -> Option<Vec<ModelModality>> {
    supports.vision.map(|vision| {
        let mut modalities = vec![ModelModality::Text];
        if vision {
            modalities.push(ModelModality::Image);
        }
        modalities
    })
}

fn reasoning_from_capi(supports: &CapiSupports) -> Option<PartialReasoningMetadata> {
    match supports.thinking {
        Some(false) => Some(PartialReasoningMetadata {
            supported_efforts: Some(Vec::new()),
            ..Default::default()
        }),
        Some(true) | None => None,
    }
}

fn collect_model_entries<'a>(
    value: &'a serde_json::Value,
    entries: &mut Vec<&'a serde_json::Value>,
) {
    match value {
        serde_json::Value::Array(items) => entries.extend(items),
        serde_json::Value::Object(object) => {
            let mut found_envelope = false;
            for key in ["data", "models", "items", "results", "available"] {
                if let Some(nested) = object.get(key) {
                    found_envelope = true;
                    collect_model_entries(nested, entries);
                }
            }
            if !found_envelope
                && ["id", "slug", "model"]
                    .iter()
                    .any(|key| object.contains_key(*key))
            {
                entries.push(value);
            }
        },
        _ => {},
    }
}

/// Parse typed partial model records from a recognized model-list envelope.
pub(crate) fn parse_models_value(
    value: &serde_json::Value,
) -> anyhow::Result<Vec<DiscoveredModel>> {
    let mut entries = Vec::new();
    collect_model_entries(value, &mut entries);
    let models = entries
        .into_iter()
        .map(|entry| serde_json::from_value::<ModelEntry>(entry.clone()))
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .filter_map(ModelEntry::into_discovered)
        .collect();
    Ok(deduplicate_discovered(models))
}

fn parse_models_payload(body: &str) -> anyhow::Result<Vec<DiscoveredModel>> {
    let value: serde_json::Value = serde_json::from_str(body)?;
    parse_models_value(&value)
}

fn models_endpoint(base_url: &str) -> String {
    format!(
        "{}{OPENAI_MODELS_ENDPOINT_PATH}",
        base_url.trim_end_matches('/')
    )
}

/// Fetch partial model records from an OpenAI-compatible `/models` endpoint.
pub async fn fetch_models_from_api(
    api_key: secrecy::Secret<String>,
    base_url: String,
) -> anyhow::Result<Vec<DiscoveredModel>> {
    let response = crate::shared_http_client()
        .get(models_endpoint(&base_url))
        .timeout(Duration::from_secs(15))
        .header(
            "Authorization",
            format!("Bearer {}", api_key.expose_secret()),
        )
        .header("Accept", "application/json")
        .send()
        .await?;
    let status = response.status();
    let body = response.text().await?;
    if !status.is_success() {
        anyhow::bail!("openai models API error HTTP {status}");
    }
    let models = parse_models_payload(&body)?;
    if models.is_empty() {
        anyhow::bail!("openai models API returned no model records");
    }
    Ok(models)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn parses_capi_limits_and_supports_without_inference() {
        let models = parse_models_payload(
            &serde_json::json!({
                "data": [{
                    "id": "capi-model",
                    "name": "CAPI Model",
                    "capabilities": {
                        "type": "chat",
                        "limits": {
                            "max_context_window_tokens": 400_000,
                            "max_prompt_tokens": 272_000,
                            "max_output_tokens": 128_000
                        },
                        "supports": {
                            "tool_calls": true,
                            "streaming": true,
                            "vision": true,
                            "thinking": false
                        }
                    }
                }]
            })
            .to_string(),
        )
        .unwrap();

        let metadata = &models[0].metadata;
        assert_eq!(metadata.context_length, Some(400_000));
        assert_eq!(metadata.max_input_tokens, Some(272_000));
        assert_eq!(metadata.max_output_tokens, Some(128_000));
        assert_eq!(
            metadata.input_modalities,
            Some(vec![ModelModality::Text, ModelModality::Image])
        );
        assert_eq!(metadata.tool_calling, Some(true));
        assert_eq!(metadata.streaming, Some(true));
        assert_eq!(
            metadata
                .reasoning
                .as_ref()
                .and_then(|reasoning| reasoning.supported_efforts.as_ref()),
            Some(&Vec::new())
        );
    }

    #[test]
    fn thinking_true_does_not_invent_supported_efforts() {
        let models = parse_models_payload(
            &serde_json::json!({
                "data": [{
                    "id": "reasoning-model",
                    "capabilities": {
                        "type": "chat",
                        "supports": { "thinking": true }
                    }
                }]
            })
            .to_string(),
        )
        .unwrap();
        assert!(models[0].metadata.reasoning.is_none());
    }

    #[test]
    fn parses_openrouter_metadata_without_derived_token_values() {
        let models = parse_models_payload(
            &serde_json::json!({
                "data": [{
                    "id": "vendor/model",
                    "name": "Vendor Model",
                    "context_length": 200_000,
                    "architecture": {
                        "input_modalities": ["text", "image", "file"],
                        "output_modalities": ["text"]
                    },
                    "supported_parameters": ["tools", "temperature"],
                    "top_provider": {
                        "context_length": 180_000,
                        "max_completion_tokens": 20_000
                    },
                    "reasoning": {
                        "supported_efforts": ["low", "high"]
                    }
                }]
            })
            .to_string(),
        )
        .unwrap();

        let metadata = &models[0].metadata;
        assert_eq!(metadata.context_length, Some(200_000));
        assert_eq!(metadata.max_input_tokens, None);
        assert_eq!(metadata.max_output_tokens, Some(20_000));
        assert_eq!(metadata.tool_calling, Some(true));
        assert_eq!(
            metadata
                .reasoning
                .as_ref()
                .and_then(|reasoning| reasoning.supported_efforts.clone()),
            Some(vec!["low".into(), "high".into()])
        );
    }

    #[test]
    fn parses_top_level_full_record_and_zdr() {
        let models = parse_models_payload(
            &serde_json::json!({
                "data": [{
                    "id": "full-model",
                    "context_length": 400_000,
                    "max_input_tokens": 272_000,
                    "max_output_tokens": 128_000,
                    "input_modalities": ["text", "audio"],
                    "output_modalities": ["text"],
                    "tool_calling": false,
                    "streaming": true,
                    "zeroDataRetentionEnabled": false,
                    "reasoning": { "supported_efforts": [] }
                }]
            })
            .to_string(),
        )
        .unwrap();

        assert_eq!(models[0].metadata.zero_data_retention_enabled, Some(false));
        assert_eq!(models[0].metadata.tool_calling, Some(false));
    }

    #[test]
    fn parses_reasoning_summary_and_include_without_applying_them() {
        let models = parse_models_payload(
            &serde_json::json!({
                "data": [{
                    "id": "reasoning-model",
                    "reasoning": {
                        "supported_efforts": ["low", "high"],
                        "summary": "concise",
                        "include": ["reasoning.encrypted_content"]
                    }
                }]
            })
            .to_string(),
        )
        .unwrap();

        let reasoning = models[0].metadata.reasoning.as_ref().unwrap();
        assert_eq!(reasoning.summary, Some(ReasoningSummary::Concise));
        assert_eq!(
            reasoning.include,
            Some(vec![ReasoningInclude::EncryptedContent])
        );
    }

    #[test]
    fn parses_nested_model_envelopes_and_explicit_recommendation() {
        let models = parse_models_payload(
            r#"{"data":{"items":[{"slug":"nested","display_name":"Nested","recommended":true}]}}"#,
        )
        .unwrap();

        assert_eq!(models.len(), 1);
        assert_eq!(models[0].id, "nested");
        assert_eq!(models[0].display_name, "Nested");
        assert!(models[0].recommended);
    }

    #[test]
    fn bare_standard_openai_entry_stays_partial() {
        let models = parse_models_payload(
            r#"{"data":[{"id":"bare","object":"model","created":123,"owned_by":"owner"}]}"#,
        )
        .unwrap();
        assert_eq!(models.len(), 1);
        assert_eq!(models[0].metadata, PartialModelMetadata::default());
    }

    #[test]
    fn excludes_explicit_non_chat_capi_records() {
        let models = parse_models_payload(
            r#"{"data":[{"id":"embedding","capabilities":{"type":"embeddings"}}]}"#,
        )
        .unwrap();
        assert!(models.is_empty());
    }

    #[test]
    fn rejects_unknown_reasoning_effort() {
        let error = parse_models_payload(
            r#"{"data":[{"id":"model","reasoning":{"supported_efforts":["ultra"]}}]}"#,
        )
        .unwrap_err();
        assert!(error.to_string().contains("unknown variant"));
    }
}
