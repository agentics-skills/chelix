/// Memory manager: orchestrates file sync, chunking, embedding, and search.
use std::{collections::HashMap, path::Path, sync::Arc};

use {
    async_trait::async_trait,
    sha2::{Digest, Sha256},
    tracing::{debug, info, warn},
};

use chelix_agents::memory_writer::{MemoryWriteResult, MemoryWriter};

use crate::{
    chunker::chunk_content,
    config::MemoryConfig,
    embed_payload::split_oversize_embed_chunks,
    embeddings::EmbeddingProvider,
    error::Result,
    schema::{ChunkRow, FileRow},
    search::{self, SearchResult},
    store::{CacheEntry, MemoryStore},
    writer::validate_memory_path,
};

pub struct MemoryManager {
    config: MemoryConfig,
    store: Box<dyn MemoryStore>,
    embedder: Option<Box<dyn EmbeddingProvider>>,
    path_epochs: tokio::sync::Mutex<HashMap<String, Arc<tokio::sync::Mutex<u64>>>>,
}

/// Status info about the memory system.
#[derive(Debug, Clone)]
pub struct MemoryStatus {
    pub total_files: usize,
    pub total_chunks: usize,
    pub embedding_model: String,
    /// SQLite database file size in bytes (0 for in-memory DBs).
    pub db_size_bytes: u64,
}

impl MemoryStatus {
    /// Human-readable database size (e.g. "12.3 MB").
    pub fn db_size_display(&self) -> String {
        format_bytes(self.db_size_bytes)
    }
}

fn format_bytes(bytes: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = 1024 * KB;
    const GB: u64 = 1024 * MB;
    match bytes {
        b if b >= GB => format!("{:.1} GB", b as f64 / GB as f64),
        b if b >= MB => format!("{:.1} MB", b as f64 / MB as f64),
        b if b >= KB => format!("{:.1} KB", b as f64 / KB as f64),
        b => format!("{b} B"),
    }
}

impl MemoryManager {
    /// Create a memory manager with an embedding provider for hybrid (vector + keyword) search.
    pub fn new(
        config: MemoryConfig,
        store: Box<dyn MemoryStore>,
        embedder: Box<dyn EmbeddingProvider>,
    ) -> Self {
        Self {
            config,
            store,
            embedder: Some(embedder),
            path_epochs: tokio::sync::Mutex::new(HashMap::new()),
        }
    }

    /// Create a memory manager without embeddings. Keyword (FTS) search only.
    pub fn keyword_only(config: MemoryConfig, store: Box<dyn MemoryStore>) -> Self {
        Self {
            config,
            store,
            embedder: None,
            path_epochs: tokio::sync::Mutex::new(HashMap::new()),
        }
    }

    /// Whether this manager has an embedding provider for vector search.
    pub fn has_embeddings(&self) -> bool {
        self.embedder.is_some()
    }

    /// Get the citation mode for this manager.
    pub fn citation_mode(&self) -> crate::config::CitationMode {
        self.config.citations
    }

    /// Root data directory used for memory writes, when configured.
    pub fn data_dir(&self) -> Option<&Path> {
        self.config.data_dir.as_deref()
    }

    /// Whether LLM reranking is enabled.
    pub fn llm_reranking_enabled(&self) -> bool {
        self.config.llm_reranking
    }

    /// Synchronize allowlisted files, detect changes, re-chunk and re-embed.
    pub async fn sync(&self) -> Result<SyncReport> {
        let mut report = SyncReport::default();
        let Some(data_dir) = self.config.data_dir.as_ref() else {
            return Ok(report);
        };

        let discovered = match crate::allowlist::discover_indexable_memory_files(data_dir) {
            Ok(paths) => paths,
            Err(error) => {
                warn!(error = %error, "memory: discovery failed");
                return Err(error.into());
            },
        };
        let discovered_paths: Vec<String> = discovered
            .iter()
            .map(|path| path.to_string_lossy().into_owned())
            .collect();

        for (path, path_str) in discovered.iter().zip(discovered_paths.iter()) {
            match self.sync_file(path, path_str, &mut report).await {
                Ok(changed) => {
                    if changed {
                        report.files_updated += 1;
                    } else {
                        report.files_unchanged += 1;
                    }
                },
                Err(e) => {
                    warn!(path = %path_str, error = %e, "failed to sync file");
                    report.errors += 1;
                },
            }
        }

        let existing_files = self.store.list_files().await?;
        let discovered_set: std::collections::HashSet<&str> =
            discovered_paths.iter().map(|s| s.as_str()).collect();
        let mut removed_stale = 0usize;
        for file in existing_files {
            if discovered_set.contains(file.path.as_str()) {
                continue;
            }
            debug!(path = %file.path, "removing stale file from memory index");
            self.store.delete_chunks_for_file(&file.path).await?;
            self.store.delete_file(&file.path).await?;
            report.files_removed += 1;
            removed_stale += 1;
        }
        if removed_stale > 0 {
            info!(
                removed = removed_stale,
                "memory: removed stale indexed files"
            );
        }

        // LRU eviction on embedding cache
        let cache_count = self.store.count_cached_embeddings().await.unwrap_or(0);
        if cache_count > CACHE_MAX_ROWS {
            let evicted = self
                .store
                .evict_embedding_cache(CACHE_MAX_ROWS)
                .await
                .unwrap_or(0);
            if evicted > 0 {
                info!(evicted, "embedding cache: evicted old entries");
            }
        }

        Ok(report)
    }

    /// Sync a single file by path. Returns true if it was updated.
    pub async fn sync_path(&self, path: &Path) -> Result<bool> {
        let Some(data_dir) = self.config.data_dir.as_ref() else {
            return Ok(false);
        };
        let path = crate::allowlist::absolutize(path);
        if !crate::allowlist::is_indexable_memory_path(data_dir, &path) {
            self.remove_path(&path).await?;
            return Ok(false);
        }
        let path_str = path.to_string_lossy().to_string();
        let mut report = SyncReport::default();
        self.sync_file(&path, &path_str, &mut report).await
    }

