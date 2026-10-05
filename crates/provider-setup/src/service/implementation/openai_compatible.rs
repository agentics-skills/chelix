//! Create, edit, and delete OpenAI Compatible providers.

use std::collections::HashSet;

use {secrecy::Secret, serde_json::Value};

use {
    chelix_config::{
        OpenAiCompatibleProviderTomlUpdate, delete_openai_compatible_provider_toml,
        schema::{ProviderEntry, ProviderStreamTransport, ProvidersConfig, ToolMode, WireApi},
        upsert_openai_compatible_provider_toml,
    },
    chelix_providers::model_id::namespaced_model_id,
    chelix_service_traits::{ServiceError, ServiceResult},
};

use {
    super::{
        LiveProviderSetupService, provider_models::blocked_agents,
        service::ProviderConfigPersistence, support::ProviderSetupTiming,
    },
    crate::provider_base_url::provider_base_url_error_with,
};

struct OpenAiCompatibleUpsert {
    name: String,
    previous_name: Option<String>,
    base_url: String,
    api_key: Option<String>,
    wire_api: WireApi,
    wire_api_toml: String,
    tool_mode: ToolMode,
    tool_mode_toml: String,
    enabled: bool,
}

impl LiveProviderSetupService {
    pub(super) async fn upsert_openai_compatible_inner(&self, params: Value) -> ServiceResult {
        let _timing = ProviderSetupTiming::start("providers.upsert_openai_compatible", None);
        let update = parse_upsert(&params)?;
        let agents = self.agent_entries().await;
        let mut registry = self.registry.write().await;
        let mut snapshot = self.config_snapshot();
        ensure_target_free(&snapshot, &update)?;
        let filesystem = self.config_persistence == ProviderConfigPersistence::Filesystem;
        let sync_memory_offered = !snapshot.offered.is_empty();
        let write_base_url = match update.previous_name.as_deref() {
            Some(previous) => {
                let current = snapshot
                    .providers
                    .get(previous)
                    .and_then(|entry| entry.base_url.as_deref())
                    .map(str::trim);
                current != Some(update.base_url.trim())
            },
            None => true,
        };
        let model_change = model_id_change(&snapshot, &update);
        apply_upsert_memory(&mut snapshot, &update, sync_memory_offered)?;
        let candidate = self.build_registry(&snapshot)?;
        let blocked = blocked_agents(&candidate, &agents);
        if !blocked.is_empty() {
            return Err(ServiceError::message(format!(
                "provider change breaks configured agents: {}",
                blocked.join(", ")
            )));
        }
        if self.config_persistence == ProviderConfigPersistence::Filesystem {
            upsert_openai_compatible_provider_toml(&OpenAiCompatibleProviderTomlUpdate {
                name: update.name.clone(),
                previous_name: update.previous_name.clone(),
                base_url: update.base_url.clone(),
                api_key: update.api_key.clone(),
                wire_api: update.wire_api_toml.clone(),
                tool_mode: update.tool_mode_toml.clone(),
                enabled: update.enabled,
                sync_offered: filesystem && sync_memory_offered,
                write_base_url,
            })
            .map_err(ServiceError::message)?;
        }
        *self
            .config
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = snapshot.clone();
        rewrite_priority(self, &model_change).await;
        *registry = self.build_registry(&snapshot)?;
        Ok(serde_json::json!({
            "ok": true,
            "providerName": update.name,
            "displayName": update.name,
            "renamedModelIds": model_change.renamed.iter().map(|(from, to)| serde_json::json!({"from": from, "to": to})).collect::<Vec<_>>(),
        }))
    }

