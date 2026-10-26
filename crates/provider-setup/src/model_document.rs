//! Provider model table edits applied to the queued configuration document.
//!
//! Existing model items stay in their current order. A renamed item keeps the
//! enabled flag already stored for that model.

use std::collections::HashMap;

use toml_edit::DocumentMut;

use chelix_config::schema::{ModelConfigMap, PartialModelMetadata};

use chelix_config::toml_document::nested_table;

pub(crate) fn edit_provider_models(
    document: &mut DocumentMut,
    provider: &str,
    previous_id: Option<&str>,
    model_id: &str,
    metadata: Option<&PartialModelMetadata>,
    remove_priority_id: Option<&str>,
) -> chelix_config::Result<ModelConfigMap> {
    let root = document.as_table_mut();
    let providers = nested_table(root, "providers", true)?;
    let provider_table = nested_table(providers, provider, false)?;
    let (mut models, mut items) = read_model_items(provider, provider_table.get("models"))?;
    apply_model_map_edit(&mut models, &mut items, previous_id, model_id, metadata)?;
    if models.is_empty() {
        provider_table.remove("models");
    } else {
        let mut models_table = toml_edit::Table::new();
        for (id, _) in &models {
            let Some(item) = items.remove(id) else {
                return Err(chelix_config::Error::message(format!(
                    "missing TOML item for model `{id}`"
                )));
            };
            models_table.insert(id, item);
        }
        provider_table.insert("models", toml_edit::Item::Table(models_table));
    }
    if let Some(priority_id) = remove_priority_id
        && let Some(chat) = root.get_mut("chat").and_then(toml_edit::Item::as_table_mut)
        && let Some(priority) = chat
            .get_mut("priority_models")
            .and_then(toml_edit::Item::as_array_mut)
    {
        priority.retain(|value| value.as_str() != Some(priority_id));
    }
    Ok(models)
}

fn read_model_items(
    provider: &str,
    models_item: Option<&toml_edit::Item>,
) -> chelix_config::Result<(ModelConfigMap, HashMap<String, toml_edit::Item>)> {
    let mut models = ModelConfigMap::new();
    let mut items = HashMap::new();
    let Some(models_item) = models_item else {
        return Ok((models, items));
    };
    let Some(table) = models_item.as_table() else {
        return Err(chelix_config::Error::message(format!(
            "providers.{provider}.models is not a TOML table"
        )));
    };
    for (key, item) in table.iter() {
        let model_table = item.as_table().ok_or_else(|| {
            chelix_config::Error::message(format!("model `{key}` is not a TOML table"))
        })?;
        let metadata =
            toml::from_str::<PartialModelMetadata>(&model_table.to_string()).map_err(|source| {
                chelix_config::Error::external(format!("parse model `{key}`"), source)
            })?;
        models.insert(key.to_string(), metadata);
        items.insert(key.to_string(), item.clone());
    }
    Ok((models, items))
}

fn apply_model_map_edit(
    models: &mut ModelConfigMap,
    items: &mut HashMap<String, toml_edit::Item>,
    previous_id: Option<&str>,
    model_id: &str,
    metadata: Option<&PartialModelMetadata>,
) -> chelix_config::Result<()> {
    match metadata {
        None => {
            if models.shift_remove(model_id).is_none() {
                return Err(chelix_config::Error::message(format!(
                    "model `{model_id}` is not configured"
                )));
            }
            items.remove(model_id);
        },
        Some(metadata) => {
            let mut metadata = metadata.clone();
            if let Some(source_id) = previous_id.filter(|id| items.contains_key(*id))
                && let Some(existing) = items.get(source_id)
            {
                metadata.enabled = model_item_enabled(existing);
            }
            let item = model_item(previous_id, model_id, &metadata, items)?;
            match previous_id {
                Some(previous) if previous != model_id => {
                    let Some(index) = models.get_index_of(previous) else {
                        return Err(chelix_config::Error::message(format!(
                            "model `{previous}` is not configured"
                        )));
                    };
                    if models.contains_key(model_id) {
                        return Err(chelix_config::Error::message(format!(
                            "model `{model_id}` is already configured"
                        )));
                    }
                    models.shift_remove(previous);
                    models.shift_insert(index, model_id.to_string(), metadata.clone());
                    items.remove(previous);
                },
                Some(_) => {
                    if !models.contains_key(model_id) {
                        return Err(chelix_config::Error::message(format!(
                            "model `{model_id}` is not configured"
                        )));
                    }
                    models.insert(model_id.to_string(), metadata.clone());
                },
                None => {
                    if models.contains_key(model_id) {
                        return Err(chelix_config::Error::message(format!(
                            "model `{model_id}` is already configured"
                        )));
                    }
                    models.insert(model_id.to_string(), metadata.clone());
                },
            }
            items.insert(model_id.to_string(), item);
        },
    }
    Ok(())
}