    async fn path_epoch(&self, path: &str) -> Arc<tokio::sync::Mutex<u64>> {
        let mut epochs = self.path_epochs.lock().await;
        epochs
            .entry(path.to_string())
            .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(0)))
            .clone()
    }

    async fn bump_path_epoch(&self, path: &str) {
        let epoch = self.path_epoch(path).await;
        let mut epoch_guard = epoch.lock().await;
        *epoch_guard = epoch_guard.saturating_add(1);
    }

    /// Remove a file path from the memory index after the backing file is gone.
    pub async fn remove_path(&self, path: &Path) -> Result<bool> {
        let path = crate::allowlist::absolutize(path);
        let path_str = path.to_string_lossy().to_string();
        let epoch = self.path_epoch(&path_str).await;
        let mut epoch_guard = epoch.lock().await;
        *epoch_guard = epoch_guard.saturating_add(1);
        let had_file = self.store.get_file(&path_str).await?.is_some();
        let had_chunks = !self.store.get_chunks_for_file(&path_str).await?.is_empty();
        self.store.delete_chunks_for_file(&path_str).await?;
        self.store.delete_file(&path_str).await?;
        drop(epoch_guard);
        if had_file || had_chunks {
            info!(path = %path_str, "memory: removed file from index");
        }
        Ok(had_file || had_chunks)
    }

    async fn file_embeddings_current(&self, path: &str) -> Result<bool> {
        let Some(embedder) = self.embedder.as_ref() else {
            return Ok(true);
        };
        let chunks = self.store.get_chunks_for_file(path).await?;
        if chunks.is_empty() {
            return Ok(false);
        }
        let model = embedder.model_name();
        Ok(chunks
            .iter()
            .all(|chunk| chunk.model == model && chunk.embedding.is_some()))
    }

    /// Sync a single file. Returns true if it was updated. Accumulates cache stats in `report`.
    async fn sync_file(
        &self,
        path: &Path,
        path_str: &str,
        report: &mut SyncReport,
    ) -> Result<bool> {
        let metadata = tokio::fs::metadata(path).await?;
        let mtime = metadata
            .modified()?
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as i64;
        let size = metadata.len() as i64;

        // Fast path: skip read+hash if mtime and size are unchanged and embeddings match.
        if let Some(existing) = self.store.get_file(path_str).await?
            && existing.mtime == mtime
            && existing.size == size
            && self.file_embeddings_current(path_str).await?
        {
            return Ok(false);
        }

        let mut start_epoch = {
            let epoch = self.path_epoch(path_str).await;
            *epoch.lock().await
        };
        let content = tokio::fs::read_to_string(path).await?;
        let hash = sha256_hex(&content);

        // Check if content hash is unchanged (mtime changed but content didn't).
        if let Some(existing) = self.store.get_file(path_str).await? {
            if existing.hash == hash {
                // Update mtime so the fast path works next time.
                let file_row = FileRow {
                    path: path_str.to_string(),
                    source: existing.source,
                    hash: existing.hash,
                    mtime,
                    size,
                };
                if self.file_embeddings_current(path_str).await? {
                    self.store.upsert_file(&file_row).await?;
                    return Ok(false);
                }
            } else {
                self.bump_path_epoch(path_str).await;
                self.store.delete_chunks_for_file(path_str).await?;
                self.store.delete_file(path_str).await?;
                start_epoch = {
                    let epoch = self.path_epoch(path_str).await;
                    *epoch.lock().await
                };
            }
        }

        // Determine source from path
        let source = match path_str.contains("MEMORY") {
            true => "longterm",
            false => "daily",
        };

        // Update file record
        let file_row = FileRow {
            path: path_str.to_string(),
            source: source.to_string(),
            hash: hash.clone(),
            mtime,
            size,
        };
        info!(path = %path_str, source, size, "memory: loaded markdown file");

        // Chunk the content (tree-sitter AST splitting when grammar available, else line-based).
        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("md");
        let raw_chunks = chunk_content(
            &content,
            self.config.chunk_size,
            self.config.chunk_overlap,
            ext,
        );
        let raw_chunks = match self
            .embedder
            .as_ref()
            .and_then(|embedder| embedder.max_embed_payload_bytes())
        {
            Some(limit) => split_oversize_embed_chunks(raw_chunks, limit)?,
            None => raw_chunks,
        };

        // Generate embeddings before replacing index rows so a sidecar failure keeps the old chunks.
        let texts: Vec<String> = raw_chunks.iter().map(|c| c.text.clone()).collect();
        let chunk_hashes: Vec<String> = texts.iter().map(|t| sha256_hex(t)).collect();

        let (embeddings, model_name) = if let Some(ref embedder) = self.embedder {
            let provider_key = embedder.provider_key();
            let model = embedder.model_name();

            // Check cache for each chunk
            let mut cached: Vec<Option<Vec<f32>>> = Vec::with_capacity(texts.len());
            for h in &chunk_hashes {
                let hit = self
                    .store
                    .get_cached_embedding(provider_key, model, h)
                    .await?;
                cached.push(hit);
            }

            // Collect indices of cache misses
            let miss_indices: Vec<usize> = cached
                .iter()
                .enumerate()
                .filter(|(_, c)| c.is_none())
                .map(|(i, _)| i)
                .collect();

            report.cache_hits += texts.len() - miss_indices.len();
            report.cache_misses += miss_indices.len();

            // Build embedding vec: cached hits + placeholder for misses
            let mut all_embeddings: Vec<Vec<f32>> =
                cached.into_iter().map(|c| c.unwrap_or_default()).collect();

            // Embed cache misses and store them in a single transaction.
            if !miss_indices.is_empty() {
                let miss_texts: Vec<String> =
                    miss_indices.iter().map(|&i| texts[i].clone()).collect();
                let new_embs = embedder.embed_batch(&miss_texts).await?;

                let mut cache_entries = Vec::with_capacity(new_embs.len());
                for (idx, emb) in miss_indices.iter().zip(&new_embs) {
                    all_embeddings[*idx] = emb.clone();
                    cache_entries.push(CacheEntry {
                        provider: provider_key,
                        model,
                        provider_key,
                        hash: &chunk_hashes[*idx],
                        embedding: emb,
                    });
                }
                self.store
                    .put_cached_embeddings_batch(&cache_entries)
                    .await?;
            }

            (Some(all_embeddings), model.to_string())
        } else {
            (None, String::new())
        };

        let chunk_rows: Vec<ChunkRow> = raw_chunks
            .iter()
            .enumerate()
            .map(|(i, chunk)| {
                let emb_blob = embeddings.as_ref().map(|embs| {
                    embs[i]
                        .iter()
                        .flat_map(|f| f.to_le_bytes())
                        .collect::<Vec<u8>>()
                });
                ChunkRow {
                    id: format!("{}:{}", path_str, i),
                    path: path_str.to_string(),
                    source: source.to_string(),
                    start_line: chunk.start_line as i64,
                    end_line: chunk.end_line as i64,
                    hash: chunk_hashes[i].clone(),
                    model: model_name.clone(),
                    text: chunk.text.clone(),
                    embedding: emb_blob,
                    updated_at: chrono_now(),
                }
            })
            .collect();

        {
            let epoch = self.path_epoch(path_str).await;
            let current_epoch = epoch.lock().await;
            if *current_epoch != start_epoch || !tokio::fs::try_exists(path).await? {
                return Ok(false);
            }
            self.store
                .replace_file_index(&file_row, &chunk_rows)
                .await?;
        }
        info!(path = %path_str, chunks = chunk_rows.len(), "synced file");

        Ok(true)
    }

    /// Search memory. Uses hybrid (vector + keyword) when embeddings are available,
    /// falls back to keyword-only search otherwise.
    #[tracing::instrument(skip(self), fields(query_len = query.len(), limit))]
    pub async fn search(&self, query: &str, limit: usize) -> Result<Vec<SearchResult>> {
        if let Some(ref embedder) = self.embedder {
            search::hybrid_search(
                self.store.as_ref(),
                embedder.as_ref(),
                query,
                limit,
                self.config.vector_weight,
                self.config.keyword_weight,
                self.config.merge_strategy,
            )
            .await
        } else {
            search::keyword_only_search(self.store.as_ref(), query, limit).await
        }
    }

    /// Get a specific chunk by ID.
    pub async fn get_chunk(&self, id: &str) -> Result<Option<ChunkRow>> {
        self.store.get_chunk_by_id(id).await
    }

    /// Get status information about the memory system.
    pub async fn status(&self) -> Result<MemoryStatus> {
        let files = self.store.list_files().await?;
        let mut total_chunks = 0usize;
        for file in &files {
            let chunks = self.store.get_chunks_for_file(&file.path).await?;
            total_chunks += chunks.len();
        }
        let db_size_bytes = std::fs::metadata(&self.config.db_path)
            .map(|m| m.len())
            .unwrap_or(0);
        Ok(MemoryStatus {
            total_files: files.len(),
            total_chunks,
            embedding_model: self
                .embedder
                .as_ref()
                .map(|e| e.model_name().to_string())
                .unwrap_or_else(|| "none (keyword-only)".into()),
            db_size_bytes,
        })
    }
}