    pub(super) async fn delete_openai_compatible_inner(&self, params: Value) -> ServiceResult {
        let _timing = ProviderSetupTiming::start("providers.delete_openai_compatible", None);
        let name = existing_section_name(required_str(&params, "name")?)?;
        let agents = self.agent_entries().await;
        let mut registry = self.registry.write().await;
        let mut snapshot = self.config_snapshot();
        let removed = removed_ids(&snapshot, &name);
        if snapshot.providers.remove(&name).is_none() {
            return Err(ServiceError::message(format!(
                "provider '{name}' is not configured"
            )));
        }
        let candidate = self.build_registry(&snapshot)?;
        let blocked = blocked_agents(&candidate, &agents);
        if !blocked.is_empty() {
            return Err(ServiceError::message(format!(
                "provider change breaks configured agents: {}",
                blocked.join(", ")
            )));
        }
        if self.config_persistence == ProviderConfigPersistence::Filesystem {
            delete_openai_compatible_provider_toml(&name).map_err(ServiceError::message)?;
        }
        *self
            .config
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = snapshot.clone();
        if let Some(priority) = self.priority_models.as_ref() {
            let prefix = format!("{name}::");
            priority.write().await.retain(|id| !id.starts_with(&prefix));
        }
        *registry = self.build_registry(&snapshot)?;
        Ok(serde_json::json!({
            "ok": true,
            "removedModelIds": removed,
        }))
    }
}

struct ModelIdChange {
    renamed: Vec<(String, String)>,
}

fn parse_upsert(params: &Value) -> Result<OpenAiCompatibleUpsert, ServiceError> {
    if let Some(transport) = params.get("streamTransport").and_then(Value::as_str)
        && transport != "sse"
    {
        return Err(ServiceError::message(
            "OpenAI Compatible providers only support stream_transport sse".to_string(),
        ));
    }
    let wire_api_toml = params
        .get("wireApi")
        .and_then(Value::as_str)
        .unwrap_or("chat-completions");
    let tool_mode_toml = params
        .get("toolMode")
        .and_then(Value::as_str)
        .unwrap_or("native");
    let base_url = required_str(params, "baseUrl")?.trim().to_string();
    if let Some(error) = provider_base_url_error_with(&base_url, wire_api_toml == "responses") {
        return Err(ServiceError::message(error.to_string()));
    }
    let previous_name = params
        .get("previousName")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .map(existing_section_name)
        .transpose()?;
    let raw_name = required_str(params, "name")?;
    let name = match previous_name.as_deref() {
        Some(previous) if raw_name == previous => previous.to_string(),
        _ => section_name(raw_name)?,
    };
    let api_key = params
        .get("apiKey")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned);
    if previous_name.is_none() && api_key.is_none() {
        return Err(ServiceError::message(
            "missing 'apiKey' parameter".to_string(),
        ));
    }
    Ok(OpenAiCompatibleUpsert {
        name,
        previous_name,
        base_url,
        api_key,
        wire_api: parse_wire_api(wire_api_toml)?,
        wire_api_toml: wire_api_toml.to_string(),
        tool_mode: parse_tool_mode(tool_mode_toml)?,
        tool_mode_toml: tool_mode_toml.to_string(),
        enabled: params
            .get("enabled")
            .and_then(Value::as_bool)
            .unwrap_or(true),
    })
}

fn required_str<'a>(params: &'a Value, key: &str) -> Result<&'a str, ServiceError> {
    params
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| ServiceError::message(format!("missing '{key}' parameter")))
}

fn existing_section_name(raw: &str) -> Result<String, ServiceError> {
    let name = raw.trim();
    match chelix_config::schema::openai_compatible_provider_name_error(name) {
        Some(error) => Err(ServiceError::message(error.to_string())),
        None => Ok(name.to_string()),
    }
}

fn section_name(raw: &str) -> Result<String, ServiceError> {
    chelix_config::schema::parse_openai_compatible_provider_name(raw)
        .map_err(|error| ServiceError::message(error.to_string()))
}

fn parse_wire_api(value: &str) -> Result<WireApi, ServiceError> {
    match value {
        "chat-completions" => Ok(WireApi::ChatCompletions),
        "responses" => Ok(WireApi::Responses),
        other => Err(ServiceError::message(format!("unknown wireApi: {other}"))),
    }
}

fn parse_tool_mode(value: &str) -> Result<ToolMode, ServiceError> {
    match value {
        "native" => Ok(ToolMode::Native),
        "text" => Ok(ToolMode::Text),
        "off" => Ok(ToolMode::Off),
        other => Err(ServiceError::message(format!("unknown toolMode: {other}"))),
    }
}

