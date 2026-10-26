use std::path::Path;

use {
    chelix_config::ChelixConfig,
    toml_edit::{DocumentMut, Item, Table},
};

pub(crate) fn parse(raw: &str) -> chelix_config::Result<DocumentMut> {
    raw.parse()
        .map_err(|error| chelix_config::Error::external("parse configuration document", error))
}

pub(crate) fn update<R>(
    document: &mut DocumentMut,
    path: &Path,
    patch: impl FnOnce(&mut ChelixConfig) -> chelix_config::Result<R>,
) -> chelix_config::Result<R> {
    let mut config = chelix_config::parse_raw_config(&document.to_string(), path)?;
    let before = serialized(&config)?;
    let result = patch(&mut config)?;
    let after = serialized(&config)?;
    patch_table(document.as_table_mut(), before.as_table(), after.as_table());
    Ok(result)
}

fn serialized(config: &ChelixConfig) -> chelix_config::Result<DocumentMut> {
    let raw = toml::to_string_pretty(config)
        .map_err(|error| chelix_config::Error::external("serialize configuration patch", error))?;
    parse(&raw)
}

fn same_table(before: &Table, after: &Table) -> bool {
    before.len() == after.len()
        && before
            .iter()
            .all(|(key, value)| after.get(key).is_some_and(|next| same_item(value, next)))
}

fn same_item(before: &Item, after: &Item) -> bool {
    match (before, after) {
        (Item::Table(before), Item::Table(after)) => same_table(before, after),
        (Item::ArrayOfTables(before), Item::ArrayOfTables(after)) => {
            before.len() == after.len()
                && before
                    .iter()
                    .zip(after.iter())
                    .all(|(before, after)| same_table(before, after))
        },
        (Item::Value(before), Item::Value(after)) => before.to_string() == after.to_string(),
        (Item::None, Item::None) => true,
        _ => false,
    }
}

fn patch_table(target: &mut Table, before: &Table, after: &Table) {
    for (key, _) in before {
        if !after.contains_key(key) {
            target.remove(key);
        }
    }
    for (key, next) in after {
        let previous = before.get(key);
        if previous.is_some_and(|old| same_item(old, next)) {
            continue;
        }
        if let (Some(Item::Table(old)), Item::Table(new)) = (previous, next) {
            if !target.contains_key(key) {
                target.insert(key, Item::Table(Table::new()));
            }
            if let Some(Item::Table(current)) = target.get_mut(key) {
                patch_table(current, old, new);
                continue;
            }
        }
        match (target.get_mut(key), next) {
            (Some(Item::Value(current)), Item::Value(next)) => {
                let decor = current.decor().clone();
                *current = next.clone();
                *current.decor_mut() = decor;
            },
            _ => {
                target.insert(key, clone_item(next));
            },
        }
    }
}

fn clone_item(item: &Item) -> Item {
    match item {
        Item::Table(table) => Item::Table(clone_table(table)),
        Item::ArrayOfTables(tables) => {
            let mut result = toml_edit::ArrayOfTables::new();
            for table in tables {
                result.push(clone_table(table));
            }
            Item::ArrayOfTables(result)
        },
        other => other.clone(),
    }
}

fn clone_table(source: &Table) -> Table {
    let mut target = Table::new();
    target.set_implicit(source.is_implicit());
    target.set_dotted(source.is_dotted());
    *target.decor_mut() = source.decor().clone();
    for (key, item) in source {
        target.insert(key, clone_item(item));
        if let (Some(old), Some(mut new)) = (source.key(key), target.key_mut(key)) {
            *new.leaf_decor_mut() = old.leaf_decor().clone();
            *new.dotted_decor_mut() = old.dotted_decor().clone();
        }
    }
    target
}
