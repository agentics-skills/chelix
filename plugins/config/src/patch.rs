//! Applies `set` and `remove` operations to a `toml_edit` document.

use serde_json::Value;
use toml_edit::{DocumentMut, Item, Table, Value as EditValue};

use crate::contract::{PatchOp, INVALID_PATH, INVALID_VALUE, PATH_CONFLICT};
use chelix_call_bus::jsonrpc::RpcError;

pub fn apply(document: &mut DocumentMut, ops: &[PatchOp], file: &str) -> Result<Vec<String>, RpcError> {
    let before = top_level(document);
    for op in ops {
        match op {
            PatchOp::Set { path, value } => set(document, path, value, file)?,
            PatchOp::Remove { path } => remove(document, path, file)?,
        }
    }
    Ok(changed_sections(&before, document))
}

fn set(document: &mut DocumentMut, path: &str, value: &Value, file: &str) -> Result<(), RpcError> {
    if value.is_null() {
        return Err(invalid_value(file, path));
    }
    let parts = split_path(path, file)?;
    set_in_table(document.as_table_mut(), &parts, value, file, path)
}

fn set_in_table(
    table: &mut Table,
    parts: &[String],
    value: &Value,
    file: &str,
    path: &str,
) -> Result<(), RpcError> {
    let Some((head, rest)) = parts.split_first() else {
        return Err(invalid_path(file, path));
    };
    if rest.is_empty() {
        let item = json_to_item(value).map_err(|_| invalid_value(file, path))?;
        table.insert(head, item);
        return Ok(());
    }
    if !table.contains_key(head) {
        table.insert(head, Item::Table(Table::new()));
    }
    let child = table.get_mut(head).ok_or_else(|| invalid_path(file, path))?;
    if let Some(next) = child.as_table_mut() {
        return set_in_table(next, rest, value, file, path);
    }
    if let Some(inline) = child.as_inline_table_mut() {
        return set_in_inline(inline, rest, value, file, path);
    }
    Err(path_conflict(file, path, head))
}

fn set_in_inline(
    table: &mut toml_edit::InlineTable,
    parts: &[String],
    value: &Value,
    file: &str,
    path: &str,
) -> Result<(), RpcError> {
    let Some((head, rest)) = parts.split_first() else {
        return Err(invalid_path(file, path));
    };
    if rest.is_empty() {
        let written = json_to_inline_value(value).map_err(|_| invalid_value(file, path))?;
        table.insert(head, written);
        return Ok(());
    }
    if !table.contains_key(head) {
        table.insert(head, EditValue::InlineTable(toml_edit::InlineTable::new()));
    }
    let child = table.get_mut(head).ok_or_else(|| invalid_path(file, path))?;
    let EditValue::InlineTable(next) = child else {
        return Err(path_conflict(file, path, head));
    };
    set_in_inline(next, rest, value, file, path)
}

fn remove(document: &mut DocumentMut, path: &str, file: &str) -> Result<(), RpcError> {
    let parts = split_path(path, file)?;
    remove_in_table(document.as_table_mut(), &parts, file, path)
}

fn remove_in_table(table: &mut Table, parts: &[String], file: &str, path: &str) -> Result<(), RpcError> {
    let Some((head, rest)) = parts.split_first() else {
        return Err(invalid_path(file, path));
    };
    if rest.is_empty() {
        table.remove(head);
        return Ok(());
    }
    let Some(child) = table.get_mut(head) else {
        return Ok(());
    };
    if let Some(next) = child.as_table_mut() {
        return remove_in_table(next, rest, file, path);
    }
    if let Some(inline) = child.as_inline_table_mut() {
        return remove_in_inline(inline, rest, file, path);
    }
    Err(path_conflict(file, path, head))
}

fn remove_in_inline(
    table: &mut toml_edit::InlineTable,
    parts: &[String],
    file: &str,
    path: &str,
) -> Result<(), RpcError> {
    let Some((head, rest)) = parts.split_first() else {
        return Err(invalid_path(file, path));
    };
    if rest.is_empty() {
        table.remove(head);
        return Ok(());
    }
    let Some(child) = table.get_mut(head) else {
        return Ok(());
    };
    let EditValue::InlineTable(next) = child else {
        return Err(path_conflict(file, path, head));
    };
    remove_in_inline(next, rest, file, path)
}

fn json_to_item(value: &Value) -> Result<Item, ()> {
    match value {
        Value::Object(object) => {
            let mut table = Table::new();
            for (key, item) in object {
                table.insert(key, json_to_item(item)?);
            }
            Ok(Item::Table(table))
        },
        other => Ok(Item::Value(json_to_inline_value(other)?)),
    }
}

