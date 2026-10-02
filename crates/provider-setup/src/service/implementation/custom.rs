//! Create, edit, and delete `custom-*` providers.

use std::collections::HashSet;

use {
    secrecy::{ExposeSecret, Secret},
    serde_json::Value,
};

use {
    chelix_config::{
        CustomProviderTomlUpdate, delete_custom_provider_toml, providers_offered_env_is_set,
        schema::{ProviderEntry, ProviderStreamTransport, ProvidersConfig, ToolMode, WireApi},
        upsert_custom_provider_toml,
    },
    chelix_providers::model_id::namespaced_model_id,
    chelix_service_traits::{ServiceError, ServiceResult},
};

use {
    super::{
        LiveProviderSetupService, provider_models::blocked_agents,
        service::ProviderConfigPersistence, support::ProviderSetupTiming,
    },
    crate::{
        config_helpers::config_with_saved_keys, provider_base_url::provider_base_url_error_with,
    },
};

struct CustomUpsert {
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
    pub(super) async fn upsert_custom_inner(&self, params: Value) -> ServiceResult {
        let _timing = ProviderSetupTiming::start("providers.upsert_custom", None);
        let update = parse_upsert(&params)?;
        let agents = self.agent_entries().await;
        let mut registry = self.registry.write().await;
        let mut snapshot = self.config_snapshot();
        let saved = self
            .key_store
            .load_all_configs()
            .map_err(ServiceError::message)?;
        ensure_target_free(&snapshot, &update)?;
        let filesystem = self.config_persistence == ProviderConfigPersistence::Filesystem;
        let offered_env = filesystem && providers_offered_env_is_set();
        if offered_env
            && !snapshot.offered.is_empty()
            && !offered_contains(&snapshot.offered, &update.name)
        {
            return Err(ServiceError::message(format!(
                "providers.offered is set outside the TOML file; add '{}' to CHELIX_PROVIDERS__OFFERED",
                update.name
            )));
        }
        let sync_memory_offered = !offered_env && !snapshot.offered.is_empty();
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
        let mut registry_config = snapshot.clone();
        inject_registry_key(&mut registry_config, &update, &saved);
        let registry_config = config_with_saved_keys(&registry_config, &self.key_store)
            .map_err(ServiceError::message)?;
        let candidate = self.build_registry(&registry_config)?;
        let blocked = blocked_agents(&candidate, &agents);
        if !blocked.is_empty() {
            return Err(ServiceError::message(format!(
                "provider change breaks configured agents: {}",
                blocked.join(", ")
            )));
        }
        if self.config_persistence == ProviderConfigPersistence::Filesystem {
            upsert_custom_provider_toml(&CustomProviderTomlUpdate {
                name: update.name.clone(),
                previous_name: update.previous_name.clone(),
                base_url: update.base_url.clone(),
                wire_api: update.wire_api_toml.clone(),
                tool_mode: update.tool_mode_toml.clone(),
                enabled: update.enabled,
                reject_when_toml_has_api_key: update.api_key.is_some(),
                sync_offered: filesystem && !offered_env,
                write_base_url,
            })
            .map_err(ServiceError::message)?;
        }
        *self
            .config
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = snapshot;
        write_key_store(self, &update, &saved)?;
        rewrite_priority(self, &model_change).await;
        let effective = self.effective_config()?;
        *registry = self.build_registry(&effective)?;
        Ok(serde_json::json!({
            "ok": true,
            "providerName": update.name,
            "displayName": update.name,
            "renamedModelIds": model_change.renamed.iter().map(|(from, to)| serde_json::json!({"from": from, "to": to})).collect::<Vec<_>>(),
        }))
    }

    pub(super) async fn delete_custom_inner(&self, params: Value) -> ServiceResult {
        let _timing = ProviderSetupTiming::start("providers.delete_custom", None);
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
        let registry_config =
            config_with_saved_keys(&snapshot, &self.key_store).map_err(ServiceError::message)?;
        let candidate = self.build_registry(&registry_config)?;
        let blocked = blocked_agents(&candidate, &agents);
        if !blocked.is_empty() {
            return Err(ServiceError::message(format!(
                "provider change breaks configured agents: {}",
                blocked.join(", ")
            )));
        }
        if self.config_persistence == ProviderConfigPersistence::Filesystem {
            delete_custom_provider_toml(&name).map_err(ServiceError::message)?;
        }
        *self
            .config
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = snapshot;
        self.key_store
            .remove(&name)
            .map_err(ServiceError::message)?;
        if let Some(priority) = self.priority_models.as_ref() {
            let prefix = format!("{name}::");
            priority.write().await.retain(|id| !id.starts_with(&prefix));
        }
        let effective = self.effective_config()?;
        *registry = self.build_registry(&effective)?;
        Ok(serde_json::json!({
            "ok": true,
            "removedModelIds": removed,
        }))
    }
}

