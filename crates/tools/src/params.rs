//! Shared helpers for extracting typed parameters from `serde_json::Value`.
//!
//! These reduce boilerplate in `AgentTool::execute` implementations that
//! manually pull fields from a JSON object.

use serde_json::Value;

use crate::Error;

/// Extract a trimmed, non-empty `&str` from a JSON object field.
///
/// Returns `None` when the key is absent, null, not a string, empty,
/// or whitespace-only.
pub fn str_param<'a>(params: &'a Value, key: &str) -> Option<&'a str> {
    params
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|v| !v.is_empty())
}

/// Try multiple keys in order and return the first non-empty string match.
pub fn str_param_any<'a>(params: &'a Value, keys: &[&str]) -> Option<&'a str> {
    keys.iter().find_map(|key| str_param(params, key))
}

/// Like [`str_param`] but returns a `crate::Error` when missing.
pub fn require_str<'a>(params: &'a Value, key: &str) -> crate::Result<&'a str> {
    str_param(params, key)
        .ok_or_else(|| Error::message(format!("missing required parameter: {key}")))
}

/// Extract an optional array of trimmed, non-empty strings.
///
/// Returns an empty vector when the key is absent or explicitly `null`.
pub fn string_array_param(params: &Value, key: &str) -> crate::Result<Vec<String>> {
    let Some(raw) = params.get(key) else {
        return Ok(Vec::new());
    };
    if raw.is_null() {
        return Ok(Vec::new());
    }

    let arr = raw
        .as_array()
        .ok_or_else(|| Error::message(format!("parameter '{key}' must be an array")))?;

    let mut out = Vec::with_capacity(arr.len());
    for (idx, item) in arr.iter().enumerate() {
        let name = item
            .as_str()
            .ok_or_else(|| Error::message(format!("parameter '{key}[{idx}]' must be a string")))?;
        let trimmed = name.trim();
        if trimmed.is_empty() {
            return Err(Error::message(format!(
                "parameter '{key}[{idx}]' cannot be empty"
            )));
        }
        out.push(trimmed.to_string());
    }

    Ok(out)
}

/// Extract a boolean, defaulting to `default` when absent.
pub fn bool_param(params: &Value, key: &str, default: bool) -> bool {
    params.get(key).and_then(Value::as_bool).unwrap_or(default)
}

/// Extract a `u64`, defaulting to `default` when absent.
pub fn u64_param(params: &Value, key: &str, default: u64) -> u64 {
    params.get(key).and_then(Value::as_u64).unwrap_or(default)
}

/// Extract an owned `String` from the first matching key.
pub fn owned_str_param(params: &Value, keys: &[&str]) -> Option<String> {
    str_param_any(params, keys).map(String::from)
}

/// Recursively drop `null` entries from JSON object parameters.
///
/// Model callers sometimes pass explicit `null` for optional fields; treat
/// those values as omitted. Object fields that become empty after stripping
/// are removed as well.
#[must_use]
pub fn without_null_params(mut params: Value) -> Value {
    prune_null_entries(&mut params);
    params
}

fn prune_null_entries(value: &mut Value) {
    if let Some(map) = value.as_object_mut() {
        map.retain(|_, child| {
            if child.is_null() {
                return false;
            }
            prune_null_entries(child);
            !child.as_object().is_some_and(serde_json::Map::is_empty)
        });
    }
}

#[cfg(test)]
mod tests {
    use {super::*, serde_json::json};

    #[test]
    fn str_param_extracts_trimmed_value() {
        let p = json!({"name": "  hello  "});
        assert_eq!(str_param(&p, "name"), Some("hello"));
    }

    #[test]
    fn str_param_returns_none_for_missing_key() {
        let p = json!({});
        assert_eq!(str_param(&p, "name"), None);
    }

    #[test]
    fn str_param_returns_none_for_empty_string() {
        let p = json!({"name": ""});
        assert_eq!(str_param(&p, "name"), None);
    }

    #[test]
    fn str_param_returns_none_for_whitespace_only() {
        let p = json!({"name": "   "});
        assert_eq!(str_param(&p, "name"), None);
    }

    #[test]
    fn str_param_any_finds_first_match() {
        let p = json!({"chatId": "42"});
        assert_eq!(str_param_any(&p, &["chat_id", "chatId"]), Some("42"));
    }

    #[test]
    fn require_str_errors_when_missing() {
        let p = json!({});
        assert!(require_str(&p, "key").is_err());
    }

    #[test]
    fn string_array_param_returns_empty_for_missing_or_null() {
        let missing = json!({});
        let explicit_null = json!({"tools": null});
        assert!(matches!(
            string_array_param(&missing, "tools"),
            Ok(values) if values.is_empty()
        ));
        assert!(matches!(
            string_array_param(&explicit_null, "tools"),
            Ok(values) if values.is_empty()
        ));
    }

    #[test]
    fn string_array_param_trims_values() {
        let p = json!({"tools": [" execute_command ", "task_list"]});
        assert!(matches!(
            string_array_param(&p, "tools"),
            Ok(values) if values == vec!["execute_command".to_string(), "task_list".to_string()]
        ));
    }

    #[test]
    fn string_array_param_rejects_wrong_types() {
        let not_array = json!({"tools": true});
        let non_string = json!({"tools": ["execute_command", 42]});
        assert!(string_array_param(&not_array, "tools").is_err());
        assert!(string_array_param(&non_string, "tools").is_err());
    }

    #[test]
    fn bool_param_returns_value_or_default() {
        let p = json!({"force": true});
        assert!(bool_param(&p, "force", false));
        assert!(!bool_param(&p, "missing", false));
    }

    #[test]
    fn u64_param_returns_value_or_default() {
        let p = json!({"limit": 50});
        assert_eq!(u64_param(&p, "limit", 20), 50);
        assert_eq!(u64_param(&p, "missing", 20), 20);
    }

    #[test]
    fn without_null_params_drops_null_values() {
        let p = json!({
            "key": "session:x",
            "label": null,
            "model_override": null
        });
        let cleaned = without_null_params(p);
        assert_eq!(cleaned, json!({"key": "session:x"}));
    }

    #[test]
    fn without_null_params_drops_nested_nulls_and_empty_objects() {
        let p = json!({
            "key": "session:x",
            "model_override": {
                "model": null,
                "reasoning_effort": null
            }
        });
        let cleaned = without_null_params(p);
        assert_eq!(cleaned, json!({"key": "session:x"}));
    }

    #[test]
    fn without_null_params_keeps_non_null_values() {
        let p = json!({
            "key": "session:x",
            "model_override": {
                "model": "provider::model",
                "reasoning_effort": "high"
            },
            "wait_for_reply": false
        });
        assert_eq!(without_null_params(p.clone()), p);
    }
}
