use {
    super::*,
    crate::schema::ChelixConfig,
    std::{
        path::{Path, PathBuf},
        sync::Mutex,
    },
    tracing::{debug, info},
};

/// Write content to a file atomically via write-to-temp + rename.
///
/// This prevents corruption when two processes (e.g. CLI + server) write
/// the config concurrently — `rename` is atomic on POSIX filesystems so
/// readers always see either the old or new content, never a partial mix.
pub(super) fn atomic_write(path: &Path, content: impl AsRef<[u8]>) -> std::io::Result<()> {
    use std::io::Write;
    let dir = path.parent().unwrap_or(Path::new("."));
    let mut tmp = tempfile::NamedTempFile::new_in(dir)?;
    tmp.write_all(content.as_ref())?;
    tmp.persist(path).map_err(|e| e.error)?;
    restrict_config_file(path)
}

pub(super) fn write_config_file(path: &Path, content: impl AsRef<[u8]>) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, content)?;
    restrict_config_file(path)
}

fn restrict_config_file(path: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    }
    #[cfg(not(unix))]
    {
        let _ = path;
    }
    Ok(())
}

/// Load config from the given path (any supported format).
///
/// After parsing, `CHELIX_*` env vars are applied as overrides.
///
/// Uses a two-pass approach so that `[env]` section values are available
/// for `${VAR}` substitution in other sections of the same config file.
pub fn load_config(path: &Path) -> crate::Result<ChelixConfig> {
    load_config_with_aliases(path, true)
}

fn load_config_with_aliases(
    path: &Path,
    apply_third_party_aliases: bool,
) -> crate::Result<ChelixConfig> {
    let raw = std::fs::read_to_string(path).map_err(|source| {
        crate::Error::external(format!("failed to read {}", path.display()), source)
    })?;
    load_config_source_with_aliases(&raw, path, apply_third_party_aliases, std::env::vars())
}

fn load_config_source_with_aliases<I>(
    raw: &str,
    path: &Path,
    apply_third_party_aliases: bool,
    env_vars: I,
) -> crate::Result<ChelixConfig>
where
    I: IntoIterator<Item = (String, String)>,
{
    let mut value = substituted_config_value(raw, path, &|name| std::env::var(name).ok())?;
    let preliminary = config_from_value(value.clone(), path)?;
    if !preliminary.env.is_empty() {
        let env = preliminary.env.clone();
        value = substituted_config_value(raw, path, &|name| {
            std::env::var(name).ok().or_else(|| env.get(name).cloned())
        })?;
    }
    let config = config_from_value(value, path)?;
    apply_env_overrides_with_options(config, env_vars.into_iter(), apply_third_party_aliases)
}

fn substituted_config_value(
    raw: &str,
    path: &Path,
    lookup: &impl Fn(&str) -> Option<String>,
) -> crate::Result<serde_json::Value> {
    let mut value = parse_config_value(raw, path)?;
    crate::llm_assignment::substitute_json_strings(&mut value, lookup)?;
    Ok(value)
}

/// Load and parse the config file with env substitution and includes.
pub fn load_config_value(path: &Path) -> crate::Result<serde_json::Value> {
    let raw = std::fs::read_to_string(path).map_err(|source| {
        crate::Error::external(format!("failed to read {}", path.display()), source)
    })?;
    substituted_config_value(&raw, path, &|name| std::env::var(name).ok())
}

/// Discover and load config from standard locations.
///
/// One-time config initialization — call once at process startup.
///
/// Performs all write side-effects that prepare the config directory:
/// - Writes `chelix.toml` on first run
/// - Persists a randomly generated port so it stays stable
///
/// After this, use [`discover_and_load`] (read-only) to load config.
pub fn initialize_config() -> crate::Result<ChelixConfig> {
    // Write the config on first run (when no config file exists).
    if find_config_file().is_none() {
        let default_path = find_or_default_config_path();
        debug!(
            path = %default_path.display(),
            "no config file found, writing default config with random port"
        );
        let mut config = ChelixConfig::default();
        config.server.port = generate_random_port();
        config.tools.execute_command.terminal_size = Some(crate::schema::TerminalSizeConfig {
            cols: 115,
            rows: 58,
        });
        config.sandbox.archived_session_retention_days = Some(7);
        write_default_config(&default_path, &config)?;
        info!(
            path = %default_path.display(),
            "wrote default config"
        );
    }

    let cfg = try_discover_and_load_readonly_with_options(true)?;

    // Persist randomly generated port so it stays stable across restarts.
    // Read-only discovery generates an in-memory port when the on-disk
    // value is 0 — write it back so the port is stable across restarts.
    let path = find_config_file()
        .ok_or_else(|| crate::Error::message("config file disappeared during initialization"))?;
    if cfg.server.port != 0 {
        let raw = std::fs::read_to_string(&path).map_err(|source| {
            crate::Error::external(format!("failed to read {}", path.display()), source)
        })?;
        let on_disk = parse_config(&raw, &path)?;
        if on_disk.server.port != 0 {
            return Ok(cfg);
        }
        debug!(
            port = cfg.server.port,
            "persisting generated port to config"
        );
        save_user_config_to_path(&path, &cfg)?;
    }

    Ok(cfg)
}

