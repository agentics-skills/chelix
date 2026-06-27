use std::{collections::HashSet, sync::mpsc, time::Duration};

use secrecy::ExposeSecret;

use tracing::{debug, warn};

use crate::{DiscoveredModel, ModelCapabilities};

const OPENAI_MODELS_ENDPOINT_PATH: &str = "/models";

#[derive(Clone, Copy)]
struct ModelCatalogEntry {
    id: &'static str,
    display_name: &'static str,
}

impl ModelCatalogEntry {
    const fn new(id: &'static str, display_name: &'static str) -> Self {
        Self { id, display_name }
    }
}

const DEFAULT_OPENAI_MODELS: &[ModelCatalogEntry] = &[
    ModelCatalogEntry::new("gpt-5.2", "GPT-5.2"),
    ModelCatalogEntry::new("gpt-5.2-chat-latest", "GPT-5.2 Chat Latest"),
    ModelCatalogEntry::new("gpt-5-mini", "GPT-5 Mini"),
];

#[must_use]
pub fn default_model_catalog() -> Vec<DiscoveredModel> {
    DEFAULT_OPENAI_MODELS
        .iter()
        .map(|entry| {
            DiscoveredModel::new(entry.id, entry.display_name)
                .with_recommended(is_recommended_openai_model(entry.id))
        })
        .collect()
}

fn title_case_chunk(chunk: &str) -> String {
    if chunk.is_empty() {
        return String::new();
    }
    let mut chars = chunk.chars();
    match chars.next() {
        Some(first) => {
            let mut out = String::new();
            out.push(first.to_ascii_uppercase());
            out.push_str(chars.as_str());
            out
        },
        None => String::new(),
    }
}

fn format_gpt_display_name(model_id: &str) -> String {
    let Some(rest) = model_id.strip_prefix("gpt-") else {
        return model_id.to_string();
    };
    let mut parts = rest.split('-');
    let Some(base) = parts.next() else {
        return "GPT".to_string();
    };
    let mut out = format!("GPT-{base}");
    for part in parts {
        out.push(' ');
        out.push_str(&title_case_chunk(part));
    }
    out
}

fn format_chatgpt_display_name(model_id: &str) -> String {
    let Some(rest) = model_id.strip_prefix("chatgpt-") else {
        return model_id.to_string();
    };
    let mut parts = rest.split('-');
    let Some(base) = parts.next() else {
        return "ChatGPT".to_string();
    };
    let mut out = format!("ChatGPT-{base}");
    for part in parts {
        out.push(' ');
        out.push_str(&title_case_chunk(part));
    }
    out
}

fn formatted_model_name(model_id: &str) -> String {
    if model_id.starts_with("gpt-") {
        return format_gpt_display_name(model_id);
    }
    if model_id.starts_with("chatgpt-") {
        return format_chatgpt_display_name(model_id);
    }
    model_id.to_string()
}

fn normalize_display_name(model_id: &str, display_name: Option<&str>) -> String {
    let normalized = display_name
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(model_id);
    if normalized == model_id {
        return formatted_model_name(model_id);
    }
    normalized.to_string()
}

fn is_likely_model_id(model_id: &str) -> bool {
    if model_id.is_empty() || model_id.len() > 160 {
        return false;
    }
    if model_id.chars().any(char::is_whitespace) {
        return false;
    }
    model_id
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.' | ':' | '/'))
}

/// Delegates to the shared [`super::super::is_chat_capable_model`] for filtering
/// non-chat models during discovery.
fn is_chat_capable_model(model_id: &str) -> bool {
    crate::is_chat_capable_model(model_id)
}

fn metadata_bool(obj: &serde_json::Map<String, serde_json::Value>, keys: &[&str]) -> Option<bool> {
    keys.iter()
        .find_map(|key| obj.get(*key).and_then(serde_json::Value::as_bool))
}

