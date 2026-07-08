//! Chelix-managed `defaults.toml` — shipped defaults that users are not
//! expected to edit directly.
//!
//! This file is regenerated on every startup so that new defaults are picked
//! up after an upgrade.  User overrides in `chelix.toml` take precedence.

use {
    crate::schema::ChelixConfig,
    std::path::{Path, PathBuf},
    tracing::{debug, warn},
};

/// Filename for the Chelix-managed defaults file.
pub const DEFAULTS_FILENAME: &str = "defaults.toml";

/// Generate the defaults TOML string from `ChelixConfig::default()`.
///
/// The output is a complete serialization of the built-in defaults with a
/// header comment explaining the ownership model.
pub fn generate_defaults_toml() -> crate::Result<String> {
    let config = ChelixConfig::default();
    let body = toml::to_string_pretty(&config)
        .map_err(|source| crate::Error::external("serialize defaults", source))?;
    Ok(format!("{DEFAULTS_HEADER}{body}"))
}

/// Write (or refresh) `defaults.toml` in the given config directory.
///
/// This is called on every startup.  The file is always overwritten because
/// it is Chelix-managed — user edits belong in `chelix.toml`.
pub fn write_defaults_toml(config_dir: &Path) -> crate::Result<PathBuf> {
    let path = config_dir.join(DEFAULTS_FILENAME);
    std::fs::create_dir_all(config_dir)?;
    let content = generate_defaults_toml()?;
    std::fs::write(&path, &content)?;
    debug!(path = %path.display(), "wrote Chelix-managed defaults.toml");
    Ok(path)
}

/// Load and parse `defaults.toml` from the given config directory.
///
/// Returns `ChelixConfig::default()` if the file does not exist or fails
/// to parse (with a warning).
pub fn load_defaults(config_dir: &Path) -> ChelixConfig {
    let path = config_dir.join(DEFAULTS_FILENAME);
    if !path.exists() {
        return ChelixConfig::default();
    }
    match std::fs::read_to_string(&path) {
        Ok(raw) => match toml::from_str::<ChelixConfig>(&raw) {
            Ok(cfg) => cfg,
            Err(e) => {
                warn!(
                    path = %path.display(),
                    error = %e,
                    "failed to parse defaults.toml, using in-memory defaults"
                );
                ChelixConfig::default()
            },
        },
        Err(e) => {
            warn!(
                path = %path.display(),
                error = %e,
                "failed to read defaults.toml, using in-memory defaults"
            );
            ChelixConfig::default()
        },
    }
}

/// Merge user overrides on top of defaults using TOML-level deep merge.
///
/// The merge loads both files as `toml_edit::DocumentMut`, then walks the
/// user document and applies each key/value on top of the defaults document.
/// This means:
/// - Keys present only in defaults are preserved (user inherits them).
/// - Keys present in both are overridden by the user value.
/// - Keys present only in the user file are added (custom user config).
///
/// The merged document is then parsed into `ChelixConfig`.
pub fn merge_defaults_with_user_toml(
    defaults_toml: &str,
    user_toml: &str,
    path: &Path,
) -> crate::Result<ChelixConfig> {
    let mut base_doc = defaults_toml
        .parse::<toml_edit::DocumentMut>()
        .map_err(|source| crate::Error::external("parse defaults TOML", source))?;
    let user_doc = user_toml
        .parse::<toml_edit::DocumentMut>()
        .map_err(|source| crate::Error::external("parse user TOML", source))?;

    apply_user_overrides(base_doc.as_table_mut(), user_doc.as_table());

    let merged_str = base_doc.to_string();
    let config: ChelixConfig = toml::from_str(&merged_str).map_err(|source| {
        crate::Error::external(
            format!("deserialize merged config from {}", path.display()),
            source,
        )
    })?;
    Ok(config)
}

/// Apply user override table on top of defaults table (recursive deep merge).
///
/// Unlike `merge_toml_tables` in config_io.rs (which removes keys not in
/// the updated doc), this function is additive: defaults keys not mentioned
/// in the user doc are preserved.
fn apply_user_overrides(defaults: &mut toml_edit::Table, user: &toml_edit::Table) {
    for (key, user_item) in user.iter() {
        match (defaults.get_mut(key), user_item) {
            // Both have tables → recurse
            (Some(toml_edit::Item::Table(def_table)), toml_edit::Item::Table(usr_table)) => {
                apply_user_overrides(def_table, usr_table);
            },
            // User overrides a value or introduces a new key
            _ => {
                defaults.insert(key, user_item.clone());
            },
        }
    }
}

