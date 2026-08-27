//! Configuration helpers for provider credentials.

use std::collections::HashMap;

use {chelix_config::schema::ProvidersConfig, secrecy::ExposeSecret};

/// Resolve an env value from overrides or process environment.
pub(crate) fn env_value(env_overrides: &HashMap<String, String>, key: &str) -> Option<String> {
    chelix_config::env_value_with_overrides(env_overrides, key)
}

/// Resolve an API key from config or environment without exposing it.
pub(crate) fn resolve_api_key(
    config: &ProvidersConfig,
    provider: &str,
    env_key: &str,
    env_overrides: &HashMap<String, String>,
) -> Option<secrecy::Secret<String>> {
    config
        .get(provider)
        .and_then(|entry| entry.api_key.clone())
        .or_else(|| env_value(env_overrides, env_key).map(secrecy::Secret::new))
        .or_else(|| chelix_config::generic_provider_api_key_from_env(provider, env_overrides))
        .filter(|secret| !secret.expose_secret().is_empty())
}
