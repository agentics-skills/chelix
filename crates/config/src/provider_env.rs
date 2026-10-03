use std::collections::HashMap;

fn non_empty_env_value(value: String) -> Option<String> {
    (!value.trim().is_empty()).then_some(value)
}

fn env_value_from_source<F>(
    env_overrides: &HashMap<String, String>,
    key: &str,
    env_lookup: F,
) -> Option<String>
where
    F: FnOnce(&str) -> Option<String>,
{
    env_overrides
        .get(key)
        .cloned()
        .and_then(non_empty_env_value)
        .or_else(|| env_lookup(key).and_then(non_empty_env_value))
}

pub fn env_value_with_overrides(
    env_overrides: &HashMap<String, String>,
    key: &str,
) -> Option<String> {
    env_value_from_source(env_overrides, key, |env_key| std::env::var(env_key).ok())
}

pub fn normalize_provider_name(value: &str) -> Option<String> {
    let normalized = value.trim().to_ascii_lowercase().replace('_', "-");
    (!normalized.is_empty()).then_some(normalized)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn env_value_with_overrides_prefers_overrides() {
        let env_overrides =
            HashMap::from([("CHELIX_API_KEY".to_string(), "override-key".to_string())]);

        assert_eq!(
            env_value_from_source(&env_overrides, "CHELIX_API_KEY", |_| Some(
                "ambient-key".to_string()
            ))
            .as_deref(),
            Some("override-key")
        );
    }
}