/// Maximum content size per write (50 KB).
const MAX_CONTENT_BYTES: usize = 50 * 1024;

#[async_trait]
impl MemoryWriter for MemoryManager {
    async fn write_memory(
        &self,
        file: &str,
        content: &str,
        append: bool,
    ) -> anyhow::Result<MemoryWriteResult> {
        let data_dir = self.config.data_dir.as_ref().ok_or_else(|| {
            anyhow::anyhow!("memory writes are disabled (no data_dir configured)")
        })?;

        if content.len() > MAX_CONTENT_BYTES {
            anyhow::bail!(
                "content exceeds maximum size of {} bytes ({} bytes provided)",
                MAX_CONTENT_BYTES,
                content.len()
            );
        }

        let path = validate_memory_path(data_dir, file)?;

        // Create parent directories if needed.
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }

        let final_content = if append && path.exists() {
            let existing = tokio::fs::read_to_string(&path).await?;
            format!("{existing}\n\n{content}")
        } else {
            content.to_string()
        };

        let bytes_written = final_content.len();
        tokio::fs::write(&path, &final_content).await?;

        debug!(path = %path.display(), bytes = bytes_written, "memory manager: wrote file");

        // Re-index so the content is immediately searchable.
        if let Err(e) = self.sync_path(&path).await {
            warn!(path = %path.display(), error = %e, "memory manager: re-index after write failed");
        }

        Ok(MemoryWriteResult {
            location: path.to_string_lossy().into_owned(),
            bytes_written,
        })
    }
}

/// Sync report.
#[derive(Debug, Default)]
pub struct SyncReport {
    pub files_updated: usize,
    pub files_unchanged: usize,
    pub files_removed: usize,
    pub errors: usize,
    pub cache_hits: usize,
    pub cache_misses: usize,
}

/// Maximum number of embedding cache rows before LRU eviction kicks in.
const CACHE_MAX_ROWS: usize = 50_000;

fn sha256_hex(data: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(data.as_bytes());
    format!("{:x}", hasher.finalize())
}

fn chrono_now() -> String {
    // Simple ISO 8601 timestamp without chrono dependency
    let dur = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    format!("{}", dur.as_secs())
}

#[allow(clippy::unwrap_used, clippy::expect_used)]
#[cfg(test)]
mod tests {
    use {
        super::*,
        crate::{
            schema::{ChunkRow, FileRow, run_migrations},
            store_sqlite::SqliteMemoryStore,
        },
        async_trait::async_trait,
        std::{
            io::Write,
            path::{Path, PathBuf},
            sync::{
                Arc,
                atomic::{AtomicUsize, Ordering},
            },
        },
        tempfile::TempDir,
    };

    /// Mock embedding provider that produces deterministic vectors from content.
    ///
    /// Uses a simple bag-of-keywords approach: each of 8 dimensions corresponds to a
    /// keyword. If the text contains that keyword the dimension is 1.0, otherwise 0.0.
    /// This lets vector search distinguish topics in tests.
    struct MockEmbedder;

    const KEYWORDS: [&str; 8] = [
        "rust", "python", "database", "memory", "search", "network", "cooking", "music",
    ];

    fn keyword_embedding(text: &str) -> Vec<f32> {
        let lower = text.to_lowercase();
        KEYWORDS
            .iter()
            .map(|kw| {
                if lower.contains(kw) {
                    1.0
                } else {
                    0.0
                }
            })
            .collect()
    }

    #[async_trait]
    impl EmbeddingProvider for MockEmbedder {
        async fn embed(&self, text: &str) -> Result<Vec<f32>> {
            Ok(keyword_embedding(text))
        }

        fn model_name(&self) -> &str {
            "mock-model"
        }

        fn dimensions(&self) -> usize {
            8
        }

        fn provider_key(&self) -> &str {
            "mock"
        }
    }

    fn notes_dir(data_dir: &Path) -> PathBuf {
        data_dir.join("agents").join("main").join("memory")
    }