/// Discover and load config from disk (read-only, no side-effects).
///
/// This is the primary config loading function. Call [`initialize_config`]
/// once at process startup to prepare the config directory, then use this
/// function everywhere else.
///
/// User config search order:
/// 1. `./chelix.{toml,yaml,yml,json}` (project-local)
/// 2. `~/.config/chelix/chelix.{toml,yaml,yml,json}` (user-global)
///
/// Returns an error if no config file is found or the file is invalid.
pub fn discover_and_load() -> crate::Result<ChelixConfig> {
    try_discover_and_load_readonly_with_options(true)
}

fn try_discover_and_load_readonly_with_options(
    apply_third_party_aliases: bool,
) -> crate::Result<ChelixConfig> {
    let path = find_config_file().ok_or_else(|| crate::Error::message("no config file found"))?;
    debug!(path = %path.display(), "loading config (read-only)");
    let mut cfg = load_layered_config(&path, apply_third_party_aliases)?;
    if cfg.server.port == 0 {
        cfg.server.port = generate_random_port();
    }
    Ok(cfg)
}

/// Load config from the user file plus env overrides.
fn load_layered_config(
    user_path: &Path,
    apply_third_party_aliases: bool,
) -> crate::Result<ChelixConfig> {
    let is_toml = user_path
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|ext| ext.eq_ignore_ascii_case("toml"));

    if is_toml {
        load_layered_config_toml(user_path, apply_third_party_aliases)
    } else {
        load_config_with_aliases(user_path, apply_third_party_aliases)
    }
}

/// TOML-specific layered loading with deep document merge.
fn load_layered_config_toml(
    user_path: &Path,
    apply_third_party_aliases: bool,
) -> crate::Result<ChelixConfig> {
    let user_raw = std::fs::read_to_string(user_path).map_err(|source| {
        crate::Error::external(format!("failed to read {}", user_path.display()), source)
    })?;
    load_layered_config_toml_source(
        &user_raw,
        user_path,
        apply_third_party_aliases,
        std::env::vars(),
    )
}

pub(super) fn load_layered_config_toml_source<I>(
    user_raw: &str,
    user_path: &Path,
    apply_third_party_aliases: bool,
    env_vars: I,
) -> crate::Result<ChelixConfig>
where
    I: IntoIterator<Item = (String, String)>,
{
    let mut value =
        substituted_config_value(user_raw, user_path, &|name| std::env::var(name).ok())?;
    let preliminary = config_from_value(value.clone(), user_path)?;
    if !preliminary.env.is_empty() {
        let env = preliminary.env.clone();
        value = substituted_config_value(user_raw, user_path, &|name| {
            std::env::var(name).ok().or_else(|| env.get(name).cloned())
        })?;
    }
    let config = config_from_value(value, user_path)?;
    apply_env_overrides_with_options(config, env_vars.into_iter(), apply_third_party_aliases)
}

/// Load an in-memory candidate through the same layered pipeline as the next startup.
///
/// The logical path is the raw-save target because its extension selects the startup parser.
pub fn load_layered_config_candidate(user_raw: &str) -> crate::Result<ChelixConfig> {
    let user_path = find_or_default_config_path();
    let is_toml = user_path
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("toml"));

    if is_toml {
        load_layered_config_toml_source(user_raw, &user_path, true, std::env::vars())
    } else {
        load_config_source_with_aliases(user_raw, &user_path, true, std::env::vars())
    }
}

fn find_config_file_in(dir: &Path) -> Option<PathBuf> {
    CONFIG_FILENAMES
        .iter()
        .map(|name| dir.join(name))
        .find(|path| path.exists())
}

pub(super) fn find_config_file_from_dirs(
    explicit_dir: Option<&Path>,
    project_dir: &Path,
    default_dir: Option<&Path>,
) -> Option<PathBuf> {
    if let Some(dir) = explicit_dir {
        return find_config_file_in(dir);
    }

    find_config_file_in(project_dir).or_else(|| default_dir.and_then(find_config_file_in))
}

/// Find the first config file in standard locations.
///
/// When a programmatic or `CHELIX_CONFIG_DIR` override is set, only that
/// directory is searched. Project-local and user-global paths are skipped.
pub fn find_config_file() -> Option<PathBuf> {
    let explicit_dir = explicit_config_dir();
    let default_dir = default_config_dir();
    find_config_file_from_dirs(
        explicit_dir.as_deref(),
        Path::new("."),
        default_dir.as_deref(),
    )
}

pub fn find_or_default_config_path() -> PathBuf {
    if let Some(path) = find_config_file() {
        return path;
    }
    config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("chelix.toml")
}