// ── Provenance ───────────────────────────────────────────────────────

/// Where a config value came from in the layered config model.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ConfigSource {
    /// Shipped built-in default (from `ChelixConfig::default()`).
    BuiltIn,
    /// User override (from `chelix.toml`).
    UserOverride,
    /// Custom value not present in defaults (user-added).
    Custom,
}

/// Provenance information for an agent preset.
#[derive(Debug, Clone, serde::Serialize)]
pub struct PresetProvenance {
    /// The preset ID.
    pub id: String,
    /// Where this preset comes from.
    pub source: ConfigSource,
}

/// Compute provenance for all agent presets in the effective config.
///
/// Compares the effective config's presets against the built-in defaults
/// to determine which are built-in, overridden, or custom.
pub fn compute_preset_provenance(effective: &crate::schema::AgentsConfig) -> Vec<PresetProvenance> {
    let defaults = ChelixConfig::default();
    let default_presets = &defaults.agents.presets;

    effective
        .presets
        .keys()
        .map(|id| {
            let source = if default_presets.contains_key(id) {
                // Present in defaults — is the effective version identical?
                let eff_toml = toml::to_string(&effective.presets[id]).unwrap_or_default();
                let def_toml = toml::to_string(&default_presets[id]).unwrap_or_default();
                if eff_toml == def_toml {
                    ConfigSource::BuiltIn
                } else {
                    ConfigSource::UserOverride
                }
            } else {
                ConfigSource::Custom
            };
            PresetProvenance {
                id: id.clone(),
                source,
            }
        })
        .collect()
}

/// Check which keys in the user TOML file shadow built-in defaults.
///
/// Returns a list of dotted-path keys that exist in both the user config
/// and the built-in defaults.  Useful for diagnostics.
pub fn find_shadowed_defaults(user_toml: &str) -> Vec<String> {
    let Ok(user_doc) = user_toml.parse::<toml_edit::DocumentMut>() else {
        return Vec::new();
    };
    let Ok(defaults_toml) = generate_defaults_toml() else {
        return Vec::new();
    };
    let Ok(defaults_doc) = defaults_toml.parse::<toml_edit::DocumentMut>() else {
        return Vec::new();
    };

    let mut shadowed = Vec::new();
    collect_shadowed_keys(
        user_doc.as_table(),
        defaults_doc.as_table(),
        &mut String::new(),
        &mut shadowed,
    );
    shadowed
}

fn collect_shadowed_keys(
    user: &toml_edit::Table,
    defaults: &toml_edit::Table,
    prefix: &mut String,
    out: &mut Vec<String>,
) {
    for (key, user_item) in user.iter() {
        let path = if prefix.is_empty() {
            key.to_string()
        } else {
            format!("{prefix}.{key}")
        };

        let Some(def_item) = defaults.get(key) else {
            continue; // Not in defaults — custom key, not a shadow
        };

        match (user_item, def_item) {
            (toml_edit::Item::Table(u), toml_edit::Item::Table(d)) => {
                collect_shadowed_keys(u, d, &mut path.clone(), out);
            },
            (toml_edit::Item::Value(u_val), toml_edit::Item::Value(d_val))
                // Only flag when the user value matches the default — that's
                // a true shadow (frozen default).  Differing values are
                // intentional overrides and should not be reported.
                if u_val.to_string().trim() == d_val.to_string().trim() =>
            {
                out.push(path);
            },
            _ => {},
        }
    }
}

const DEFAULTS_HEADER: &str = "\
# ┌─────────────────────────────────────────────────────────────────────┐
# │  CHELIX-MANAGED DEFAULTS — DO NOT EDIT                             │
# │                                                                     │
# │  This file is regenerated on every startup.  Any manual edits       │
# │  will be lost.  To override a value, set it in chelix.toml         │
# │  instead.                                                           │
# │                                                                     │
# │  Merge order:                                                       │
# │    1. Built-in Rust defaults                                        │
# │    2. This file (defaults.toml)                                     │
# │    3. User overrides (chelix.toml)                                  │
# │    4. Environment variable overrides (CHELIX_*)                     │
# └─────────────────────────────────────────────────────────────────────┘

";