    fn write_note(data_dir: &Path, name: &str, content: &str) -> PathBuf {
        let dir = notes_dir(data_dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(name);
        std::fs::write(&path, content).unwrap();
        path
    }

    fn manager_config(data_dir: PathBuf) -> MemoryConfig {
        MemoryConfig {
            db_path: ":memory:".into(),
            data_dir: Some(data_dir.clone()),
            memory_dirs: vec![data_dir.join("MEMORY.md"), data_dir.join("agents")],
            chunk_size: 50,
            chunk_overlap: 10,
            vector_weight: 0.7,
            keyword_weight: 0.3,
            ..Default::default()
        }
    }

    async fn setup() -> (MemoryManager, TempDir) {
        let tmp = TempDir::new().unwrap();
        let data_dir = tmp.path().to_path_buf();
        std::fs::create_dir_all(notes_dir(&data_dir)).unwrap();
        let pool = sqlx::SqlitePool::connect(":memory:").await.unwrap();
        run_migrations(&pool).await.unwrap();
        let store = Box::new(SqliteMemoryStore::new(pool));
        let embedder = Box::new(MockEmbedder);
        (
            MemoryManager::new(manager_config(data_dir), store, embedder),
            tmp,
        )
    }

    #[tokio::test]
    async fn test_sync_and_search() {
        let (manager, tmp) = setup().await;
        let note = write_note(tmp.path(), "2024-01-01.md", "");
        let mut f = std::fs::File::create(&note).unwrap();
        writeln!(f, "# Daily Log").unwrap();
        writeln!(f, "Today I worked on the Rust memory system.").unwrap();
        writeln!(f, "It uses SQLite for storage and hybrid search.").unwrap();

        // Sync
        let report = manager.sync().await.unwrap();
        assert_eq!(report.files_updated, 1);
        assert_eq!(report.files_unchanged, 0);

        // Sync again - should be unchanged
        let report2 = manager.sync().await.unwrap();
        assert_eq!(report2.files_updated, 0);
        assert_eq!(report2.files_unchanged, 1);

        // Status
        let status = manager.status().await.unwrap();
        assert_eq!(status.total_files, 1);
        assert!(status.total_chunks > 0);
        assert_eq!(status.embedding_model, "mock-model");
    }

    #[tokio::test]
    async fn sync_reconciles_equal_sized_path_sets_when_allowed_file_fails() {
        let (manager, tmp) = setup().await;
        let data_dir = tmp.path();
        let allowed = data_dir.join("MEMORY.md");
        std::fs::write(&allowed, "allowed alpha notes").unwrap();
        assert_eq!(manager.sync().await.unwrap().files_updated, 1);

        let forbidden = data_dir.join("agents").join("main").join("SOUL.md");
        std::fs::write(&forbidden, "forbidden soul").unwrap();
        let forbidden_path = forbidden.to_string_lossy().into_owned();
        manager
            .store
            .upsert_file(&FileRow {
                path: forbidden_path.clone(),
                source: "md".into(),
                hash: "stale".into(),
                mtime: 1,
                size: 14,
            })
            .await
            .unwrap();
        manager
            .store
            .upsert_chunks(&[ChunkRow {
                id: "stale-chunk".into(),
                path: forbidden_path.clone(),
                source: "md".into(),
                start_line: 1,
                end_line: 1,
                hash: "stale".into(),
                model: "mock-model".into(),
                text: "forbidden soul".into(),
                embedding: None,
                updated_at: "now".into(),
            }])
            .await
            .unwrap();

        let failing = data_dir.join("agents").join("other");
        std::fs::create_dir_all(&failing).unwrap();
        std::fs::write(failing.join("MEMORY.md"), [0xff, 0xfe, 0xfd]).unwrap();

        let report = manager.sync().await.unwrap();
        assert_eq!(report.files_updated, 0);
        assert!(report.errors > 0);
        assert!(report.files_removed > 0);

        let indexed: Vec<String> = manager
            .store
            .list_files()
            .await
            .unwrap()
            .into_iter()
            .map(|file| file.path)
            .collect();
        assert!(
            indexed
                .iter()
                .any(|path| path == &allowed.to_string_lossy())
        );
        assert!(!indexed.iter().any(|path| path == &forbidden_path));

        assert_eq!(
            std::fs::read_to_string(&allowed).unwrap(),
            "allowed alpha notes"
        );
        assert_eq!(
            std::fs::read_to_string(&forbidden).unwrap(),
            "forbidden soul"
        );
        assert!(failing.join("MEMORY.md").exists());
    }

    #[tokio::test]
    async fn test_sync_detects_changes() {
        let (manager, tmp) = setup().await;
        let mem_dir = notes_dir(tmp.path());
        let file_path = mem_dir.join("test.md");

        std::fs::write(&file_path, "version 1").unwrap();
        let r1 = manager.sync().await.unwrap();
        assert_eq!(r1.files_updated, 1);

        std::fs::write(&file_path, "version 2 with different content").unwrap();
        let r2 = manager.sync().await.unwrap();
        assert_eq!(r2.files_updated, 1);
    }

    #[tokio::test]
    async fn test_sync_removes_deleted_files() {
        let (manager, tmp) = setup().await;
        let mem_dir = notes_dir(tmp.path());
        let file_path = mem_dir.join("temp.md");

        std::fs::write(&file_path, "temporary content").unwrap();
        manager.sync().await.unwrap();

        std::fs::remove_file(&file_path).unwrap();
        let report = manager.sync().await.unwrap();
        assert_eq!(report.files_removed, 1);
    }

    /// End-to-end: sync markdown files, then search and verify the returned text
    /// matches what was written.
    #[tokio::test]
    async fn test_search_returns_synced_content() {
        let (manager, tmp) = setup().await;
        let mem_dir = notes_dir(tmp.path());

        std::fs::write(
            mem_dir.join("2024-01-15.md"),
            "# Rust and memory\nToday I built a Rust memory system with search capabilities.",
        )
        .unwrap();

        manager.sync().await.unwrap();

        // Search for "rust memory" — should return the chunk we just synced
        let results = manager.search("rust memory", 5).await.unwrap();
        assert!(!results.is_empty(), "search should return results");
        let texts: Vec<&str> = results.iter().map(|r| r.text.as_str()).collect();
        let combined = texts.join(" ");
        assert!(
            combined.contains("Rust memory system"),
            "search results should contain the synced text, got: {combined}"
        );
    }

    /// Keyword (FTS) search works through the manager after sync.
    #[tokio::test]
    async fn test_keyword_search_through_manager() {
        let (manager, tmp) = setup().await;
        let mem_dir = notes_dir(tmp.path());

        std::fs::write(
            mem_dir.join("log.md"),
            "Rust programming is great for building fast systems.",
        )
        .unwrap();

        manager.sync().await.unwrap();

        // Keyword search bypasses embeddings—FTS5 MATCH query
        let results = manager.search("programming", 5).await.unwrap();
        assert!(
            !results.is_empty(),
            "keyword search should find 'programming'"
        );
        assert!(
            results[0].text.contains("programming"),
            "top result should contain the search term"
        );
    }

    /// Multiple files with distinct topics: searching for one topic should rank that
    /// file's chunks higher than unrelated files.
    #[tokio::test]
    async fn test_multi_file_topic_separation() {
        let (manager, tmp) = setup().await;
        let mem_dir = notes_dir(tmp.path());

        std::fs::write(
            mem_dir.join("rust.md"),
            "Rust is a systems programming language focused on safety and performance.",
        )
        .unwrap();
        std::fs::write(
            mem_dir.join("cooking.md"),
            "Today I tried a new cooking recipe for pasta with garlic and olive oil.",
        )
        .unwrap();
        std::fs::write(
            mem_dir.join("music.md"),
            "Listened to music all afternoon. Jazz and classical music are relaxing.",
        )
        .unwrap();

        manager.sync().await.unwrap();

        let status = manager.status().await.unwrap();
        assert_eq!(status.total_files, 3);

        // Search for "rust" — the rust.md chunk should come first
        let results = manager.search("rust", 5).await.unwrap();
        assert!(!results.is_empty());
        assert!(
            results[0].path.contains("rust.md"),
            "top result for 'rust' should come from rust.md, got: {}",
            results[0].path
        );

        // Search for "cooking" — the cooking.md chunk should come first
        let results = manager.search("cooking", 5).await.unwrap();
        assert!(!results.is_empty());
        assert!(
            results[0].path.contains("cooking.md"),
            "top result for 'cooking' should come from cooking.md, got: {}",
            results[0].path
        );

        // Search for "music" — the music.md chunk should come first
        let results = manager.search("music", 5).await.unwrap();
        assert!(!results.is_empty());
        assert!(
            results[0].path.contains("music.md"),
            "top result for 'music' should come from music.md, got: {}",
            results[0].path
        );
    }

    /// Sync many files and verify search still completes (basic scale sanity check).
    #[tokio::test]
    async fn test_scale_many_files() {
        let (manager, tmp) = setup().await;
        let mem_dir = notes_dir(tmp.path());

        // Create 50 files, each with several lines
        for i in 0..50 {
            let topic = &KEYWORDS[i % KEYWORDS.len()];
            let mut content = format!("# File {i} about {topic}\n\n");
            for j in 0..20 {
                content.push_str(&format!(
                    "Line {j}: This paragraph discusses {topic} in detail with enough words to fill a line.\n"
                ));
            }
            std::fs::write(mem_dir.join(format!("file_{i:03}.md")), &content).unwrap();
        }

        let report = manager.sync().await.unwrap();
        assert_eq!(report.files_updated, 50);

        let status = manager.status().await.unwrap();
        assert_eq!(status.total_files, 50);
        assert!(
            status.total_chunks >= 50,
            "should have at least one chunk per file, got {}",
            status.total_chunks
        );

        // Search should still return results
        let results = manager.search("database", 10).await.unwrap();
        assert!(
            !results.is_empty(),
            "search across 50 files should return results"
        );

        // All top results should be about database
        for r in &results {
            assert!(
                r.text.to_lowercase().contains("database"),
                "result should be about database, got: {}",
                r.text.chars().take(80).collect::<String>()
            );
        }
    }

    /// Keyword-only mode: sync and search without any embedding provider.
    #[tokio::test]
    async fn test_keyword_only_mode() {
        let tmp = TempDir::new().unwrap();
        let mem_dir = notes_dir(tmp.path());
        std::fs::create_dir_all(&mem_dir).unwrap();

        let pool = sqlx::SqlitePool::connect(":memory:").await.unwrap();
        run_migrations(&pool).await.unwrap();

        let data_dir = tmp.path().to_path_buf();
        let config = manager_config(data_dir);

        let store = Box::new(SqliteMemoryStore::new(pool));
        let manager = MemoryManager::keyword_only(config, store);

        assert!(!manager.has_embeddings());

        // Write a test file and sync (should work without embeddings).
        std::fs::write(
            mem_dir.join("note.md"),
            "Rust programming is great for building fast systems.",
        )
        .unwrap();

        let report = manager.sync().await.unwrap();
        assert_eq!(report.files_updated, 1);

        let status = manager.status().await.unwrap();
        assert_eq!(status.total_files, 1);
        assert!(status.total_chunks > 0);
        assert_eq!(status.embedding_model, "none (keyword-only)");

        // Keyword search should still work.
        let results = manager.search("programming", 5).await.unwrap();
        assert!(
            !results.is_empty(),
            "keyword-only search should find results"
        );
        assert!(results[0].text.contains("programming"));
    }

    /// Mock embedder that counts how many texts it has been asked to embed.
    struct CountingEmbedder {
        embed_count: AtomicUsize,
    }

    impl CountingEmbedder {
        fn new() -> Self {
            Self {
                embed_count: AtomicUsize::new(0),
            }
        }

        fn count(&self) -> usize {
            self.embed_count.load(Ordering::SeqCst)
        }
    }

    #[async_trait]
    impl EmbeddingProvider for CountingEmbedder {
        async fn embed(&self, text: &str) -> Result<Vec<f32>> {
            self.embed_count.fetch_add(1, Ordering::SeqCst);
            Ok(keyword_embedding(text))
        }

        fn model_name(&self) -> &str {
            "mock-model"
        }

        fn dimensions(&self) -> usize {
            8
        }

        fn provider_key(&self) -> &str {
            "mock"
        }
    }

    #[tokio::test]
    async fn test_embedding_cache_hits() {
        let tmp = TempDir::new().unwrap();
        let mem_dir = notes_dir(tmp.path());
        std::fs::create_dir_all(&mem_dir).unwrap();

        let pool = sqlx::SqlitePool::connect(":memory:").await.unwrap();
        run_migrations(&pool).await.unwrap();

        let data_dir = tmp.path().to_path_buf();
        let config = manager_config(data_dir);

        let embedder = Arc::new(CountingEmbedder::new());
        let embedder_ref = Arc::clone(&embedder);

        // Wrap in a forwarding provider that delegates to the Arc'd one.
        struct ArcEmbedder(Arc<CountingEmbedder>);

        #[async_trait]
        impl EmbeddingProvider for ArcEmbedder {
            async fn embed(&self, text: &str) -> Result<Vec<f32>> {
                self.0.embed(text).await
            }

            fn model_name(&self) -> &str {
                self.0.model_name()
            }

            fn dimensions(&self) -> usize {
                self.0.dimensions()
            }

            fn provider_key(&self) -> &str {
                self.0.provider_key()
            }
        }

        let store = Box::new(SqliteMemoryStore::new(pool));
        let manager = MemoryManager::new(config, store, Box::new(ArcEmbedder(embedder)));

        // Write a file and sync
        std::fs::write(
            mem_dir.join("test.md"),
            "Rust programming with database and memory search features.",
        )
        .unwrap();

        let r1 = manager.sync().await.unwrap();
        assert_eq!(r1.files_updated, 1);
        assert!(r1.cache_misses > 0);
        assert_eq!(r1.cache_hits, 0);
        let first_embed_count = embedder_ref.count();
        assert!(first_embed_count > 0);

        // Modify the file so it gets re-chunked, but same text content -> cache hits
        // Actually, we need to change the file hash to trigger re-sync.
        // Instead, delete the file record but keep the cache, then re-sync.
        // Simplest: write a second file with same chunk text won't work.
        // Best approach: delete file from store and re-sync.
        // Actually the easiest way: write same content but change the file hash
        // by adding a trailing newline.
        std::fs::write(
            mem_dir.join("test.md"),
            "Rust programming with database and memory search features.\n",
        )
        .unwrap();

        let r2 = manager.sync().await.unwrap();
        assert_eq!(r2.files_updated, 1);
        // The chunk text is the same, so we should get cache hits
        assert!(r2.cache_hits > 0, "second sync should have cache hits");
        // No new embeddings should have been generated
        assert_eq!(
            embedder_ref.count(),
            first_embed_count,
            "no new embed calls expected on cache hit"
        );
    }

    #[test]
    fn test_sha256_hex() {
        let hash = sha256_hex("hello");
        assert_eq!(hash.len(), 64);
        // Known SHA-256 of "hello"
        assert_eq!(
            hash,
            "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824"
        );
    }

    // --- MemoryWriter impl tests ---

    /// Create a `MemoryManager` with `data_dir` set, enabling write support.
    async fn setup_writable() -> (MemoryManager, TempDir) {
        setup().await
    }

    #[tokio::test]
    async fn test_memory_writer_overwrite() {
        let (manager, tmp) = setup_writable().await;
        let data_dir = tmp.path().to_path_buf();

        manager
            .write_memory("MEMORY.md", "first", false)
            .await
            .unwrap();
        manager
            .write_memory("MEMORY.md", "second", false)
            .await
            .unwrap();

        let content = std::fs::read_to_string(data_dir.join("MEMORY.md")).unwrap();
        assert_eq!(content, "second");
    }

    #[tokio::test]
    async fn test_memory_writer_append() {
        let (manager, tmp) = setup_writable().await;
        let data_dir = tmp.path().to_path_buf();

        manager
            .write_memory("MEMORY.md", "first", false)
            .await
            .unwrap();
        manager
            .write_memory("MEMORY.md", "second", true)
            .await
            .unwrap();

        let content = std::fs::read_to_string(data_dir.join("MEMORY.md")).unwrap();
        assert!(content.contains("first"));
        assert!(content.contains("second"));
    }

    #[tokio::test]
    async fn test_memory_writer_size_limit() {
        let (manager, _tmp) = setup_writable().await;

        let big = "x".repeat(MAX_CONTENT_BYTES + 1);
        let result = manager.write_memory("MEMORY.md", &big, false).await;
        assert!(result.is_err(), "oversized content should be rejected");

        let at_limit = "x".repeat(MAX_CONTENT_BYTES);
        let result = manager.write_memory("MEMORY.md", &at_limit, false).await;
        assert!(result.is_ok(), "content at limit should succeed");
    }

    #[tokio::test]
    async fn test_memory_writer_rejects_path_traversal() {
        let (manager, _tmp) = setup_writable().await;

        for bad_path in &[
            "../etc/passwd",
            "memory/../../../etc/passwd",
            "memory/../../secret.md",
        ] {
            let result = manager.write_memory(bad_path, "test", false).await;
            assert!(result.is_err(), "should reject path traversal: {bad_path}");
        }
    }

    #[tokio::test]
    async fn test_memory_writer_rejects_absolute_paths() {
        let (manager, _tmp) = setup_writable().await;

        let result = manager.write_memory("/etc/passwd", "test", false).await;
        assert!(result.is_err(), "should reject absolute paths");
    }

    #[tokio::test]
    async fn test_memory_writer_rejects_invalid_names() {
        let (manager, _tmp) = setup_writable().await;

        let invalid = &[
            "memory/notes.txt",
            "memory/.md",
            "memory/a b c.md",
            "memory/sub/nested.md",
            "random.md",
            "foo/bar.md",
        ];

        for name in invalid {
            let result = manager.write_memory(name, "test", false).await;
            assert!(result.is_err(), "should reject invalid name: {name}");
        }
    }

    #[tokio::test]
    async fn test_memory_writer_reindexes() {
        let (manager, _tmp) = setup_writable().await;

        manager
            .write_memory(
                "MEMORY.md",
                "The cooking recipe uses garlic and olive oil.",
                false,
            )
            .await
            .unwrap();

        // Content should be immediately searchable
        let results = manager.search("cooking", 5).await.unwrap();
        assert!(!results.is_empty(), "saved content should be searchable");
        assert!(
            results[0].text.contains("cooking"),
            "search should find the saved text"
        );
    }

    #[tokio::test]
    async fn test_memory_writer_returns_correct_result() {
        let (manager, tmp) = setup_writable().await;
        let data_dir = tmp.path().to_path_buf();

        let result = manager
            .write_memory("MEMORY.md", "hello world", false)
            .await
            .unwrap();

        assert_eq!(
            result.location,
            data_dir.join("MEMORY.md").to_string_lossy()
        );
        assert_eq!(result.bytes_written, "hello world".len());
    }

    #[tokio::test]
    async fn test_memory_writer_disabled_without_data_dir() {
        let tmp = TempDir::new().unwrap();
        let pool = sqlx::SqlitePool::connect(":memory:").await.unwrap();
        run_migrations(&pool).await.unwrap();
        let config = MemoryConfig {
            db_path: ":memory:".into(),
            data_dir: None,
            chunk_size: 50,
            chunk_overlap: 10,
            ..Default::default()
        };
        let manager = MemoryManager::keyword_only(config, Box::new(SqliteMemoryStore::new(pool)));
        let _tmp = tmp;

        let result = manager.write_memory("MEMORY.md", "test", false).await;
        assert!(result.is_err(), "writes should fail without data_dir");
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("no data_dir configured"),
            "error should mention data_dir"
        );
    }