/// Lock guarding config read-modify-write cycles.
pub(super) struct ConfigSaveState {
    pub(super) target_path: Option<PathBuf>,
}

/// Lock guarding config read-modify-write cycles and the target config path
/// being synchronized.
pub(super) static CONFIG_SAVE_LOCK: Mutex<ConfigSaveState> =
    Mutex::new(ConfigSaveState { target_path: None });

/// Atomically load the current config, apply `f`, and save only the user
/// override file.
///
/// The closure receives the **effective** (merged) config for reading, but
/// only the fields that differ from defaults are written back to the user
/// file.  This prevents built-in defaults from being materialized into the
/// user config.
///
/// Acquires a process-wide lock so concurrent callers cannot race.
/// Returns the path written to.
pub fn update_config(f: impl FnOnce(&mut ChelixConfig)) -> crate::Result<PathBuf> {
    let mut guard = CONFIG_SAVE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let target_path = find_or_default_config_path();
    guard.target_path = Some(target_path.clone());
    let mut config = try_discover_and_load_readonly_with_options(false)?;
    f(&mut config);
    save_user_config_to_path(&target_path, &config)
}

/// Atomically load, mutate, validate, and save the user configuration.
///
/// The mutation may reject the update before any file is written. The fully
/// mutated configuration is then validated before the existing user config is
/// replaced.
pub fn update_config_checked(
    f: impl FnOnce(&mut ChelixConfig) -> crate::Result<()>,
) -> crate::Result<PathBuf> {
    let mut guard = CONFIG_SAVE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let target_path = find_or_default_config_path();
    guard.target_path = Some(target_path.clone());
    let mut config = try_discover_and_load_readonly_with_options(false)?;
    f(&mut config)?;
    validate_agent_ids(&config.agents, "configuration update")?;
    validate_sandbox_archived_session_retention(&config, "configuration update")?;

    let serialized = toml::to_string_pretty(&config)
        .map_err(|source| crate::Error::external("serialize config", source))?;
    let validation = crate::validate::validate_toml_str(&serialized);
    let errors = validation
        .diagnostics
        .into_iter()
        .filter(|diagnostic| diagnostic.severity == crate::validate::Severity::Error)
        .map(|diagnostic| format!("{}: {}", diagnostic.path, diagnostic.message))
        .collect::<Vec<_>>();
    if !errors.is_empty() {
        return Err(crate::Error::message(format!(
            "configuration update rejected: {}",
            errors.join("; ")
        )));
    }

    save_user_config_to_path(&target_path, &config)
}

