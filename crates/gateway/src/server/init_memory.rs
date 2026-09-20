use std::{collections::HashMap, path::Path as FsPath, sync::Arc};

#[cfg(feature = "local-embeddings")]
use std::path::PathBuf;

use {anyhow::Result, secrecy::ExposeSecret, tracing::info};

use super::helpers::env_value_with_overrides;

/// Initialize the memory system (embedding providers, sync, watchers).
///
/// Returns `Ok(Some(runtime))` when the memory system is available, `Ok(None)`
/// when the database could not be opened, and `Err` when an explicitly selected
/// local embedding provider cannot start.
pub(crate) async fn init_memory_system(
    config: &chelix_config::ChelixConfig,
    data_dir: &FsPath,
    effective_providers: &chelix_config::schema::ProvidersConfig,
    runtime_env_overrides: &HashMap<String, String>,
    db_pool_max_connections: u32,
) -> Result<Option<chelix_memory::runtime::DynMemoryRuntime>> {
    // Build embedding provider(s) for the fallback chain.
    let mut embedding_providers: Vec<(
        String,
        Box<dyn chelix_memory::embeddings::EmbeddingProvider>,
    )> = Vec::new();

    let mem_cfg = &config.memory;

    if mem_cfg.disable_rag {
        info!("memory: RAG disabled via memory.disable_rag=true, using keyword-only search");
    } else {
        // 1. If user explicitly configured an embedding provider, use it.
        if let Some(provider) = mem_cfg.provider {
            match provider {
                chelix_config::MemoryProvider::Local => {
                    #[cfg(feature = "local-embeddings")]
                    {
                        let cache_dir = mem_cfg.base_url.as_ref().map(PathBuf::from).unwrap_or_else(
                            chelix_memory::embeddings_local::LocalEmbeddingProvider::default_cache_dir,
                        );
                        let hf_token = mem_cfg
                            .huggingface_api_key
                            .as_ref()
                            .map(|key| key.expose_secret().clone())
                            .filter(|token| !token.is_empty())
                            .or_else(|| env_value_with_overrides(runtime_env_overrides, "HF_TOKEN"))
                            .or_else(|| {
                                env_value_with_overrides(
                                    runtime_env_overrides,
                                    "HUGGINGFACE_API_KEY",
                                )
                            });
                        let model_spec =
                            chelix_memory::embeddings_local::LocalEmbeddingProvider::resolve_model(
                                cache_dir.clone(),
                                mem_cfg.model.as_deref(),
                            )?;
                        let provider =
                            chelix_memory::embeddings_local::LocalEmbeddingProvider::new(
                                model_spec, cache_dir, hf_token,
                            )
                            .await
                            .map_err(|error| {
                                anyhow::anyhow!("memory: local embedding sidecar failed: {error}")
                            })?;
                        embedding_providers.push(("local".into(), Box::new(provider)));
                    }
                    #[cfg(not(feature = "local-embeddings"))]
                    {
                        return Err(anyhow::anyhow!(
                            "memory: 'local' embedding provider requires the 'local-embeddings' client feature and the chelix-embedding-service binary"
                        ));
                    }
                },
                chelix_config::MemoryProvider::Custom | chelix_config::MemoryProvider::OpenAi => {
                    let base_url = mem_cfg
                        .base_url
                        .clone()
                        .unwrap_or_else(|| "https://api.openai.com".into());
                    let api_key = mem_cfg
                        .api_key
                        .as_ref()
                        .map(|k| k.expose_secret().clone())
                        .or_else(|| {
                            env_value_with_overrides(runtime_env_overrides, "OPENAI_API_KEY")
                        })
                        .unwrap_or_default();
                    let mut e =
                        chelix_memory::embeddings_openai::OpenAiEmbeddingProvider::new(api_key);
                    if base_url != "https://api.openai.com" {
                        e = e.with_base_url(base_url);
                    }
                    if let Some(ref model) = mem_cfg.model {
                        e = e.with_model(model.clone(), 1536);
                    }
                    let provider_name = match provider {
                        chelix_config::MemoryProvider::Custom => "custom",
                        chelix_config::MemoryProvider::OpenAi => "openai",
                        chelix_config::MemoryProvider::Local => "local",
                    };
                    embedding_providers.push((provider_name.to_owned(), Box::new(e)));
                },
            }
        }

        // 2. Auto-detect remote providers only when memory.provider is unset.
        const EMBEDDING_CANDIDATES: &[(&str, &str, &str)] = &[
            ("openai", "OPENAI_API_KEY", "https://api.openai.com"),
            (
                "openrouter",
                "OPENROUTER_API_KEY",
                "https://openrouter.ai/api/v1",
            ),
        ];

        if mem_cfg.provider.is_none() {
            for (config_name, env_key, default_base) in EMBEDDING_CANDIDATES {
                let key = effective_providers
                    .get(config_name)
                    .and_then(|e| e.api_key.as_ref().map(|k| k.expose_secret().clone()))
                    .or_else(|| env_value_with_overrides(runtime_env_overrides, env_key))
                    .filter(|k| !k.is_empty());
                if let Some(api_key) = key {
                    let base = effective_providers
                        .get(config_name)
                        .and_then(|e| e.base_url.clone())
                        .unwrap_or_else(|| default_base.to_string());
                    let mut e =
                        chelix_memory::embeddings_openai::OpenAiEmbeddingProvider::new(api_key);
                    if base != "https://api.openai.com" {
                        e = e.with_base_url(base);
                    }
                    embedding_providers.push((config_name.to_string(), Box::new(e)));
                }
            }
        }
    }

    // Build the final embedder: fallback chain, single provider, or keyword-only.
    let embedder: Option<Box<dyn chelix_memory::embeddings::EmbeddingProvider>> =
        if mem_cfg.disable_rag {
            None
        } else if embedding_providers.is_empty() {
            info!("memory: no embedding provider found, using keyword-only search");
            None
        } else {
            let names: Vec<&str> = embedding_providers
                .iter()
                .map(|(n, _)| n.as_str())
                .collect();
            if embedding_providers.len() == 1 {
                if let Some((name, provider)) = embedding_providers.into_iter().next() {
                    info!(provider = %name, "memory: using single embedding provider");
                    Some(provider)
                } else {
                    None
                }
            } else {
                info!(providers = ?names, active = names[0], "memory: fallback chain configured");
                Some(Box::new(
                    chelix_memory::embeddings_fallback::FallbackEmbeddingProvider::new(
                        embedding_providers,
                    ),
                ))
            }
        };

    let memory_db_path = data_dir.join("memory.db");
    let memory_pool_result = {
        use {
            sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqliteSynchronous},
            std::str::FromStr,
        };
        let options =
            match SqliteConnectOptions::from_str(&format!("sqlite:{}", memory_db_path.display())) {
                Ok(options) => options,
                Err(error) => {
                    tracing::warn!(
                        path = %memory_db_path.display(),
                        error = %error,
                        "memory: invalid memory database path"
                    );
                    return Ok(None);
                },
            }
            .create_if_missing(true)
            .journal_mode(SqliteJournalMode::Wal)
            .synchronous(SqliteSynchronous::Normal)
            .busy_timeout(std::time::Duration::from_secs(5));
        sqlx::pool::PoolOptions::new()
            .max_connections(db_pool_max_connections)
            .connect_with(options)
            .await
    };
    match memory_pool_result {
        Ok(memory_pool) => {
            if let Err(e) = chelix_memory::schema::run_migrations(&memory_pool).await {
                tracing::warn!("memory migration failed: {e}");
                Ok(None)
            } else {
                Ok(build_memory_runtime(mem_cfg, data_dir, embedder, memory_pool).await)
            }
        },
        Err(e) => {
            tracing::warn!("memory: failed to open memory.db: {e}");
            Ok(None)
        },
    }
}