fn json_to_inline_value(value: &Value) -> Result<EditValue, ()> {
    match value {
        Value::Null => Err(()),
        Value::Bool(flag) => Ok(EditValue::from(*flag)),
        Value::String(text) => Ok(EditValue::from(text.as_str())),
        Value::Number(number) => {
            if let Some(integer) = number.as_i64() {
                Ok(EditValue::from(integer))
            } else if let Some(float) = number.as_f64() {
                Ok(EditValue::from(float))
            } else {
                Err(())
            }
        },
        Value::Array(items) => {
            let mut array = toml_edit::Array::new();
            for item in items {
                array.push(json_to_inline_value(item)?);
            }
            Ok(EditValue::Array(array))
        },
        Value::Object(object) => {
            let mut inline = toml_edit::InlineTable::new();
            for (key, item) in object {
                inline.insert(key, json_to_inline_value(item)?);
            }
            Ok(EditValue::InlineTable(inline))
        },
    }
}

fn split_path(path: &str, file: &str) -> Result<Vec<String>, RpcError> {
    let keys = toml_edit::Key::parse(path).map_err(|_| invalid_path(file, path))?;
    let parts = keys.iter().map(|key| key.get().to_owned()).collect::<Vec<_>>();
    if parts.is_empty() {
        return Err(invalid_path(file, path));
    }
    Ok(parts)
}

fn top_level(document: &DocumentMut) -> Vec<(String, String)> {
    document
        .as_table()
        .iter()
        .map(|(key, item)| (key.to_owned(), item.to_string()))
        .collect()
}

fn changed_sections(before: &[(String, String)], document: &DocumentMut) -> Vec<String> {
    let after = top_level(document);
    let mut sections = Vec::new();
    for (key, value) in &after {
        let previous = before.iter().find(|(name, _)| name == key);
        if previous.map(|(_, text)| text) != Some(value) {
            sections.push(key.clone());
        }
    }
    for (key, _) in before {
        if after.iter().all(|(name, _)| name != key) {
            sections.push(key.clone());
        }
    }
    sections
}

fn invalid_path(file: &str, path: &str) -> RpcError {
    RpcError::new(INVALID_PATH, format!("{file}: invalid path {path}"))
        .with_data(serde_json::json!({"path": path}))
}

fn invalid_value(file: &str, path: &str) -> RpcError {
    RpcError::new(INVALID_VALUE, format!("{file}: invalid value at {path}"))
        .with_data(serde_json::json!({"path": path}))
}

fn path_conflict(file: &str, path: &str, found: &str) -> RpcError {
    RpcError::new(PATH_CONFLICT, format!("{file}: {found} is not a table"))
        .with_data(serde_json::json!({"path": path, "found": found}))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contract::PatchOp;

    fn apply_ops(text: &str, ops: &[PatchOp]) -> Result<String, RpcError> {
        let mut document: DocumentMut = text.parse().expect("toml");
        apply(&mut document, ops, "chelix.toml")?;
        Ok(document.to_string())
    }

    #[test]
    fn set_creates_standard_tables_and_keeps_comment() {
        let text = apply_ops(
            "# keep\n[section]\nkey = 1\n",
            &[PatchOp::Set {
                path: "section.other".into(),
                value: Value::from(2),
            }],
        )
        .expect("patch");
        assert!(text.contains("# keep"));
        assert!(text.contains("other = 2"));
    }

    #[test]
    fn set_inside_inline_table_stays_inline() {
        let text = apply_ops(
            "a = { b = 1 }\n",
            &[PatchOp::Set {
                path: "a.c".into(),
                value: serde_json::json!({"d": 1}),
            }],
        )
        .expect("patch");
        assert!(text.contains("b = 1"));
        assert!(text.contains("d = 1"));
        assert!(!text.contains("[a.c]"));
    }

    #[test]
    fn quoted_dotted_key_is_one_segment() {
        let text = apply_ops(
            "",
            &[PatchOp::Set {
                path: "agents.\"my.agent\".model".into(),
                value: Value::from("gpt"),
            }],
        )
        .expect("patch");
        assert!(text.contains("my.agent") || text.contains("\"my.agent\""));
        assert!(text.contains("gpt"));
    }

    #[test]
    fn remove_deletes_key() {
        let text = apply_ops(
            "key = 1\n",
            &[PatchOp::Remove { path: "key".into() }],
        )
        .expect("patch");
        assert!(!text.contains("key"));
    }

    #[test]
    fn null_value_is_invalid() {
        let error = apply_ops(
            "",
            &[PatchOp::Set {
                path: "key".into(),
                value: Value::Null,
            }],
        )
        .expect_err("null");
        assert_eq!(error.code, INVALID_VALUE);
    }

    #[test]
    fn path_conflict_on_scalar() {
        let error = apply_ops(
            "key = 1\n",
            &[PatchOp::Set {
                path: "key.child".into(),
                value: Value::from(2),
            }],
        )
        .expect_err("conflict");
        assert_eq!(error.code, PATH_CONFLICT);
    }
}
