use {
    super::config_io::{nested_table, write_config_file},
    crate::loader::find_or_default_config_path,
};

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
    pub removed_model_ids: Vec<String>,
    pub renamed_model_ids: Vec<(String, String)>,
}

pub fn upsert_openai_compatible_provider_toml(
    update: &OpenAiCompatibleProviderTomlUpdate,
) -> crate::Result<OpenAiCompatibleProviderTomlResult> {
    with_provider_document(|root| apply_upsert(root, update))
}

pub fn delete_openai_compatible_provider_toml(name: &str) -> crate::Result<Vec<String>> {
    with_provider_document(|root| {
        let removed = {
            let providers = providers_table(root)?;
            let Some(item) = providers.remove(name) else {
                return Err(crate::Error::message(format!(
                    "provider '{name}' is not configured"
                )));
            };
            model_ids(name, &item)
        };
        remove_priority_prefix(root, &format!("{name}::"));
        Ok(removed)
    })
}

fn with_provider_document<T>(
    edit: impl FnOnce(&mut toml_edit::Table) -> crate::Result<T>,
) -> crate::Result<T> {
    let path = find_or_default_config_path();
    let is_toml = path
        .extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| ext.eq_ignore_ascii_case("toml"));
    if !is_toml {
        return Err(crate::Error::message(
            "OpenAI Compatible providers can only be edited in a TOML config",
        ));
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let raw = if path.exists() {
        std::fs::read_to_string(&path).map_err(|source| {
            crate::Error::external(format!("failed to read {}", path.display()), source)
        })?
    } else {
        String::new()
    };
    let mut document = if raw.trim().is_empty() {
        toml_edit::DocumentMut::new()
    } else {
        raw.parse::<toml_edit::DocumentMut>().map_err(|source| {
            crate::Error::external(format!("failed to parse {}", path.display()), source)
        })?
    };
    let result = edit(document.as_table_mut())?;
    write_config_file(&path, document.to_string()).map_err(|source| {
        crate::Error::external(format!("failed to write {}", path.display()), source)
    })?;
    Ok(result)
}

fn apply_upsert(
    root: &mut toml_edit::Table,
    update: &OpenAiCompatibleProviderTomlUpdate,
) -> crate::Result<OpenAiCompatibleProviderTomlResult> {
    let previous = update
        .previous_name
        .as_deref()
        .filter(|name| *name != update.name);
    let renamed = if let Some(previous) = previous {
        let providers = providers_table(root)?;
        if providers.contains_key(&update.name) {
            return Err(crate::Error::message(format!(
                "provider '{}' already exists",
                update.name
            )));
        }
        let Some(item) = providers.remove(previous) else {
            return Err(crate::Error::message(format!(
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
            return Err(crate::Error::message(format!(
                "provider '{}' already exists",
                update.name
            )));
        }
        if update.previous_name.is_some() && !providers.contains_key(&update.name) {
            return Err(crate::Error::message(format!(
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
        removed_model_ids: Vec::new(),
        renamed_model_ids: renamed,
    })
}

pub fn write_provider_api_key(
    name: &str,
    api_key: &str,
    base_url: Option<&str>,
    write_base_url: bool,
) -> crate::Result<()> {
    with_provider_document(|root| {
        let providers = providers_table(root)?;
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
    })
}

pub fn set_provider_enabled_flag(name: &str, enabled: bool) -> crate::Result<()> {
    with_provider_document(|root| {
        let providers = providers_table(root)?;
        let table = nested_table(providers, name, false)?;
        table.insert("enabled", toml_edit::value(enabled));
        Ok(())
    })
}

pub fn clear_provider_api_key(name: &str) -> crate::Result<()> {
    with_provider_document(|root| {
        let providers = providers_table(root)?;
        if !providers.contains_key(name) {
            return Err(crate::Error::message(format!(
                "provider '{name}' is not configured"
            )));
        }
        let table = nested_table(providers, name, false)?;
        table.remove("api_key");
        table.insert("enabled", toml_edit::value(false));
        Ok(())
    })
}

pub fn set_model_enabled_in_toml(
    provider: &str,
    model_id: &str,
    enabled: bool,
) -> crate::Result<()> {
    with_provider_document(|root| {
        let providers = providers_table(root)?;
        let Some(provider_item) = providers.get_mut(provider) else {
            return Err(crate::Error::message(format!(
                "provider '{provider}' is not configured"
            )));
        };
        let Some(provider_table) = provider_item.as_table_mut() else {
            return Err(crate::Error::message(format!(
                "providers.{provider} is not a table"
            )));
        };
        let Some(models_item) = provider_table.get_mut("models") else {
            return Err(crate::Error::message(format!(
                "model '{model_id}' is not configured"
            )));
        };
        let Some(models) = models_item.as_table_mut() else {
            return Err(crate::Error::message(format!(
                "providers.{provider}.models is not a table"
            )));
        };
        let Some(model_item) = models.get_mut(model_id) else {
            return Err(crate::Error::message(format!(
                "model '{model_id}' is not configured"
            )));
        };
        let Some(model) = model_item.as_table_mut() else {
            return Err(crate::Error::message(format!(
                "model '{model_id}' is not a table"
            )));
        };
        model.insert("enabled", toml_edit::value(enabled));
        Ok(())
    })
}

pub fn set_config_strings(updates: &[(&[&str], Option<&str>)]) -> crate::Result<()> {
    with_provider_document(|root| {
        for (path, value) in updates {
            set_string_path(root, path, *value)?;
        }
        Ok(())
    })
}

fn set_string_path(
    root: &mut toml_edit::Table,
    path: &[&str],
    value: Option<&str>,
) -> crate::Result<()> {
    let [head, rest @ ..] = path else {
        return Err(crate::Error::message("config path is empty"));
    };
    if rest.is_empty() {
        match value.map(str::trim).filter(|text| !text.is_empty()) {
            Some(text) => {
                root.insert(head, toml_edit::value(text));
            },
            None => {
                root.remove(head);
            },
        }
        return Ok(());
    }
    let child = nested_table(root, head, false)?;
    set_string_path(child, rest, value)
}

fn providers_table(root: &mut toml_edit::Table) -> crate::Result<&mut toml_edit::Table> {
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
