use std::{path::Path as FsPath, sync::Arc};

use {anyhow::Result, chelix_memory::embeddings::EmbeddingProvider, tracing::info};

/// Initialize the memory system (remote embedding provider, sync, watchers).
///
/// Returns `Ok(Some(runtime))` when the memory system is available, `Ok(None)`
/// when the database could not be opened, and `Err` when a configured embedding
/// provider cannot be reached.
pub(crate) async fn init_memory_system(
    config: &chelix_config::ChelixConfig,
    data_dir: &FsPath,
    db_pool_max_connections: u32,
) -> Result<Option<chelix_memory::runtime::DynMemoryRuntime>> {
    let mem_cfg = &config.memory;
    let endpoint = mem_cfg
        .embedding_endpoint()
        .map_err(|error| anyhow::anyhow!("memory: {error}"))?;

    let embedder: Option<Box<dyn EmbeddingProvider>> = if mem_cfg.disable_rag {
        info!("memory: RAG disabled via memory.disable_rag=true, using keyword-only search");
        None
    } else if let Some(endpoint) = endpoint {
        let provider = chelix_memory::embeddings_http::HttpEmbeddingProvider::new(
            endpoint.url,
            endpoint.api_key,
            endpoint.dimensions,
        )
        .map_err(|error| anyhow::anyhow!("memory: embedding provider failed: {error}"))?;
        provider
            .embed("probe")
            .await
            .map_err(|error| anyhow::anyhow!("memory: embedding provider probe failed: {error}"))?;
        info!("memory: using remote embedding provider");
        Some(Box::new(provider))
    } else {
        info!("memory: no embedding provider found, using keyword-only search");
        None
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
    embedder: Option<Box<dyn EmbeddingProvider>>,
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
                        url = %status.embedding_url,
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
