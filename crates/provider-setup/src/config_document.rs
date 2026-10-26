//! Provider table edits applied to the queued configuration document.

use {chelix_config::toml_document::nested_table, toml_edit::DocumentMut};

pub struct OpenAiCompatibleProviderTomlUpdate {
    pub name: String,
    pub previous_name: Option<String>,
    pub base_url: String,
    pub api_key: Option<String>,
    pub wire_api: String,
    pub tool_mode: String,
    pub enabled: bool,
    pub sync_offered: bool,
    pub write_base_url: bool,
}

pub struct OpenAiCompatibleProviderTomlResult {
    pub renamed_model_ids: Vec<(String, String)>,
}

pub fn upsert_openai_compatible_provider_toml(
    document: &mut DocumentMut,
    update: &OpenAiCompatibleProviderTomlUpdate,
) -> chelix_config::Result<OpenAiCompatibleProviderTomlResult> {
    apply_upsert(document.as_table_mut(), update)
}

pub fn delete_openai_compatible_provider_toml(
    document: &mut DocumentMut,
    name: &str,
) -> chelix_config::Result<Vec<String>> {
    let root = document.as_table_mut();
    let removed = {
        let providers = providers_table(root)?;
        let Some(item) = providers.remove(name) else {
            return Err(chelix_config::Error::message(format!(
                "provider '{name}' is not configured"
            )));
        };
        model_ids(name, &item)
    };
    remove_priority_prefix(root, &format!("{name}::"));
    Ok(removed)
}

fn apply_upsert(
    root: &mut toml_edit::Table,
    update: &OpenAiCompatibleProviderTomlUpdate,
) -> chelix_config::Result<OpenAiCompatibleProviderTomlResult> {
    let previous = update
        .previous_name
        .as_deref()
        .filter(|name| *name != update.name);
    let renamed = if let Some(previous) = previous {
        let providers = providers_table(root)?;
        if providers.contains_key(&update.name) {
            return Err(chelix_config::Error::message(format!(
                "provider '{}' already exists",
                update.name
            )));
        }
        let Some(item) = providers.remove(previous) else {
            return Err(chelix_config::Error::message(format!(
                "provider '{previous}' is not configured"
            )));
        };
        let renamed = renamed_ids(previous, &update.name, &item);
        providers.insert(&update.name, item);
        if update.sync_offered {
            replace_offered_name(providers, previous, &update.name);
        }
        renamed
    } else {
        let providers = providers_table(root)?;
        if update.previous_name.is_none() && providers.contains_key(&update.name) {
            return Err(chelix_config::Error::message(format!(
                "provider '{}' already exists",
                update.name
            )));
        }
        if update.previous_name.is_some() && !providers.contains_key(&update.name) {
            return Err(chelix_config::Error::message(format!(
                "provider '{}' is not configured",
                update.name
            )));
        }
        Vec::new()
    };
    if let Some(previous) = previous {
        replace_priority_prefix(root, previous, &update.name);
    }
    let providers = providers_table(root)?;
    if update.sync_offered {
        append_offered_name(providers, &update.name);
    }
    let table = nested_table(providers, &update.name, false)?;
    table.insert("enabled", toml_edit::value(update.enabled));
    if let Some(api_key) = update.api_key.as_deref() {
        table.insert("api_key", toml_edit::value(api_key));
    }
    if update.write_base_url {
        table.insert("base_url", toml_edit::value(&update.base_url));
    }
    table.insert("wire_api", toml_edit::value(&update.wire_api));
    table.insert("stream_transport", toml_edit::value("sse"));
    table.insert("tool_mode", toml_edit::value(&update.tool_mode));
    Ok(OpenAiCompatibleProviderTomlResult {
        renamed_model_ids: renamed,
    })
}

pub fn write_provider_api_key(
    document: &mut DocumentMut,
    name: &str,
    api_key: &str,
    base_url: Option<&str>,
    write_base_url: bool,
) -> chelix_config::Result<()> {
    let providers = providers_table(document.as_table_mut())?;
    let table = nested_table(providers, name, false)?;
    table.insert("api_key", toml_edit::value(api_key));
    table.insert("enabled", toml_edit::value(true));
    if write_base_url {
        match base_url.map(str::trim).filter(|url| !url.is_empty()) {
            Some(url) => {
                table.insert("base_url", toml_edit::value(url));
            },
            None => {
                table.remove("base_url");
            },
        }
    }
    Ok(())
}