    async fn file_pool(db_path: &Path) -> sqlx::SqlitePool {
        use {sqlx::sqlite::SqliteConnectOptions, std::str::FromStr};
        let options = SqliteConnectOptions::from_str(&format!("sqlite:{}", db_path.display()))
            .unwrap()
            .create_if_missing(true);
        sqlx::SqlitePool::connect_with(options).await.unwrap()
    }

    async fn file_backed_manager(
        db_path: &Path,
        data_dir: PathBuf,
        embedder: Box<dyn EmbeddingProvider>,
    ) -> MemoryManager {
        let pool = file_pool(db_path).await;
        run_migrations(&pool).await.unwrap();
        let mut config = manager_config(data_dir);
        config.db_path = db_path.to_string_lossy().into_owned();
        MemoryManager::new(config, Box::new(SqliteMemoryStore::new(pool)), embedder)
    }

    async fn chunks_for(db_path: &Path, path: &str) -> Vec<ChunkRow> {
        let store = SqliteMemoryStore::new(file_pool(db_path).await);
        store.get_chunks_for_file(path).await.unwrap()
    }

    async fn indexed_file(db_path: &Path, path: &str) -> Option<FileRow> {
        let store = SqliteMemoryStore::new(file_pool(db_path).await);
        store.get_file(path).await.unwrap()
    }

