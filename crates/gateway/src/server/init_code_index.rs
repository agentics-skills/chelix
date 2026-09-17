//! Initialize the code index system.
//!
//! When the `code-index-builtin` feature is enabled, creates a
//! [`chelix_code_index::CodeIndex`] backed by the builtin SQLite+FTS5 store
//! (discover, filter, status, peek, and search all work). A failure to open
//! that store is a startup error.
//!
//! When the feature is disabled or `[code_index].enabled = false`, the index
//! runs in config-only mode where search operations return
//! [`BackendUnavailable`](chelix_code_index::Error::BackendUnavailable).

use std::sync::Arc;

use tracing::info;

/// Initialize the code index.
///
/// Reads `[code_index]` from the loaded `ChelixConfig`. Falls back to
/// `CodeIndexConfig::default()` when the section is absent or empty.
pub(crate) async fn init_code_index(
    data_dir: &std::path::Path,
    config: &chelix_config::ChelixConfig,
) -> anyhow::Result<Arc<chelix_code_index::CodeIndex>> {
    // Build CodeIndexConfig from TOML, then overlay data_dir.
    let mut code_index_config = chelix_code_index::CodeIndexConfig::from(&config.code_index);
    // TOML data_dir overrides the default; if not set, use data_dir/code-index.
    if code_index_config.data_dir.is_none() {
        code_index_config.data_dir = Some(data_dir.join("code-index"));
    }

    if !config.code_index.enabled {
        info!("code-index: disabled via [code_index].enabled = false");
        return Ok(Arc::new(chelix_code_index::CodeIndex::config_only(
            code_index_config,
        )));
    }

    #[cfg(feature = "code-index-builtin")]
    {
        let default_index_root = data_dir.join("code-index");
        let index_root = code_index_config
            .data_dir
            .as_deref()
            .unwrap_or(default_index_root.as_path());
        let db_path = index_root.join("index.db");
        let store = chelix_code_index::store_sqlite::SqliteCodeIndexStore::new(&db_path)
            .await
            .map_err(|error| {
                anyhow::anyhow!(
                    "code-index: failed to initialize builtin backend at {}: {error}",
                    db_path.display()
                )
            })?;
        info!(path = %db_path.display(), "code-index: builtin SQLite backend initialized");
        Ok(Arc::new(chelix_code_index::CodeIndex::new_builtin(
            code_index_config,
            Box::new(store),
            None,
        )))
    }

    #[cfg(not(feature = "code-index-builtin"))]
    {
        info!(
            "code-index: initialized in config-only mode \
             (code-index-builtin feature disabled — search unavailable)"
        );
        Ok(Arc::new(chelix_code_index::CodeIndex::config_only(
            code_index_config,
        )))
    }
}