/// Build the memory runtime, start initial sync, file watchers, and periodic syncs.
async fn build_memory_runtime(
    mem_cfg: &chelix_config::schema::MemoryEmbeddingConfig,
    data_dir: &FsPath,
    embedder: Option<Box<dyn chelix_memory::embeddings::EmbeddingProvider>>,
    memory_pool: sqlx::SqlitePool,
) -> Option<chelix_memory::runtime::DynMemoryRuntime> {
    let data_memory_file = data_dir.join("MEMORY.md");
    let agents_root = data_dir.join("agents");

    if let Err(error) = std::fs::create_dir_all(&agents_root) {
        tracing::warn!(
            path = %agents_root.display(),
            error = %error,
            "memory: failed to create agents directory"
        );
    }

    let memory_runtime_config = chelix_memory::config::MemoryConfig {
        db_path: data_dir.join("memory.db").to_string_lossy().into(),
        data_dir: Some(data_dir.to_path_buf()),
        memory_dirs: vec![data_memory_file, agents_root],
        citations: match mem_cfg.citations {
            chelix_config::MemoryCitationsMode::On => chelix_memory::config::CitationMode::On,
            chelix_config::MemoryCitationsMode::Off => chelix_memory::config::CitationMode::Off,
            chelix_config::MemoryCitationsMode::Auto => chelix_memory::config::CitationMode::Auto,
        },
        llm_reranking: mem_cfg.llm_reranking,
        merge_strategy: match mem_cfg.search_merge_strategy {
            chelix_config::MemorySearchMergeStrategy::Rrf => {
                chelix_memory::config::MergeStrategy::Rrf
            },
            chelix_config::MemorySearchMergeStrategy::Linear => {
                chelix_memory::config::MergeStrategy::Linear
            },
        },
        ..Default::default()
    };

    let store = Box::new(chelix_memory::store_sqlite::SqliteMemoryStore::new(
        memory_pool,
    ));
    let memory_dirs_for_watch = memory_runtime_config.memory_dirs.clone();
    let manager: chelix_memory::runtime::DynMemoryRuntime =
        Arc::new(if let Some(embedder) = embedder {
            chelix_memory::manager::MemoryManager::new(memory_runtime_config, store, embedder)
        } else {
            chelix_memory::manager::MemoryManager::keyword_only(memory_runtime_config, store)
        });

    // Initial sync + periodic re-sync (15min with watcher, 5min without).
    let sync_manager = Arc::clone(&manager);
    tokio::spawn(async move {
        match sync_manager.sync().await {
            Ok(report) => {
                info!(
                    updated = report.files_updated,
                    unchanged = report.files_unchanged,
                    removed = report.files_removed,
                    errors = report.errors,
                    cache_hits = report.cache_hits,
                    cache_misses = report.cache_misses,
                    "memory: initial sync complete"
                );
                match sync_manager.status().await {
                    Ok(status) => info!(
                        files = status.total_files,
                        chunks = status.total_chunks,
                        db_size = %status.db_size_display(),
                        model = %status.embedding_model,
                        "memory: status"
                    ),
                    Err(e) => tracing::warn!("memory: failed to get status: {e}"),
                }
            },
            Err(e) => tracing::warn!("memory: initial sync failed: {e}"),
        }

        // Start file watcher for real-time sync (if feature enabled).
        #[cfg(feature = "file-watcher")]
        {
            let watcher_manager = Arc::clone(&sync_manager);
            let watch_specs = chelix_memory::watcher::build_watch_specs(&memory_dirs_for_watch);
            match chelix_memory::watcher::MemoryFileWatcher::start(watch_specs) {
                Ok(mut watcher) => {
                    info!("memory: file watcher started");
                    tokio::spawn(async move {
                        while let Some(event) = watcher.recv().await {
                            let path = match &event {
                                chelix_memory::watcher::WatchEvent::Created(p)
                                | chelix_memory::watcher::WatchEvent::Modified(p) => {
                                    Some(p.clone())
                                },
                                chelix_memory::watcher::WatchEvent::Removed(p) => {
                                    if let Err(e) = watcher_manager.sync().await {
                                        tracing::warn!(
                                            path = %p.display(),
                                            error = %e,
                                            "memory: watcher sync (removal) failed"
                                        );
                                    }
                                    None
                                },
                            };
                            if let Some(path) = path
                                && let Err(e) = watcher_manager.sync_path(&path).await
                            {
                                tracing::warn!(
                                    path = %path.display(),
                                    error = %e,
                                    "memory: watcher sync_path failed"
                                );
                            }
                        }
                    });
                },
                Err(e) => {
                    tracing::warn!("memory: failed to start file watcher: {e}");
                },
            }
        }

        // Periodic full sync as safety net (longer interval with watcher).
        #[cfg(feature = "file-watcher")]
        let interval_secs = 900; // 15 minutes
        #[cfg(not(feature = "file-watcher"))]
        let interval_secs = 300; // 5 minutes

        #[cfg(not(feature = "file-watcher"))]
        let _ = memory_dirs_for_watch;

        let mut interval = tokio::time::interval(std::time::Duration::from_secs(interval_secs));
        interval.tick().await; // skip first immediate tick
        loop {
            interval.tick().await;
            if let Err(e) = sync_manager.sync().await {
                tracing::warn!("memory: periodic sync failed: {e}");
            }
        }
    });

    info!(
        embeddings = manager.has_embeddings(),
        "memory system initialized"
    );
    Some(manager)
}