    struct FailingEmbedder {
        model: &'static str,
    }

    #[async_trait]
    impl EmbeddingProvider for FailingEmbedder {
        async fn embed(&self, _text: &str) -> Result<Vec<f32>> {
            Err(crate::error::Error::Embedding("sidecar failed".into()))
        }

        fn model_name(&self) -> &str {
            self.model
        }

        fn dimensions(&self) -> usize {
            8
        }

        fn provider_key(&self) -> &str {
            self.model
        }
    }

    struct NamedEmbedder {
        name: &'static str,
    }

    #[async_trait]
    impl EmbeddingProvider for NamedEmbedder {
        async fn embed(&self, text: &str) -> Result<Vec<f32>> {
            Ok(keyword_embedding(text))
        }

        fn model_name(&self) -> &str {
            self.name
        }

        fn dimensions(&self) -> usize {
            8
        }

        fn provider_key(&self) -> &str {
            self.name
        }
    }

    #[tokio::test]
    async fn embed_failure_keeps_previous_chunks() {
        let tmp = TempDir::new().unwrap();
        let mem_dir = notes_dir(tmp.path());
        std::fs::create_dir_all(&mem_dir).unwrap();
        let db_path = tmp.path().join("memory.db");
        let note = mem_dir.join("note.md");
        let path_str = note.to_string_lossy().into_owned();
        std::fs::write(&note, "Rust programming with database memory.").unwrap();

        {
            let manager = file_backed_manager(
                &db_path,
                tmp.path().to_path_buf(),
                Box::new(NamedEmbedder { name: "mock-model" }),
            )
            .await;
            let report = manager.sync().await.unwrap();
            assert_eq!(report.files_updated, 1);
        }
        let chunks = chunks_for(&db_path, &path_str).await;
        assert!(!chunks.is_empty());
        assert!(chunks.iter().all(|chunk| chunk.model == "mock-model"));

        let manager = file_backed_manager(
            &db_path,
            tmp.path().to_path_buf(),
            Box::new(FailingEmbedder { model: "new-model" }),
        )
        .await;
        let report = manager.sync().await.unwrap();
        assert!(report.errors > 0);
        let chunks = chunks_for(&db_path, &path_str).await;
        assert!(!chunks.is_empty());
        assert!(chunks.iter().any(|chunk| chunk.text.contains("Rust")));
        assert!(chunks.iter().all(|chunk| chunk.model == "mock-model"));
    }

