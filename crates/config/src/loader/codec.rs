//! Parse `chelix.toml` text into [`ChelixConfig`](crate::schema::ChelixConfig).
//!
//! [`parse_raw_config`] deserializes a document and rejects unknown fields.
//! [`parse_config_source`] does the same after two-pass `${VAR}` substitution
//! from that document's `[env]` table.

use std::path::Path;

use crate::schema::ChelixConfig;

/// Parse raw text into the typed configuration.
///
/// Unknown fields are rejected. `path` is included in error context only.
pub fn parse_raw_config(raw: &str, path: &Path) -> crate::Result<ChelixConfig> {
    let value = parse_config_value(raw)?;
    config_from_value(value, path)
}

/// Parse `raw` with the two-pass `[env]` substitution used when a document is loaded.
///
/// The first pass reads `[env]`. The second pass substitutes those values, then
/// parses the substituted document. Unresolved placeholders are left unchanged.
/// `path` is included in error context only.
pub fn parse_config_source(raw: &str, path: &Path) -> crate::Result<ChelixConfig> {
    let env = parse_raw_config(raw, path)?.env;
    let value = substituted_config_value(raw, &|name| env.get(name).cloned())?;
    config_from_value(value, path)
}

/// Re-resolve `${VAR}` placeholders in a loaded config using `overrides`.
///
/// Call this after runtime values (for example UI variables stored in the
/// database) become available. Substitution happens at the JSON value level,
/// so override values that contain quotes or backslashes stay literal.
pub fn resubstitute_config(
    config: &ChelixConfig,
    overrides: &std::collections::HashMap<String, String>,
) -> crate::Result<ChelixConfig> {
    let mut json = serde_json::to_value(config)
        .map_err(|source| crate::Error::external("serialize config for resubstitution", source))?;
    crate::llm_assignment::substitute_json_strings(&mut json, &|name| {
        overrides.get(name).cloned()
    })?;
    serde_json::from_value(json)
        .map_err(|source| crate::Error::external("deserialize config after resubstitution", source))
}

fn substituted_config_value(
    raw: &str,
    lookup: &impl Fn(&str) -> Option<String>,
) -> crate::Result<serde_json::Value> {
    let mut value = parse_config_value(raw)?;
    crate::llm_assignment::substitute_json_strings(&mut value, lookup)?;
    Ok(value)
}

fn config_from_value(value: serde_json::Value, path: &Path) -> crate::Result<ChelixConfig> {
    let context = format!("config {}", path.display());
    validate_config_shape(&value, &context)?;
    serde_json::from_value(value)
        .map_err(|source| crate::Error::external(format!("invalid {context}"), source))
}

fn validate_config_shape(value: &serde_json::Value, context: &str) -> crate::Result<()> {
    let schema = crate::validate::schema_map::build_schema_map();
    let mut diagnostics = Vec::new();
    crate::validate::schema_map::check_unknown_fields(value, &schema, "", &mut diagnostics);
    if diagnostics.is_empty() {
        return Ok(());
    }
    let details = diagnostics
        .into_iter()
        .map(|diagnostic| format!("{}: {}", diagnostic.path, diagnostic.message))
        .collect::<Vec<_>>()
        .join("; ");
    Err(crate::Error::message(format!(
        "invalid {context}: {details}"
    )))
}

fn parse_config_value(raw: &str) -> crate::Result<serde_json::Value> {
    let value: toml::Value = toml::from_str(raw)?;
    Ok(serde_json::to_value(value)?)
}