/// Edit one provider model table in the user TOML file.
///
/// Existing model items are moved unchanged. Only the inserted or replaced
/// record is serialized. The callback runs before the file is written and
/// must stay synchronous.
pub fn update_provider_model_toml(
    provider: &str,
    previous_id: Option<&str>,
    model_id: &str,
    metadata: Option<&crate::schema::PartialModelMetadata>,
    remove_priority_id: Option<&str>,
    validate: impl FnOnce(&crate::schema::ModelConfigMap) -> crate::Result<()>,
) -> crate::Result<crate::schema::ModelConfigMap> {
    let path = find_or_default_config_path();
    let is_toml = path
        .extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| ext.eq_ignore_ascii_case("toml"));
    if !is_toml {
        return Err(crate::Error::message(
            "model records can only be edited in a TOML config",
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
    let models = edit_provider_models_document(
        document.as_table_mut(),
        provider,
        previous_id,
        model_id,
        metadata,
        remove_priority_id,
        validate,
    )?;
    write_config_file(&path, document.to_string()).map_err(|source| {
        crate::Error::external(format!("failed to write {}", path.display()), source)
    })?;
    Ok(models)
}

fn edit_provider_models_document(
    root: &mut toml_edit::Table,
    provider: &str,
    previous_id: Option<&str>,
    model_id: &str,
    metadata: Option<&crate::schema::PartialModelMetadata>,
    remove_priority_id: Option<&str>,
    validate: impl FnOnce(&crate::schema::ModelConfigMap) -> crate::Result<()>,
) -> crate::Result<crate::schema::ModelConfigMap> {
    let providers = nested_table(root, "providers", true)?;
    let provider_table = nested_table(providers, provider, false)?;
    let (mut models, mut items) = read_model_items(provider, provider_table.get("models"))?;
    apply_model_map_edit(&mut models, &mut items, previous_id, model_id, metadata)?;
    validate(&models)?;
    if models.is_empty() {
        provider_table.remove("models");
    } else {
        let mut models_table = toml_edit::Table::new();
        for (id, _) in &models {
            let Some(item) = items.remove(id) else {
                return Err(crate::Error::message(format!(
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
) -> crate::Result<(
    crate::schema::ModelConfigMap,
    std::collections::HashMap<String, toml_edit::Item>,
)> {
    let mut models = crate::schema::ModelConfigMap::new();
    let mut items = std::collections::HashMap::new();
    let Some(models_item) = models_item else {
        return Ok((models, items));
    };
    let Some(table) = models_item.as_table() else {
        return Err(crate::Error::message(format!(
            "providers.{provider}.models is not a TOML table"
        )));
    };
    for (key, item) in table.iter() {
        let model_table = item
            .as_table()
            .ok_or_else(|| crate::Error::message(format!("model `{key}` is not a TOML table")))?;
        let metadata =
            toml::from_str::<crate::schema::PartialModelMetadata>(&model_table.to_string())
                .map_err(|source| crate::Error::external(format!("parse model `{key}`"), source))?;
        models.insert(key.to_string(), metadata);
        items.insert(key.to_string(), item.clone());
    }
    Ok((models, items))
}

fn apply_model_map_edit(
    models: &mut crate::schema::ModelConfigMap,
    items: &mut std::collections::HashMap<String, toml_edit::Item>,
    previous_id: Option<&str>,
    model_id: &str,
    metadata: Option<&crate::schema::PartialModelMetadata>,
) -> crate::Result<()> {
    match metadata {
        None => {
            if models.shift_remove(model_id).is_none() {
                return Err(crate::Error::message(format!(
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
                        return Err(crate::Error::message(format!(
                            "model `{previous}` is not configured"
                        )));
                    };
                    if models.contains_key(model_id) {
                        return Err(crate::Error::message(format!(
                            "model `{model_id}` is already configured"
                        )));
                    }
                    models.shift_remove(previous);
                    models.shift_insert(index, model_id.to_string(), metadata.clone());
                    items.remove(previous);
                },
                Some(_) => {
                    if !models.contains_key(model_id) {
                        return Err(crate::Error::message(format!(
                            "model `{model_id}` is not configured"
                        )));
                    }
                    models.insert(model_id.to_string(), metadata.clone());
                },
                None => {
                    if models.contains_key(model_id) {
                        return Err(crate::Error::message(format!(
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
    metadata: &crate::schema::PartialModelMetadata,
    items: &std::collections::HashMap<String, toml_edit::Item>,
) -> crate::Result<toml_edit::Item> {
    let text = toml::to_string(metadata)
        .map_err(|source| crate::Error::external("serialize model metadata", source))?;
    let document = text
        .parse::<toml_edit::DocumentMut>()
        .map_err(|source| crate::Error::external("parse model metadata", source))?;
    let source_key = previous_id.unwrap_or(model_id);
    let mut item = items
        .get(source_key)
        .cloned()
        .unwrap_or_else(|| toml_edit::Item::Table(toml_edit::Table::new()));
    let table = item.as_table_mut().ok_or_else(|| {
        crate::Error::message(format!("model `{source_key}` is not a TOML table"))
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

pub(super) fn nested_table<'a>(
    parent: &'a mut toml_edit::Table,
    key: &str,
    implicit: bool,
) -> crate::Result<&'a mut toml_edit::Table> {
    if !parent.contains_key(key) {
        let mut table = toml_edit::Table::new();
        table.set_implicit(implicit);
        parent.insert(key, toml_edit::Item::Table(table));
    }
    parent
        .get_mut(key)
        .and_then(toml_edit::Item::as_table_mut)
        .ok_or_else(|| crate::Error::message(format!("`{key}` is not a TOML table")))
}

/// Serialize `config` to TOML and write it to the user-global config path.
///
/// Only writes the user override layer (fields that differ from defaults
/// are preserved; built-in defaults are not materialized).
///
/// Creates parent directories if needed. Returns the path written to.
///
/// Prefer [`update_config`] for read-modify-write cycles to avoid races.
pub fn save_config(config: &ChelixConfig) -> crate::Result<PathBuf> {
    let mut guard = CONFIG_SAVE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let target_path = find_or_default_config_path();
    guard.target_path = Some(target_path.clone());
    save_user_config_to_path(&target_path, config)
}

/// Write raw TOML to the config file, preserving comments.
///
/// Validates the input by parsing it first. Acquires the config save lock
/// so concurrent callers cannot race.  Returns the path written to.
pub fn save_raw_config(toml_str: &str) -> crate::Result<PathBuf> {
    let _ = parse_config(toml_str, Path::new("chelix.toml"))?;
    let mut guard = CONFIG_SAVE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let path = find_or_default_config_path();
    guard.target_path = Some(path.clone());
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    atomic_write(&path, toml_str)?;
    debug!(path = %path.display(), "saved raw config");
    Ok(path)
}

/// Serialize `config` to TOML and write it to the provided path.
///
/// For existing TOML files, this preserves user comments by merging the new
/// serialized values into the current document structure before writing.
pub fn save_config_to_path(path: &Path, config: &ChelixConfig) -> crate::Result<PathBuf> {
    validate_agent_ids(&config.agents, "configuration write")?;
    validate_sandbox_archived_session_retention(config, "configuration write")?;
    let mut guard = CONFIG_SAVE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    guard.target_path = Some(path.to_path_buf());
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let toml_str = toml::to_string_pretty(config)
        .map_err(|source| crate::Error::external("serialize config", source))?;

    let is_toml_path = path
        .extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| ext.eq_ignore_ascii_case("toml"));

    if is_toml_path && path.exists() {
        merge_toml_preserving_comments(path, &toml_str)?;
    } else {
        atomic_write(path, toml_str)?;
    }

    debug!(path = %path.display(), "saved config");
    Ok(path.to_path_buf())
}

/// Serialize `config` and write every persisted setting into `chelix.toml`.
pub fn save_user_config_to_path(path: &Path, config: &ChelixConfig) -> crate::Result<PathBuf> {
    validate_agent_ids(&config.agents, "user configuration write")?;
    validate_sandbox_archived_session_retention(config, "user configuration write")?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let effective_toml = toml::to_string_pretty(config)
        .map_err(|source| crate::Error::external("serialize config", source))?;
    let is_toml_path = path
        .extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| ext.eq_ignore_ascii_case("toml"));

    if is_toml_path && path.exists() {
        merge_toml_preserving_comments(path, &effective_toml)?;
    } else {
        write_config_file(path, effective_toml).map_err(|source| {
            crate::Error::external(format!("failed to write {}", path.display()), source)
        })?;
    }

    debug!(path = %path.display(), "saved config");
    Ok(path.to_path_buf())
}

fn merge_toml_preserving_comments(path: &Path, updated_toml: &str) -> crate::Result<()> {
    let current_toml = std::fs::read_to_string(path)?;
    let mut current_doc = current_toml
        .parse::<toml_edit::DocumentMut>()
        .map_err(|source| crate::Error::external("parse existing TOML", source))?;
    let updated_doc = updated_toml
        .parse::<toml_edit::DocumentMut>()
        .map_err(|source| crate::Error::external("parse updated TOML", source))?;

    merge_toml_tables(current_doc.as_table_mut(), updated_doc.as_table());
    atomic_write(path, current_doc.to_string())?;
    Ok(())
}

pub(super) fn merge_toml_tables(current: &mut toml_edit::Table, updated: &toml_edit::Table) {
    let current_keys: Vec<String> = current.iter().map(|(key, _)| key.to_string()).collect();
    for key in current_keys {
        if !updated.contains_key(&key) {
            let _ = current.remove(&key);
        }
    }

    for (key, updated_item) in updated.iter() {
        if let Some(current_item) = current.get_mut(key) {
            merge_toml_items(current_item, updated_item);
        } else {
            // Clone the item and strip `doc_position` metadata inherited from
            // the source document.  Without this, toml_edit uses the position
            // from the *serialized* document, causing new sub-tables to be
            // interleaved among existing sections instead of appearing after
            // their parent (GH-684).
            current.insert(key, clone_item_without_positions(updated_item));
        }
    }
}

/// Deep-clone a `toml_edit::Item`, stripping `doc_position` from every table
/// so that newly inserted entries get auto-positioned by `toml_edit` rather
/// than inheriting stale positions from a different document.
fn clone_item_without_positions(item: &toml_edit::Item) -> toml_edit::Item {
    match item {
        toml_edit::Item::Table(t) => toml_edit::Item::Table(clone_table_without_positions(t)),
        toml_edit::Item::ArrayOfTables(arr) => {
            let mut new_arr = toml_edit::ArrayOfTables::new();
            for table in arr.iter() {
                new_arr.push(clone_table_without_positions(table));
            }
            toml_edit::Item::ArrayOfTables(new_arr)
        },
        other => other.clone(),
    }
}

/// Clone a table, recursively stripping `doc_position` so new tables get
/// auto-positioned when inserted into a different document.
fn clone_table_without_positions(src: &toml_edit::Table) -> toml_edit::Table {
    let mut dst = toml_edit::Table::new();
    // doc_position is None for manually created tables → auto-positioned
    dst.set_implicit(src.is_implicit());
    dst.set_dotted(src.is_dotted());
    *dst.decor_mut() = src.decor().clone();
    for (key, item) in src.iter() {
        dst.insert(key, clone_item_without_positions(item));
        // Preserve key decorations (whitespace/comments around the key)
        if let (Some(src_key), Some(mut dst_key)) = (src.key(key), dst.key_mut(key)) {
            *dst_key.leaf_decor_mut() = src_key.leaf_decor().clone();
            *dst_key.dotted_decor_mut() = src_key.dotted_decor().clone();
        }
    }
    dst
}

fn merge_toml_items(current: &mut toml_edit::Item, updated: &toml_edit::Item) {
    match (current, updated) {
        (toml_edit::Item::Table(current_table), toml_edit::Item::Table(updated_table)) => {
            merge_toml_tables(current_table, updated_table);
        },
        (toml_edit::Item::Value(current_value), toml_edit::Item::Value(updated_value)) => {
            let existing_decor = current_value.decor().clone();
            *current_value = updated_value.clone();
            *current_value.decor_mut() = existing_decor;
        },
        (current_item, updated_item) => {
            *current_item = updated_item.clone();
        },
    }
}

/// Write the default config file to the user-global config path.
/// Only called when no config file exists yet.
pub(super) fn write_default_config(path: &Path, config: &ChelixConfig) -> crate::Result<()> {
    if path.exists() {
        return Ok(());
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let toml_str = toml::to_string_pretty(config)
        .map_err(|source| crate::Error::external("serialize config", source))?;
    atomic_write(path, &toml_str)?;
    debug!(path = %path.display(), "wrote default config file");
    Ok(())
}

/// Apply `CHELIX_*` environment variable overrides to a loaded config.
///
/// Maps env vars to config fields using `__` as a section separator and
/// lowercasing. For example:
/// - `CHELIX_AUTH__DISABLED=true` → `auth.disabled = true`
/// - `CHELIX_TOOLS__EXECUTE_COMMAND__DEFAULT_TIMEOUT_SECS=60` → `tools.execute_command.default_timeout_secs = 60`
/// - `CHELIX_CHAT__AUTO_TITLE=false` → `chat.auto_title = false`
///
/// The config is serialized to a JSON value, env overrides are merged in,
/// then deserialized back. Only env vars with the `CHELIX_` prefix are
/// considered. `CHELIX_CONFIG_DIR`, `CHELIX_DATA_DIR`, `CHELIX_SHARE_DIR`,
/// `CHELIX_TOKEN`, `CHELIX_PASSWORD`, `CHELIX_TAILSCALE`,
/// `CHELIX_WEBAUTHN_RP_ID`, and `CHELIX_WEBAUTHN_ORIGIN` are excluded
/// (they are handled separately).
pub fn apply_env_overrides(config: ChelixConfig) -> crate::Result<ChelixConfig> {
    apply_env_overrides_with_options(config, std::env::vars(), true)
}

/// Apply env overrides from an arbitrary iterator of (key, value) pairs.
/// Exposed for testing without mutating the process environment.
#[cfg(test)]
pub(super) fn apply_env_overrides_with(
    config: ChelixConfig,
    vars: impl Iterator<Item = (String, String)>,
) -> ChelixConfig {
    apply_env_overrides_with_options(config, vars, true)
        .unwrap_or_else(|error| panic!("test env override must be valid: {error}"))
}

const ENV_OVERRIDE_EXCLUDED: &[&str] = &[
    "CHELIX_CONFIG_DIR",
    "CHELIX_DATA_DIR",
    "CHELIX_SHARE_DIR",
    "CHELIX_TOKEN",
    "CHELIX_PASSWORD",
    "CHELIX_TAILSCALE",
    "CHELIX_WEBAUTHN_RP_ID",
    "CHELIX_WEBAUTHN_ORIGIN",
    "CHELIX_EXTERNAL_URL",
];

fn env_override_path(key: &str) -> Option<Vec<String>> {
    if !key.starts_with("CHELIX_") || ENV_OVERRIDE_EXCLUDED.contains(&key) || !key.contains("__") {
        return None;
    }
    let path_parts: Vec<String> = key["CHELIX_".len()..]
        .split("__")
        .map(|segment| match segment.to_ascii_lowercase().as_str() {
            "exec" => "execute_command".to_string(),
            normalized => normalized.to_string(),
        })
        .collect();
    if path_parts.is_empty() {
        None
    } else {
        Some(path_parts)
    }
}

pub(super) fn apply_env_overrides_with_options(
    config: ChelixConfig,
    vars: impl Iterator<Item = (String, String)>,
    apply_third_party_aliases: bool,
) -> crate::Result<ChelixConfig> {
    use serde_json::Value;

    let mut root: Value = serde_json::to_value(config).map_err(|source| {
        crate::Error::external("failed to serialize config for env override", source)
    })?;

    // Third-party env var aliases.
    // Intentionally empty for now.
    const ENV_ALIASES: &[(&str, &[&str])] = &[];

    for (key, val) in vars {
        // Check third-party aliases first (before the CHELIX_ prefix check).
        let mut matched_alias = false;
        if apply_third_party_aliases {
            for &(alias_key, path) in ENV_ALIASES {
                if key == alias_key {
                    let path_parts: Vec<String> = path.iter().map(|s| s.to_string()).collect();
                    // Only apply if the field is currently null/empty.
                    let current = get_nested(&root, &path_parts);
                    if current.is_none()
                        || current == Some(&Value::Null)
                        || current.and_then(|v| v.as_str()).unwrap_or("x").is_empty()
                    {
                        set_nested(&mut root, &path_parts, parse_env_value(&val));
                    }
                    matched_alias = true;
                    break;
                }
            }
        }
        if matched_alias {
            continue;
        }

        // CHELIX_AUTH__DISABLED → ["auth", "disabled"]
        let Some(path_parts) = env_override_path(&key) else {
            continue;
        };
        let parsed_val = parse_env_value(&val);
        let rejects_assignment =
            crate::llm_assignment::subtree_assigns_llm(&path_parts, &parsed_val)
                || get_nested(&root, &path_parts).is_some_and(|existing| {
                    crate::llm_assignment::subtree_assigns_llm(&path_parts, existing)
                });
        if rejects_assignment {
            return Err(crate::Error::message(format!(
                "environment variable {key} cannot assign an LLM provider, model, or token"
            )));
        }
        set_nested(&mut root, &path_parts, parsed_val);
    }

    validate_config_shape(&root, "environment overrides")?;
    let config: ChelixConfig = serde_json::from_value(root)
        .map_err(|source| crate::Error::external("failed to apply env overrides", source))?;
    validate_provider_names(&config.providers, "environment overrides")?;
    validate_context7_request_timeout(&config, "environment overrides")?;
    validate_linkup_request_timeout(&config, "environment overrides")?;
    validate_search_request_timeouts(&config, "environment overrides")?;
    validate_sandbox_archived_session_retention(&config, "environment overrides")?;
    Ok(config)
}

/// Re-resolve `${VAR}` placeholders in a loaded config using additional overrides.
///
/// Call this after runtime env vars (e.g. DB-stored UI variables) become
/// available.  Substitution happens at the JSON value level (not textual
/// TOML), so override values that contain quotes or backslashes are safe.
///
/// Lookup precedence: process env → `overrides` map.
pub fn resubstitute_config(
    config: &ChelixConfig,
    overrides: &std::collections::HashMap<String, String>,
) -> crate::Result<ChelixConfig> {
    let mut json = serde_json::to_value(config)
        .map_err(|source| crate::Error::external("serialize config for resubstitution", source))?;
    crate::llm_assignment::substitute_json_strings(&mut json, &|name| {
        std::env::var(name)
            .ok()
            .or_else(|| overrides.get(name).cloned())
    })?;
    let reloaded: ChelixConfig = serde_json::from_value(json).map_err(|source| {
        crate::Error::external("deserialize config after resubstitution", source)
    })?;
    apply_env_overrides(reloaded)
}

/// Parse a string env value into a JSON value, trying bool and number first.
pub(super) fn parse_env_value(val: &str) -> serde_json::Value {
    let trimmed = val.trim();

    // Support JSON arrays/objects for list-like env overrides, e.g. '["a","b"]' or '[]'.
    if ((trimmed.starts_with('[') && trimmed.ends_with(']'))
        || (trimmed.starts_with('{') && trimmed.ends_with('}')))
        && let Ok(parsed) = serde_json::from_str::<serde_json::Value>(trimmed)
    {
        return parsed;
    }

    if val.eq_ignore_ascii_case("true") {
        return serde_json::Value::Bool(true);
    }
    if val.eq_ignore_ascii_case("false") {
        return serde_json::Value::Bool(false);
    }
    if let Ok(n) = val.parse::<i64>() {
        return serde_json::Value::Number(n.into());
    }
    if let Ok(n) = val.parse::<f64>()
        && let Some(n) = serde_json::Number::from_f64(n)
    {
        return serde_json::Value::Number(n);
    }
    serde_json::Value::String(val.to_string())
}

/// Set a value at a nested JSON path, creating intermediate objects as needed.
/// Read a nested value from a JSON tree by path.
fn get_nested<'a>(root: &'a serde_json::Value, path: &[String]) -> Option<&'a serde_json::Value> {
    let mut current = root;
    for key in path {
        current = current.get(key.as_str())?;
    }
    Some(current)
}

pub(super) fn set_nested(root: &mut serde_json::Value, path: &[String], val: serde_json::Value) {
    if path.is_empty() {
        return;
    }
    let mut current = root;
    for (i, key) in path.iter().enumerate() {
        if i == path.len() - 1 {
            if let serde_json::Value::Object(map) = current {
                map.insert(key.clone(), val);
            }
            return;
        }
        if !current.get(key).is_some_and(|v| v.is_object())
            && let serde_json::Value::Object(map) = current
        {
            map.insert(key.clone(), serde_json::Value::Object(Default::default()));
        }
        let Some(next) = current.get_mut(key) else {
            return;
        };
        current = next;
    }
}

pub(super) fn parse_config(raw: &str, path: &Path) -> crate::Result<ChelixConfig> {
    let value = parse_config_value(raw, path)?;
    config_from_value(value, path)
}

fn config_from_value(value: serde_json::Value, path: &Path) -> crate::Result<ChelixConfig> {
    validate_config_shape(&value, &format!("config {}", path.display()))?;
    let config: ChelixConfig = serde_json::from_value(value).map_err(|source| {
        crate::Error::external(format!("invalid config {}", path.display()), source)
    })?;
    let context = format!("config {}", path.display());
    validate_provider_names(&config.providers, &context)?;
    validate_agent_ids(&config.agents, &context)?;
    let invalid_channels = config.channels.invalid_channel_types();
    if !invalid_channels.is_empty() {
        let paths = invalid_channels
            .into_iter()
            .map(|(path, channel_type)| format!("{path}: unknown channel type '{channel_type}'"))
            .collect::<Vec<_>>()
            .join("; ");
        return Err(crate::Error::message(format!("invalid {context}: {paths}")));
    }
    validate_context7_request_timeout(&config, &context)?;
    validate_linkup_request_timeout(&config, &context)?;
    validate_search_request_timeouts(&config, &context)?;
    validate_execute_command_terminal_size(&config, &context)?;
    validate_sandbox_archived_session_retention(&config, &context)?;
    Ok(config)
}

fn validate_sandbox_archived_session_retention(
    config: &ChelixConfig,
    context: &str,
) -> crate::Result<()> {
    if config.sandbox.mode == crate::schema::SandboxMode::On
        && config.sandbox.archived_session_retention_days.is_none()
    {
        return Err(crate::Error::message(format!(
            "invalid {context}: sandbox.archived_session_retention_days is required when sandbox.mode is On"
        )));
    }
    Ok(())
}

fn validate_execute_command_terminal_size(
    config: &ChelixConfig,
    context: &str,
) -> crate::Result<()> {
    if config.tools.execute_command.terminal_size.is_none() {
        return Err(crate::Error::message(format!(
            "invalid {context}: tools.execute_command.terminal_size is required"
        )));
    }
    Ok(())
}

fn validate_context7_request_timeout(config: &ChelixConfig, context: &str) -> crate::Result<()> {
    if config.tools.context7.request_timeout_secs == 0 {
        return Err(crate::Error::message(format!(
            "invalid {context}: tools.context7.request_timeout_secs must be at least 1"
        )));
    }
    Ok(())
}

fn validate_linkup_request_timeout(config: &ChelixConfig, context: &str) -> crate::Result<()> {
    if config.tools.linkup.request_timeout_secs == 0 {
        return Err(crate::Error::message(format!(
            "invalid {context}: tools.linkup.request_timeout_secs must be at least 1"
        )));
    }
    Ok(())
}

fn validate_search_request_timeouts(config: &ChelixConfig, context: &str) -> crate::Result<()> {
    for (name, timeout) in [
        ("exa", config.tools.exa.request_timeout_secs),
        ("google", config.tools.google.request_timeout_secs),
        ("felo", config.tools.felo.request_timeout_secs),
    ] {
        if timeout == 0 {
            return Err(crate::Error::message(format!(
                "invalid {context}: tools.{name}.request_timeout_secs must be at least 1"
            )));
        }
    }
    Ok(())
}

fn validate_agent_ids(agents: &crate::schema::AgentsConfig, context: &str) -> crate::Result<()> {
    let mut invalid = agents
        .entries
        .keys()
        .filter_map(|id| {
            crate::schema::validate_agent_id(id)
                .err()
                .map(|message| format!("agents.{id}: {message}"))
        })
        .collect::<Vec<_>>();
    if invalid.is_empty() {
        return Ok(());
    }
    invalid.sort();
    Err(crate::Error::message(format!(
        "invalid {context}: {}",
        invalid.join("; ")
    )))
}

fn validate_provider_names(
    providers: &crate::schema::ProvidersConfig,
    context: &str,
) -> crate::Result<()> {
    let invalid = providers.invalid_provider_names();
    if invalid.is_empty() {
        return Ok(());
    }
    let paths = invalid
        .into_iter()
        .map(|(path, _)| path)
        .collect::<Vec<_>>();
    Err(crate::Error::message(format!(
        "invalid {context}: unknown provider at {}",
        paths.join(", ")
    )))
}

fn validate_config_shape(value: &serde_json::Value, context: &str) -> crate::Result<()> {
    let schema = crate::validate::schema_map::build_schema_map();
    let mut diagnostics = Vec::new();
    crate::validate::schema_map::check_unknown_fields(value, &schema, "", &mut diagnostics);
    if !diagnostics.is_empty() {
        let details = diagnostics
            .into_iter()
            .map(|diagnostic| format!("{}: {}", diagnostic.path, diagnostic.message))
            .collect::<Vec<_>>()
            .join("; ");
        return Err(crate::Error::message(format!(
            "invalid {context}: {details}"
        )));
    }
    Ok(())
}

fn parse_config_value(raw: &str, path: &Path) -> crate::Result<serde_json::Value> {
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("toml");

    match ext {
        "toml" => {
            let v: toml::Value = toml::from_str(raw)?;
            Ok(serde_json::to_value(v)?)
        },
        "yaml" | "yml" => {
            let v: serde_yaml::Value = serde_yaml::from_str(raw)?;
            Ok(serde_json::to_value(v)?)
        },
        "json" => Ok(serde_json::from_str(raw)?),
        _ => Err(crate::Error::message(format!(
            "unsupported config format: .{ext}"
        ))),
    }
}