struct ModelIdChange {
    renamed: Vec<(String, String)>,
}

fn parse_upsert(params: &Value) -> Result<CustomUpsert, ServiceError> {
    if let Some(transport) = params.get("streamTransport").and_then(Value::as_str)
        && transport != "sse"
    {
        return Err(ServiceError::message(
            "custom providers only support stream_transport sse".to_string(),
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
        Some(previous) if raw_name == previous || format!("custom-{raw_name}") == previous => {
            previous.to_string()
        },
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
    Ok(CustomUpsert {
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
    if name.starts_with("custom-") && name.len() > "custom-".len() {
        return Ok(name.to_string());
    }
    Err(ServiceError::message(
        "provider name must start with custom-".to_string(),
    ))
}

fn section_name(raw: &str) -> Result<String, ServiceError> {
    let mut slug = raw.trim().to_ascii_lowercase();
    if let Some(rest) = slug.strip_prefix("custom-") {
        slug = rest.to_string();
    }
    let valid = !slug.is_empty()
        && !slug.starts_with('-')
        && !slug.ends_with('-')
        && slug
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '-');
    if !valid {
        return Err(ServiceError::message(
            "provider name must contain only letters, digits, and hyphens".to_string(),
        ));
    }
    Ok(format!("custom-{slug}"))
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

fn ensure_target_free(config: &ProvidersConfig, update: &CustomUpsert) -> Result<(), ServiceError> {
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
    update: &CustomUpsert,
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
        reject_toml_api_key(previous, &entry, update.api_key.is_some())?;
        fill_entry(&mut entry, update);
        config.providers.insert(update.name.clone(), entry);
    } else {
        let entry = config.providers.entry(update.name.clone()).or_default();
        reject_toml_api_key(&update.name, entry, update.api_key.is_some())?;
        fill_entry(entry, update);
    }
    if sync_offered {
        append_offered(&mut config.offered, &update.name);
    }
    Ok(())
}

fn reject_toml_api_key(
    name: &str,
    entry: &ProviderEntry,
    replacing_key: bool,
) -> Result<(), ServiceError> {
    if replacing_key
        && entry
            .api_key
            .as_ref()
            .is_some_and(|key| !key.expose_secret().trim().is_empty())
    {
        return Err(ServiceError::message(format!(
            "provider '{name}' already has an api_key in the service configuration"
        )));
    }
    Ok(())
}

fn fill_entry(entry: &mut ProviderEntry, update: &CustomUpsert) {
    entry.enabled = update.enabled;
    entry.base_url = Some(update.base_url.clone());
    entry.wire_api = update.wire_api;
    entry.stream_transport = ProviderStreamTransport::Sse;
    entry.tool_mode = update.tool_mode;
}

fn inject_registry_key(
    config: &mut ProvidersConfig,
    update: &CustomUpsert,
    saved: &std::collections::HashMap<String, crate::key_store::ProviderConfig>,
) {
    let source = update
        .previous_name
        .as_deref()
        .filter(|name| *name != update.name)
        .unwrap_or(update.name.as_str());
    let key = update
        .api_key
        .clone()
        .or_else(|| saved.get(source).and_then(|config| config.api_key.clone()));
    if let Some(entry) = config.providers.get_mut(&update.name)
        && entry
            .api_key
            .as_ref()
            .is_none_or(|value| value.expose_secret().trim().is_empty())
        && let Some(key) = key
    {
        entry.api_key = Some(Secret::new(key));
    }
}

fn model_id_change(config: &ProvidersConfig, update: &CustomUpsert) -> ModelIdChange {
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

fn write_key_store(
    service: &LiveProviderSetupService,
    update: &CustomUpsert,
    saved: &std::collections::HashMap<String, crate::key_store::ProviderConfig>,
) -> Result<(), ServiceError> {
    let previous = update
        .previous_name
        .as_deref()
        .filter(|name| *name != update.name);
    if update.previous_name.is_none() || previous.is_some() {
        service
            .key_store
            .remove(&update.name)
            .map_err(ServiceError::message)?;
    }
    let carried = previous.and_then(|name| saved.get(name));
    let api_key = update
        .api_key
        .clone()
        .or_else(|| carried.and_then(|config| config.api_key.clone()));
    service
        .key_store
        .save_config_with_display_name(
            &update.name,
            api_key,
            Some(update.base_url.clone()),
            Some(update.name.clone()),
        )
        .map_err(ServiceError::message)?;
    if let Some(previous) = previous {
        service
            .key_store
            .remove(previous)
            .map_err(ServiceError::message)?;
    }
    Ok(())
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
