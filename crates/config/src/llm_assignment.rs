//! Paths whose values assign an LLM provider, model, or token.
//!
//! `${...}` substitution on these paths fails config load.

use serde_json::Value;

pub(crate) fn skips_substitution(path: &[String]) -> bool {
    if path.iter().any(|segment| segment == "model_override") {
        return true;
    }
    if path.first().is_some_and(|segment| segment == "providers") {
        return true;
    }
    if starts_with(path, &["chat", "priority_models"])
        || starts_with(path, &["auxiliary", "title_generation"])
    {
        return true;
    }
    path.len() >= 3 && path[0] == "agents" && path[1] != "default" && path[2] == "model"
}

pub(crate) fn substitute_json_strings(
    value: &mut Value,
    lookup: &impl Fn(&str) -> Option<String>,
) -> crate::Result<()> {
    let mut path = Vec::new();
    walk(value, &mut path, lookup)
}

fn walk(
    value: &mut Value,
    path: &mut Vec<String>,
    lookup: &impl Fn(&str) -> Option<String>,
) -> crate::Result<()> {
    match value {
        Value::String(text) if skips_substitution(path) && text.contains("${") => {
            return Err(crate::Error::message(format!(
                "environment substitution is not allowed at {}",
                path.join(".")
            )));
        },
        Value::String(text) if !skips_substitution(path) && text.contains("${") => {
            *text = crate::env_subst::substitute_env_with(text, lookup);
        },
        Value::Object(map) => {
            for (key, child) in map.iter_mut() {
                path.push(key.clone());
                walk(child, path, lookup)?;
                path.pop();
            }
        },
        Value::Array(items) => {
            for child in items {
                walk(child, path, lookup)?;
            }
        },
        _ => {},
    }
    Ok(())
}

fn starts_with(path: &[String], prefix: &[&str]) -> bool {
    path.len() >= prefix.len()
        && path
            .iter()
            .zip(prefix.iter())
            .all(|(segment, expected)| segment == expected)
}