pub fn clear_provider_api_key(document: &mut DocumentMut, name: &str) -> chelix_config::Result<()> {
    let providers = providers_table(document.as_table_mut())?;
    if !providers.contains_key(name) {
        return Err(chelix_config::Error::message(format!(
            "provider '{name}' is not configured"
        )));
    }
    let table = nested_table(providers, name, false)?;
    table.remove("api_key");
    table.insert("enabled", toml_edit::value(false));
    Ok(())
}

fn providers_table(root: &mut toml_edit::Table) -> chelix_config::Result<&mut toml_edit::Table> {
    nested_table(root, "providers", true)
}

fn model_raw_ids(item: &toml_edit::Item) -> Vec<String> {
    let Some(table) = item.as_table() else {
        return Vec::new();
    };
    let Some(models) = table.get("models") else {
        return Vec::new();
    };
    if let Some(table) = models.as_table() {
        return table.iter().map(|(key, _)| key.to_string()).collect();
    }
    models
        .as_inline_table()
        .map(|table| table.iter().map(|(key, _)| key.to_string()).collect())
        .unwrap_or_default()
}

fn model_ids(provider: &str, item: &toml_edit::Item) -> Vec<String> {
    model_raw_ids(item)
        .into_iter()
        .map(|raw| format!("{provider}::{raw}"))
        .collect()
}

fn renamed_ids(previous: &str, name: &str, item: &toml_edit::Item) -> Vec<(String, String)> {
    model_raw_ids(item)
        .into_iter()
        .map(|raw| (format!("{previous}::{raw}"), format!("{name}::{raw}")))
        .collect()
}

fn normalized_offered(value: &str) -> String {
    value.trim().to_ascii_lowercase()
}

fn append_offered_name(providers: &mut toml_edit::Table, name: &str) {
    let Some(array) = providers
        .get_mut("offered")
        .and_then(toml_edit::Item::as_array_mut)
    else {
        return;
    };
    if array.is_empty() {
        return;
    }
    let target = normalized_offered(name);
    let present = array.iter().any(|value| {
        value
            .as_str()
            .is_some_and(|entry| normalized_offered(entry) == target)
    });
    if !present {
        array.push(name);
    }
}

fn replace_offered_name(providers: &mut toml_edit::Table, previous: &str, name: &str) {
    let Some(array) = providers
        .get_mut("offered")
        .and_then(toml_edit::Item::as_array_mut)
    else {
        return;
    };
    let target = normalized_offered(previous);
    let indexes = array
        .iter()
        .enumerate()
        .filter_map(|(index, value)| {
            value
                .as_str()
                .is_some_and(|entry| normalized_offered(entry) == target)
                .then_some(index)
        })
        .collect::<Vec<_>>();
    for index in indexes {
        array.replace(index, name);
    }
}

fn replace_priority_prefix(root: &mut toml_edit::Table, previous: &str, name: &str) {
    let Some(priority) = priority_array(root) else {
        return;
    };
    let prefix = format!("{previous}::");
    let replacements = priority
        .iter()
        .enumerate()
        .filter_map(|(index, value)| {
            let raw = value.as_str()?.strip_prefix(&prefix)?;
            Some((index, format!("{name}::{raw}")))
        })
        .collect::<Vec<_>>();
    for (index, id) in replacements {
        priority.replace(index, id);
    }
}

fn remove_priority_prefix(root: &mut toml_edit::Table, prefix: &str) {
    let Some(priority) = priority_array(root) else {
        return;
    };
    priority.retain(|value| value.as_str().is_none_or(|id| !id.starts_with(prefix)));
}