fn model_item_enabled(item: &toml_edit::Item) -> bool {
    item.as_table()
        .and_then(|table| table.get("enabled"))
        .and_then(toml_edit::Item::as_bool)
        .unwrap_or(true)
}

fn model_item(
    previous_id: Option<&str>,
    model_id: &str,
    metadata: &PartialModelMetadata,
    items: &HashMap<String, toml_edit::Item>,
) -> chelix_config::Result<toml_edit::Item> {
    let text = toml::to_string(metadata)
        .map_err(|source| chelix_config::Error::external("serialize model metadata", source))?;
    let document = text
        .parse::<DocumentMut>()
        .map_err(|source| chelix_config::Error::external("parse model metadata", source))?;
    let source_key = previous_id.unwrap_or(model_id);
    let mut item = items
        .get(source_key)
        .cloned()
        .unwrap_or_else(|| toml_edit::Item::Table(toml_edit::Table::new()));
    let table = item.as_table_mut().ok_or_else(|| {
        chelix_config::Error::message(format!("model `{source_key}` is not a TOML table"))
    })?;
    let stale = table
        .iter()
        .map(|(key, _)| key.to_string())
        .collect::<Vec<_>>();
    for key in stale {
        table.remove(&key);
    }
    for (key, value) in document.as_table().iter() {
        table.insert(key, value.clone());
    }
    Ok(item)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    fn metadata() -> PartialModelMetadata {
        PartialModelMetadata {
            context_length: Some(4),
            enabled: true,
            ..PartialModelMetadata::default()
        }
    }

    fn model_keys(document: &DocumentMut) -> Vec<String> {
        document["providers"]["local-ai"]["models"]
            .as_table()
            .expect("models")
            .iter()
            .map(|(key, _)| key.to_string())
            .collect()
    }

    #[test]
    fn rename_preserves_model_order_and_stored_enabled_flag() {
        let mut document = r#"
[providers.local-ai.models.alpha]
context_length = 1

[providers.local-ai.models.beta]
context_length = 2
enabled = false

[providers.local-ai.models.gamma]
context_length = 3

[chat]
priority_models = ["local-ai::beta", "local-ai::gamma"]
"#
        .parse::<DocumentMut>()
        .expect("document");

        let models = edit_provider_models(
            &mut document,
            "local-ai",
            Some("beta"),
            "delta",
            Some(&metadata()),
            Some("local-ai::beta"),
        )
        .expect("rename model");

        assert_eq!(models.keys().cloned().collect::<Vec<_>>(), vec![
            "alpha".to_string(),
            "delta".to_string(),
            "gamma".to_string()
        ]);
        assert_eq!(model_keys(&document), vec!["alpha", "delta", "gamma"]);
        assert_eq!(
            document["providers"]["local-ai"]["models"]["delta"]["enabled"].as_bool(),
            Some(false)
        );
        let priority = document["chat"]["priority_models"]
            .as_array()
            .expect("priority")
            .iter()
            .filter_map(toml_edit::Value::as_str)
            .collect::<Vec<_>>();
        assert_eq!(priority, vec!["local-ai::gamma"]);
    }

    #[test]
    fn delete_removes_one_model_item_and_its_priority_id() {
        let mut document = r#"
[providers.local-ai.models.alpha]
context_length = 1

[providers.local-ai.models.beta]
context_length = 2

[chat]
priority_models = ["local-ai::alpha"]
"#
        .parse::<DocumentMut>()
        .expect("document");

        let models = edit_provider_models(
            &mut document,
            "local-ai",
            None,
            "alpha",
            None,
            Some("local-ai::alpha"),
        )
        .expect("delete model");

        assert_eq!(models.keys().cloned().collect::<Vec<_>>(), vec![
            "beta".to_string()
        ]);
        assert_eq!(model_keys(&document), vec!["beta"]);
        assert!(
            document["chat"]["priority_models"]
                .as_array()
                .expect("priority")
                .is_empty()
        );
    }
}