fn parse_capabilities(entry: &serde_json::Value) -> Option<ModelCapabilities> {
    let capabilities = entry.get("capabilities")?.as_object()?;
    let tools = metadata_bool(capabilities, &["tool_calling", "tools", "function_calling"])
        .unwrap_or(false);
    let vision = metadata_bool(capabilities, &["vision"]).unwrap_or(false);
    let reasoning = metadata_bool(capabilities, &["reasoning"])
        .or_else(|| metadata_bool(capabilities, &["thinking"]))
        .unwrap_or(false);
    Some(ModelCapabilities {
        text_generation: true,
        tools,
        vision,
        reasoning,
    })
}

fn parse_context_window(entry: &serde_json::Value) -> Option<u32> {
    ["context_length", "context_window", "max_input_tokens"]
        .iter()
        .find_map(|key| entry.get(*key).and_then(serde_json::Value::as_u64))
        .and_then(|value| u32::try_from(value).ok())
        .filter(|value| *value > 0)
}

fn parse_model_entry(entry: &serde_json::Value) -> Option<DiscoveredModel> {
    let obj = entry.as_object()?;
    let model_id = obj
        .get("id")
        .or_else(|| obj.get("slug"))
        .or_else(|| obj.get("model"))
        .and_then(serde_json::Value::as_str)?;

    if !is_likely_model_id(model_id) {
        return None;
    }

    let display_name = obj
        .get("display_name")
        .or_else(|| obj.get("displayName"))
        .or_else(|| obj.get("name"))
        .or_else(|| obj.get("title"))
        .and_then(serde_json::Value::as_str);

    let created_at = obj.get("created").and_then(serde_json::Value::as_i64);

    let recommended = is_recommended_openai_model(model_id);
    let mut model = DiscoveredModel::new(model_id, normalize_display_name(model_id, display_name))
        .with_created_at(created_at)
        .with_recommended(recommended);
    if let Some(capabilities) = parse_capabilities(entry) {
        model = model.with_capabilities(capabilities);
    }
    if let Some(context_window) = parse_context_window(entry) {
        model = model.with_context_window(context_window);
    }
    Some(model)
}

/// Known OpenAI flagship model IDs (latest generation, no date suffix).
/// These are the models most users care about.
fn is_recommended_openai_model(model_id: &str) -> bool {
    matches!(
        model_id,
        "gpt-5.4" | "gpt-5.4-mini" | "gpt-5.4-pro" | "o4-mini" | "o3"
    )
}

fn collect_candidate_arrays<'a>(
    value: &'a serde_json::Value,
    out: &mut Vec<&'a serde_json::Value>,
) {
    match value {
        serde_json::Value::Array(items) => out.extend(items),
        serde_json::Value::Object(map) => {
            for key in ["models", "data", "items", "results", "available"] {
                if let Some(nested) = map.get(key) {
                    collect_candidate_arrays(nested, out);
                }
            }
        },
        _ => {},
    }
}

fn parse_models_payload(value: &serde_json::Value) -> Vec<DiscoveredModel> {
    let mut candidates = Vec::new();
    collect_candidate_arrays(value, &mut candidates);

    let mut models = Vec::new();
    let mut seen = HashSet::new();
    for entry in candidates {
        if let Some(model) = parse_model_entry(entry)
            && is_chat_capable_model(&model.id)
            && seen.insert(model.id.clone())
        {
            models.push(model);
        }
    }

    // Sort by created_at descending (newest first). Models without a
    // timestamp are placed after those with one, preserving relative order.
    models.sort_by(|a, b| match (a.created_at, b.created_at) {
        (Some(a_ts), Some(b_ts)) => b_ts.cmp(&a_ts), // newest first
        (Some(_), None) => std::cmp::Ordering::Less, // timestamp before no-timestamp
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (None, None) => std::cmp::Ordering::Equal,
    });

    models
}

fn models_endpoint(base_url: &str) -> String {
    format!(
        "{}{OPENAI_MODELS_ENDPOINT_PATH}",
        base_url.trim_end_matches('/')
    )
}