fn priority_array(root: &mut toml_edit::Table) -> Option<&mut toml_edit::Array> {
    root.get_mut("chat")?
        .as_table_mut()?
        .get_mut("priority_models")?
        .as_array_mut()
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    fn document(raw: &str) -> DocumentMut {
        raw.parse().expect("test document")
    }

    fn provider_update(
        name: &str,
        previous_name: Option<&str>,
    ) -> OpenAiCompatibleProviderTomlUpdate {
        OpenAiCompatibleProviderTomlUpdate {
            name: name.to_string(),
            previous_name: previous_name.map(str::to_string),
            base_url: "https://models.example.invalid/v1".to_string(),
            api_key: Some("secret".to_string()),
            wire_api: "chat-completions".to_string(),
            tool_mode: "native".to_string(),
            enabled: true,
            sync_offered: true,
            write_base_url: true,
        }
    }

    fn priority(document: &DocumentMut) -> Vec<String> {
        document
            .get("chat")
            .and_then(toml_edit::Item::as_table)
            .and_then(|chat| chat.get("priority_models"))
            .and_then(toml_edit::Item::as_array)
            .map(|priority| {
                priority
                    .iter()
                    .filter_map(toml_edit::Value::as_str)
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default()
    }

    #[test]
    fn rename_keeps_model_ids_and_rewrites_priority_and_offered_names() {
        let mut document = document(
            r#"
[providers]
offered = ["local-ai"]

[providers.local-ai.models.zeta]
enabled = false

[providers.local-ai.models.alpha]
enabled = true

[chat]
priority_models = ["local-ai::zeta", "other::beta", "local-ai::alpha"]
"#,
        );

        let result = upsert_openai_compatible_provider_toml(
            &mut document,
            &provider_update("edge-ai", Some("local-ai")),
        )
        .expect("rename provider");

        assert_eq!(result.renamed_model_ids, vec![
            ("local-ai::zeta".to_string(), "edge-ai::zeta".to_string()),
            ("local-ai::alpha".to_string(), "edge-ai::alpha".to_string()),
        ]);
        let models = document["providers"]["edge-ai"]["models"]
            .as_table()
            .expect("models table");
        assert_eq!(models.iter().map(|(key, _)| key).collect::<Vec<_>>(), vec![
            "zeta", "alpha"
        ]);
        assert!(
            !document["providers"]
                .as_table()
                .expect("providers")
                .contains_key("local-ai")
        );
        assert_eq!(priority(&document), vec![
            "edge-ai::zeta".to_string(),
            "other::beta".to_string(),
            "edge-ai::alpha".to_string()
        ]);
        let offered = document["providers"]["offered"]
            .as_array()
            .expect("offered")
            .iter()
            .filter_map(toml_edit::Value::as_str)
            .collect::<Vec<_>>();
        assert_eq!(offered, vec!["edge-ai"]);
    }

    #[test]
    fn delete_removes_provider_and_priority_prefix() {
        let mut document = document(
            r#"
[providers.local-ai.models.alpha]
enabled = true

[chat]
priority_models = ["local-ai::alpha", "other::beta"]
"#,
        );

        let removed = delete_openai_compatible_provider_toml(&mut document, "local-ai")
            .expect("delete provider");

        assert_eq!(removed, vec!["local-ai::alpha".to_string()]);
        assert!(document.get("providers").is_none_or(|providers| {
            providers
                .as_table()
                .is_none_or(|table| !table.contains_key("local-ai"))
        }));
        assert_eq!(priority(&document), vec!["other::beta".to_string()]);
    }

    #[test]
    fn credential_patches_preserve_enabled_and_optional_base_url() {
        let mut document = document(
            r#"
[providers.local-ai]
enabled = false
base_url = "https://old.example.invalid/v1"
api_key = "old"
"#,
        );

        write_provider_api_key(&mut document, "local-ai", "new", Some("  "), true)
            .expect("write key");
        let provider = document["providers"]["local-ai"]
            .as_table()
            .expect("provider");
        assert_eq!(
            provider.get("api_key").and_then(toml_edit::Item::as_str),
            Some("new")
        );
        assert_eq!(
            provider.get("enabled").and_then(toml_edit::Item::as_bool),
            Some(true)
        );
        assert!(provider.get("base_url").is_none());

        clear_provider_api_key(&mut document, "local-ai").expect("clear key");
        let provider = document["providers"]["local-ai"]
            .as_table()
            .expect("provider");
        assert!(provider.get("api_key").is_none());
        assert_eq!(
            provider.get("enabled").and_then(toml_edit::Item::as_bool),
            Some(false)
        );
    }
}