    #[tokio::test]
    async fn model_change_reembeds_unchanged_markdown() {
        let tmp = TempDir::new().unwrap();
        let mem_dir = notes_dir(tmp.path());
        std::fs::create_dir_all(&mem_dir).unwrap();
        let db_path = tmp.path().join("memory.db");
        let note = mem_dir.join("note.md");
        let path_str = note.to_string_lossy().into_owned();
        std::fs::write(&note, "Rust programming with database memory.").unwrap();

        {
            let manager = file_backed_manager(
                &db_path,
                tmp.path().to_path_buf(),
                Box::new(NamedEmbedder { name: "old-model" }),
            )
            .await;
            assert_eq!(manager.sync().await.unwrap().files_updated, 1);
        }

        let manager = file_backed_manager(
            &db_path,
            tmp.path().to_path_buf(),
            Box::new(NamedEmbedder { name: "new-model" }),
        )
        .await;
        let report = manager.sync().await.unwrap();
        assert_eq!(report.files_updated, 1);
        let chunks = chunks_for(&db_path, &path_str).await;
        assert!(!chunks.is_empty());
        assert!(chunks.iter().all(|chunk| chunk.model == "new-model"));
    }

    struct GatedEmbedder {
        started: tokio::sync::Mutex<Option<tokio::sync::oneshot::Sender<()>>>,
        release: tokio::sync::Mutex<Option<tokio::sync::oneshot::Receiver<()>>>,
        name: &'static str,
    }

    #[async_trait]
    impl EmbeddingProvider for GatedEmbedder {
        async fn embed(&self, text: &str) -> Result<Vec<f32>> {
            if let Some(started) = self.started.lock().await.take() {
                let _ = started.send(());
            }
            if let Some(release) = self.release.lock().await.take() {
                let _ = release.await;
            }
            Ok(keyword_embedding(text))
        }

        fn model_name(&self) -> &str {
            self.name
        }

        fn dimensions(&self) -> usize {
            8
        }

        fn provider_key(&self) -> &str {
            self.name
        }
    }

    #[tokio::test]
    async fn delete_during_embed_does_not_restore_index() {
        let tmp = TempDir::new().unwrap();
        let mem_dir = notes_dir(tmp.path());
        std::fs::create_dir_all(&mem_dir).unwrap();
        let db_path = tmp.path().join("memory.db");
        let note = mem_dir.join("note.md");
        let path_str = note.to_string_lossy().into_owned();
        std::fs::write(&note, "Rust programming with database memory.").unwrap();

        {
            let manager = file_backed_manager(
                &db_path,
                tmp.path().to_path_buf(),
                Box::new(NamedEmbedder { name: "old-model" }),
            )
            .await;
            assert_eq!(manager.sync().await.unwrap().files_updated, 1);
        }

        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = tokio::sync::oneshot::channel();
        let manager = Arc::new(
            file_backed_manager(
                &db_path,
                tmp.path().to_path_buf(),
                Box::new(GatedEmbedder {
                    started: tokio::sync::Mutex::new(Some(started_tx)),
                    release: tokio::sync::Mutex::new(Some(release_rx)),
                    name: "new-model",
                }),
            )
            .await,
        );
        let sync_manager = Arc::clone(&manager);
        let sync_task = tokio::spawn(async move { sync_manager.sync().await });
        started_rx.await.unwrap();
        std::fs::remove_file(&note).unwrap();
        assert!(manager.remove_path(&note).await.unwrap());
        release_tx.send(()).unwrap();
        let report = sync_task.await.unwrap().unwrap();
        assert_eq!(report.errors, 0);
        assert!(chunks_for(&db_path, &path_str).await.is_empty());
        assert!(indexed_file(&db_path, &path_str).await.is_none());
    }

    #[tokio::test]
    async fn partial_delete_embed_failure_does_not_keep_removed_text() {
        let tmp = TempDir::new().unwrap();
        let mem_dir = notes_dir(tmp.path());
        std::fs::create_dir_all(&mem_dir).unwrap();
        let db_path = tmp.path().join("memory.db");
        let note = mem_dir.join("note.md");
        let path_str = note.to_string_lossy().into_owned();
        std::fs::write(&note, "SECRETTOKEN keep this note.").unwrap();

        {
            let manager = file_backed_manager(
                &db_path,
                tmp.path().to_path_buf(),
                Box::new(NamedEmbedder { name: "old-model" }),
            )
            .await;
            assert_eq!(manager.sync().await.unwrap().files_updated, 1);
            assert!(
                chunks_for(&db_path, &path_str)
                    .await
                    .iter()
                    .any(|chunk| chunk.text.contains("SECRETTOKEN"))
            );
        }

        std::fs::write(&note, "keep this note.").unwrap();
        let manager = file_backed_manager(
            &db_path,
            tmp.path().to_path_buf(),
            Box::new(FailingEmbedder { model: "old-model" }),
        )
        .await;
        assert!(manager.remove_path(&note).await.unwrap());
        assert!(manager.sync_path(&note).await.is_err());
        assert!(
            chunks_for(&db_path, &path_str)
                .await
                .iter()
                .all(|chunk| !chunk.text.contains("SECRETTOKEN"))
        );
    }