fn ensure_target_free(
    config: &ProvidersConfig,
    update: &OpenAiCompatibleUpsert,
) -> Result<(), ServiceError> {
    let current = update.previous_name.as_deref().unwrap_or(&update.name);
    if update.previous_name.is_some() && !config.providers.contains_key(current) {
        return Err(ServiceError::message(format!(
            "provider '{current}' is not configured"
        )));
    }
    if update.previous_name.as_deref() != Some(update.name.as_str())
        && config.providers.contains_key(&update.name)
    {
        return Err(ServiceError::message(format!(
            "provider '{}' already exists",
            update.name
        )));
    }
    Ok(())
}

fn replace_offered(offered: &mut [String], previous: &str, name: &str) {
    let target = normalize_offered(previous);
    for entry in offered.iter_mut() {
        if normalize_offered(entry) == target {
            *entry = name.to_string();
        }
    }
}

fn normalize_offered(value: &str) -> String {
    value.trim().to_ascii_lowercase()
}

fn offered_contains(offered: &[String], name: &str) -> bool {
    let target = normalize_offered(name);
    offered
        .iter()
        .any(|entry| normalize_offered(entry) == target)
}

fn append_offered(offered: &mut Vec<String>, name: &str) {
    if offered.is_empty() || offered_contains(offered, name) {
        return;
    }
    offered.push(name.to_string());
}

fn apply_upsert_memory(
    config: &mut ProvidersConfig,
    update: &OpenAiCompatibleUpsert,
    sync_offered: bool,
) -> Result<(), ServiceError> {
    if let Some(previous) = update.previous_name.as_deref()
        && previous != update.name
    {
        if sync_offered {
            replace_offered(&mut config.offered, previous, &update.name);
        }
        let Some(mut entry) = config.providers.remove(previous) else {
            return Err(ServiceError::message(format!(
                "provider '{previous}' is not configured"
            )));
        };
        fill_entry(&mut entry, update);
        config.providers.insert(update.name.clone(), entry);
    } else {
        let entry = config.providers.entry(update.name.clone()).or_default();
        fill_entry(entry, update);
    }
    if sync_offered {
        append_offered(&mut config.offered, &update.name);
    }
    Ok(())
}

fn fill_entry(entry: &mut ProviderEntry, update: &OpenAiCompatibleUpsert) {
    entry.enabled = update.enabled;
    entry.base_url = Some(update.base_url.clone());
    if let Some(api_key) = update.api_key.as_ref() {
        entry.api_key = Some(Secret::new(api_key.clone()));
    }
    entry.wire_api = update.wire_api;
    entry.stream_transport = ProviderStreamTransport::Sse;
    entry.tool_mode = update.tool_mode;
}

fn model_id_change(config: &ProvidersConfig, update: &OpenAiCompatibleUpsert) -> ModelIdChange {
    let Some(previous) = update
        .previous_name
        .as_deref()
        .filter(|name| *name != update.name)
    else {
        return ModelIdChange {
            renamed: Vec::new(),
        };
    };
    let Some(entry) = config.providers.get(previous) else {
        return ModelIdChange {
            renamed: Vec::new(),
        };
    };
    ModelIdChange {
        renamed: entry
            .models
            .keys()
            .map(|raw| {
                (
                    namespaced_model_id(previous, raw),
                    namespaced_model_id(&update.name, raw),
                )
            })
            .collect(),
    }
}

fn removed_ids(config: &ProvidersConfig, name: &str) -> Vec<String> {
    config
        .providers
        .get(name)
        .map(|entry| {
            entry
                .models
                .keys()
                .map(|raw| namespaced_model_id(name, raw))
                .collect()
        })
        .unwrap_or_default()
}

async fn rewrite_priority(service: &LiveProviderSetupService, change: &ModelIdChange) {
    if change.renamed.is_empty() {
        return;
    }
    let Some(priority) = service.priority_models.as_ref() else {
        return;
    };
    let mut ids = priority.write().await;
    let renamed = change.renamed.iter().cloned().collect::<HashSet<_>>();
    for id in ids.iter_mut() {
        if let Some((_, to)) = renamed.iter().find(|(from, _)| from == id) {
            *id = to.clone();
        }
    }
}