/// Fetch available models from the OpenAI-compatible `/models` endpoint.
pub async fn fetch_models_from_api(
    api_key: secrecy::Secret<String>,
    base_url: String,
) -> anyhow::Result<Vec<DiscoveredModel>> {
    let client = crate::shared_http_client();
    let response = client
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
    let payload: serde_json::Value = serde_json::from_str(&body)?;
    let models = parse_models_payload(&payload);
    if models.is_empty() {
        anyhow::bail!("openai models API returned no models");
    }
    Ok(models)
}

/// Spawn model discovery in a background thread and return the receiver
/// immediately, without blocking. Call `.recv()` later to collect the result.
pub fn start_model_discovery(
    api_key: secrecy::Secret<String>,
    base_url: String,
) -> mpsc::Receiver<anyhow::Result<Vec<DiscoveredModel>>> {
    let (tx, rx) = mpsc::sync_channel(1);
    std::thread::spawn(move || {
        let result = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(anyhow::Error::from)
            .and_then(|rt| rt.block_on(fetch_models_from_api(api_key, base_url)));
        let _ = tx.send(result);
    });
    rx
}

fn fetch_models_blocking(
    api_key: secrecy::Secret<String>,
    base_url: String,
) -> anyhow::Result<Vec<DiscoveredModel>> {
    start_model_discovery(api_key, base_url)
        .recv()
        .map_err(|err| anyhow::anyhow!("openai model discovery worker failed: {err}"))?
}

pub fn live_models(
    api_key: &secrecy::Secret<String>,
    base_url: &str,
) -> anyhow::Result<Vec<DiscoveredModel>> {
    let models = fetch_models_blocking(api_key.clone(), base_url.to_string())?;
    debug!(model_count = models.len(), "loaded live models");
    Ok(models)
}

#[must_use]
pub fn available_models(api_key: &secrecy::Secret<String>, base_url: &str) -> Vec<DiscoveredModel> {
    let fallback = default_model_catalog();
    if cfg!(test) {
        return fallback;
    }

    let discovered = match live_models(api_key, base_url) {
        Ok(models) => models,
        Err(err) => {
            warn!(error = %err, base_url = %base_url, "failed to fetch openai models, using fallback catalog");
            return fallback;
        },
    };

    let merged = crate::merge_discovered_with_fallback_catalog(discovered, fallback);
    debug!(model_count = merged.len(), "loaded openai models catalog");
    merged
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn parse_model_entry_reads_provider_capabilities_and_context_length() {
        let entry = serde_json::json!({
            "id": "Combos/z.ai/glm",
            "created": 1_782_516_658_i64,
            "context_length": 1_000_000,
            "capabilities": {
                "tool_calling": true,
                "reasoning": true,
                "vision": false,
                "thinking": true
            }
        });

        let model = parse_model_entry(&entry).expect("model parses");
        assert_eq!(model.id, "Combos/z.ai/glm");
        assert_eq!(model.context_window, Some(1_000_000));
        assert_eq!(
            model.capabilities,
            Some(ModelCapabilities {
                text_generation: true,
                tools: true,
                vision: false,
                reasoning: true,
            })
        );
    }

    #[test]
    fn parse_model_entry_uses_thinking_when_reasoning_is_missing() {
        let entry = serde_json::json!({
            "id": "Combos/cx/gpt",
            "max_input_tokens": 400_000,
            "capabilities": {
                "tool_calling": true,
                "thinking": true,
                "vision": true
            }
        });

        let model = parse_model_entry(&entry).expect("model parses");
        assert_eq!(model.context_window, Some(400_000));
        assert_eq!(
            model.capabilities.expect("capabilities parsed").reasoning,
            true
        );
    }

    #[test]
    fn parse_model_entry_leaves_metadata_empty_for_bare_entries() {
        let entry = serde_json::json!({
            "id": "Combos/cx/gpt-mini",
            "created": 1_782_516_658_i64
        });

        let model = parse_model_entry(&entry).expect("model parses");
        assert_eq!(model.capabilities, None);
        assert_eq!(model.context_window, None);
    }
}