    #[tokio::test]
    async fn stale_sync_after_partial_delete_does_not_restore_removed_text() {
        let tmp = TempDir::new().unwrap();
        let mem_dir = notes_dir(tmp.path());
        std::fs::create_dir_all(&mem_dir).unwrap();
        let db_path = tmp.path().join("memory.db");
        let note = mem_dir.join("note.md");
        let path_str = note.to_string_lossy().into_owned();
        std::fs::write(&note, "SECRETTOKEN keep this note.").unwrap();

        {
            let manager = file_backed_manager(
                &db_path,
                tmp.path().to_path_buf(),
                Box::new(NamedEmbedder { name: "old-model" }),
            )
            .await;
            assert_eq!(manager.sync().await.unwrap().files_updated, 1);
        }

        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = tokio::sync::oneshot::channel();
        let manager = Arc::new(
            file_backed_manager(
                &db_path,
                tmp.path().to_path_buf(),
                Box::new(GatedEmbedder {
                    started: tokio::sync::Mutex::new(Some(started_tx)),
                    release: tokio::sync::Mutex::new(Some(release_rx)),
                    name: "new-model",
                }),
            )
            .await,
        );
        let sync_manager = Arc::clone(&manager);
        let note_for_sync = note.clone();
        let sync_task = tokio::spawn(async move { sync_manager.sync_path(&note_for_sync).await });
        started_rx.await.unwrap();
        std::fs::write(&note, "keep this note.").unwrap();
        assert!(manager.remove_path(&note).await.unwrap());
        release_tx.send(()).unwrap();
        let _ = sync_task.await.unwrap();
        assert!(
            chunks_for(&db_path, &path_str)
                .await
                .iter()
                .all(|chunk| !chunk.text.contains("SECRETTOKEN"))
        );
    }

    #[tokio::test]
    async fn content_change_embed_failure_drops_stale_chunks() {
        let tmp = TempDir::new().unwrap();
        let mem_dir = notes_dir(tmp.path());
        std::fs::create_dir_all(&mem_dir).unwrap();
        let db_path = tmp.path().join("memory.db");
        let note = mem_dir.join("note.md");
        let path_str = note.to_string_lossy().into_owned();
        std::fs::write(&note, "SECRETTOKEN original version.").unwrap();

        {
            let manager = file_backed_manager(
                &db_path,
                tmp.path().to_path_buf(),
                Box::new(NamedEmbedder { name: "old-model" }),
            )
            .await;
            assert_eq!(manager.sync().await.unwrap().files_updated, 1);
        }

        std::fs::write(&note, "replacement without secret.").unwrap();
        let manager = file_backed_manager(
            &db_path,
            tmp.path().to_path_buf(),
            Box::new(FailingEmbedder { model: "old-model" }),
        )
        .await;
        assert!(manager.sync_path(&note).await.is_err());
        assert!(
            chunks_for(&db_path, &path_str)
                .await
                .iter()
                .all(|chunk| !chunk.text.contains("SECRETTOKEN"))
        );
    }

    struct FirstEmbedGatesThenFails {
        started: tokio::sync::Mutex<Option<tokio::sync::oneshot::Sender<()>>>,
        release: tokio::sync::Mutex<Option<tokio::sync::oneshot::Receiver<()>>>,
        seen: AtomicUsize,
        name: &'static str,
    }

    #[async_trait]
    impl EmbeddingProvider for FirstEmbedGatesThenFails {
        async fn embed(&self, text: &str) -> Result<Vec<f32>> {
            let call = self.seen.fetch_add(1, Ordering::SeqCst);
            if call == 0 {
                if let Some(started) = self.started.lock().await.take() {
                    let _ = started.send(());
                }
                if let Some(release) = self.release.lock().await.take() {
                    let _ = release.await;
                }
                return Ok(keyword_embedding(text));
            }
            Err(crate::error::Error::Embedding("sidecar failed".into()))
        }

        fn model_name(&self) -> &str {
            self.name
        }

        fn dimensions(&self) -> usize {
            8
        }

        fn provider_key(&self) -> &str {
            self.name
        }
    }

    #[tokio::test]
    async fn overwrite_during_stale_model_sync_does_not_restore_old_text() {
        let tmp = TempDir::new().unwrap();
        let mem_dir = notes_dir(tmp.path());
        std::fs::create_dir_all(&mem_dir).unwrap();
        let db_path = tmp.path().join("memory.db");
        let note = mem_dir.join("note.md");
        let path_str = note.to_string_lossy().into_owned();
        std::fs::write(&note, "SECRETTOKEN original version.").unwrap();

        {
            let manager = file_backed_manager(
                &db_path,
                tmp.path().to_path_buf(),
                Box::new(NamedEmbedder { name: "old-model" }),
            )
            .await;
            assert_eq!(manager.sync().await.unwrap().files_updated, 1);
        }

        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = tokio::sync::oneshot::channel();
        let manager = Arc::new(
            file_backed_manager(
                &db_path,
                tmp.path().to_path_buf(),
                Box::new(FirstEmbedGatesThenFails {
                    started: tokio::sync::Mutex::new(Some(started_tx)),
                    release: tokio::sync::Mutex::new(Some(release_rx)),
                    seen: AtomicUsize::new(0),
                    name: "new-model",
                }),
            )
            .await,
        );
        let sync_manager = Arc::clone(&manager);
        let note_for_sync = note.clone();
        let stale_sync = tokio::spawn(async move { sync_manager.sync_path(&note_for_sync).await });
        started_rx.await.unwrap();
        std::fs::write(&note, "replacement without secret.").unwrap();
        assert!(manager.sync_path(&note).await.is_err());
        release_tx.send(()).unwrap();
        let _ = stale_sync.await.unwrap();
        assert!(
            chunks_for(&db_path, &path_str)
                .await
                .iter()
                .all(|chunk| !chunk.text.contains("SECRETTOKEN"))
        );
    }

    #[tokio::test]
    async fn sync_path_stores_absolutized_key_for_relative_data_dir() {
        std::fs::create_dir_all("target").unwrap();
        let tmp = TempDir::new_in("target").unwrap();
        let cwd = std::env::current_dir().unwrap();
        let abs_data = crate::allowlist::absolutize(tmp.path());
        let rel_data = abs_data
            .strip_prefix(&cwd)
            .expect("temp data dir should be under the test cwd")
            .to_path_buf();
        assert!(!rel_data.is_absolute());

        let db_path = tmp.path().join("memory.db");
        let relative_file = rel_data.join("MEMORY.md");
        std::fs::write(&relative_file, "relative data dir memory").unwrap();

        let manager = file_backed_manager(
            &db_path,
            rel_data.clone(),
            Box::new(NamedEmbedder { name: "mock-model" }),
        )
        .await;
        manager.sync_path(&relative_file).await.unwrap();

        let expected = crate::allowlist::absolutize(&relative_file)
            .to_string_lossy()
            .into_owned();
        assert!(indexed_file(&db_path, &expected).await.is_some());
        assert!(
            indexed_file(&db_path, &relative_file.to_string_lossy())
                .await
                .is_none()
        );
    }
}
