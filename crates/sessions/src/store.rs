use std::{
    fs::{self, File, OpenOptions},
    io::{BufRead, BufReader, Seek, Write, copy},
    path::PathBuf,
    sync::{Arc, Mutex, MutexGuard},
};

use {
    crate::{
        Error, PersistedMessage, Result, filter_ui_history,
        tail_cursor::{SessionFileStamp, SessionTailRegistry, SessionTailState, scan_tail},
    },
    fd_lock::RwLock,
    serde::{Deserialize, Serialize},
};

/// How to locate a user message that starts a session-tail mutation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UserMessageTarget {
    /// Zero-based physical JSONL message index.
    MessageIndex(usize),
    /// Client-assigned user message sequence number.
    ClientSeq(u64),
}

/// Result of truncating a session from a selected user message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TruncateTailResult {
    pub target_index: usize,
    pub kept_count: usize,
    pub removed_count: usize,
    pub pruned_media_count: usize,
}

/// A single search hit within a session.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchResult {
    pub session_key: String,
    pub snippet: String,
    pub role: String,
    pub message_index: usize,
}

/// Append-only JSONL session storage with file locking.
pub struct SessionStore {
    pub base_dir: PathBuf,
    tail_registry: Arc<SessionTailRegistry>,
}

#[must_use]
fn slice_on_char_boundaries(content: &str, start: usize, end: usize) -> &str {
    let bounded_start = content.floor_char_boundary(start.min(content.len()));
    let bounded_end = content.floor_char_boundary(end.min(content.len()));
    if bounded_start >= bounded_end {
        return "";
    }
    &content[bounded_start..bounded_end]
}

impl SessionStore {
    pub fn new(base_dir: PathBuf) -> Self {
        Self {
            base_dir,
            tail_registry: Arc::new(SessionTailRegistry::new()),
        }
    }

    /// Sanitize a session key for use as a filename.
    pub fn key_to_filename(key: &str) -> String {
        key.replace(':', "_")
    }

    fn path_for(&self, key: &str) -> PathBuf {
        self.base_dir
            .join(format!("{}.jsonl", Self::key_to_filename(key)))
    }

    fn tail_state_for(
        &self,
        key: &str,
        path: &std::path::Path,
    ) -> Result<Arc<Mutex<SessionTailState>>> {
        self.tail_registry.session_state(path).inspect_err(|error| {
            tracing::error!(session_key = key, %error, "failed to access session tail state");
        })
    }

    /// Directory for session media files (screenshots, audio, etc.).
    fn media_dir_for(&self, key: &str) -> PathBuf {
        self.base_dir.join("media").join(Self::key_to_filename(key))
    }

    /// Absolute path for a session media file.
    pub fn media_path_for(&self, key: &str, filename: &str) -> PathBuf {
        self.media_dir_for(key).join(filename)
    }

    /// Save a media file for a session. Returns the relative path from base_dir.
    pub async fn save_media(&self, key: &str, filename: &str, data: &[u8]) -> Result<String> {
        let dir = self.media_dir_for(key);
        let file_path = self.media_path_for(key, filename);
        let data = data.to_vec();

        tokio::task::spawn_blocking(move || -> Result<()> {
            fs::create_dir_all(&dir)?;
            fs::write(&file_path, &data)?;
            Ok(())
        })
        .await??;

        let sanitized = Self::key_to_filename(key);
        Ok(format!("media/{sanitized}/{filename}"))
    }

    /// Read a media file. Returns raw bytes.
    pub async fn read_media(&self, key: &str, filename: &str) -> Result<Vec<u8>> {
        let file_path = self.media_path_for(key, filename);

        tokio::task::spawn_blocking(move || -> Result<Vec<u8>> {
            let data = fs::read(&file_path)?;
            Ok(data)
        })
        .await?
    }

    /// Append a message (JSON value) as a single line to the session file.
    pub async fn append(&self, key: &str, message: &serde_json::Value) -> Result<()> {
        self.append_serializable(key, std::slice::from_ref(message), None)
            .await
            .map(|_| ())
    }

    /// Append a message and return its zero-based physical JSONL index.
    ///
    /// The existing line count and append share one exclusive file lock, so
    /// the returned index always identifies the record written by this call.
    pub async fn append_with_index(&self, key: &str, message: &serde_json::Value) -> Result<usize> {
        self.append_with_expected_index(key, std::slice::from_ref(message), None)
            .await
    }

    /// Append a message only when its expected zero-based physical index still
    /// matches the session tail. The check and write share one exclusive lock.
    pub async fn append_at_index(
        &self,
        key: &str,
        message: &serde_json::Value,
        expected_index: usize,
    ) -> Result<usize> {
        self.append_with_expected_index(key, std::slice::from_ref(message), Some(expected_index))
            .await
    }

    /// Append several messages as one unit and return the index of the first.
    ///
    /// The tail check and every write share a single exclusive file lock, and
    /// the batch is serialized before the lock is taken. A turn that leads with
    /// replayed queue prompts therefore never leaves a partially persisted
    /// prefix behind: either the whole batch lands in history, or none of it.
    pub async fn append_batch_at_index(
        &self,
        key: &str,
        messages: &[serde_json::Value],
        expected_index: usize,
    ) -> Result<usize> {
        self.append_with_expected_index(key, messages, Some(expected_index))
            .await
    }

    async fn append_with_expected_index(
        &self,
        key: &str,
        messages: &[serde_json::Value],
        expected_index: Option<usize>,
    ) -> Result<usize> {
        self.append_serializable(key, messages, expected_index)
            .await
    }

    async fn append_serializable<T>(
        &self,
        key: &str,
        messages: &[T],
        expected_index: Option<usize>,
    ) -> Result<usize>
    where
        T: Clone + Send + Serialize + 'static,
    {
        let messages = messages.to_vec();
        let path = self.path_for(key);
        let session_key = key.to_string();
        let registry = Arc::clone(&self.tail_registry);

        tokio::task::spawn_blocking(move || -> Result<usize> {
            let batch = serialize_batch(&messages).inspect_err(|error| {
                tracing::error!(%session_key, %error, "failed to serialize session append");
            })?;
            let tail_state = registry.session_state(&path).inspect_err(|error| {
                tracing::error!(%session_key, %error, "failed to access session tail state");
            })?;
            let mut tail = lock_tail_state(&tail_state, &registry, &path, &session_key)?;
            let result =
                append_serialized_locked(&path, &batch, expected_index, &registry, &mut tail);
            match result {
                Ok(message_index) => Ok(message_index),
                Err(AppendFailure::ExpectedIndex(error)) => {
                    tracing::warn!(%session_key, %error, "session append tail check failed");
                    Err(error)
                },
                Err(AppendFailure::InvalidatesCursor(error)) => {
                    tail.invalidate();
                    tracing::error!(%session_key, %error, "session append failed");
                    Err(error)
                },
            }
        })
        .await?
    }

    /// Read all messages from a session file.
    pub async fn read(&self, key: &str) -> Result<Vec<serde_json::Value>> {
        let path = self.path_for(key);

        tokio::task::spawn_blocking(move || -> Result<Vec<serde_json::Value>> {
            if !path.exists() {
                return Ok(vec![]);
            }
            let file = File::open(&path)?;
            let reader = BufReader::new(file);
            let mut messages = Vec::new();
            for line in reader.lines() {
                let line = line?;
                let trimmed = line.trim();
                if trimmed.is_empty() {
                    continue;
                }
                match serde_json::from_str(trimmed) {
                    Ok(val) => messages.push(val),
                    Err(e) => {
                        tracing::warn!("skipping malformed JSONL line: {e}");
                    },
                }
            }
            Ok(messages)
        })
        .await?
    }

    /// Count the compact UI history entities for session metadata.
    pub async fn ui_message_count(&self, key: &str) -> Result<u32> {
        let count = crate::count_rendered_bubbles(&filter_ui_history(self.read(key).await?)?);
        u32::try_from(count)
            .map_err(|error| Error::message(format!("UI message count exceeds u32: {error}")))
    }

    /// Read all messages from a session that match a given `run_id`.
    pub async fn read_by_run_id(&self, key: &str, run_id: &str) -> Result<Vec<serde_json::Value>> {
        let mut matching = Vec::new();
        for message in self.read(key).await? {
            let persisted = serde_json::from_value::<PersistedMessage>(message.clone())?;
            if persisted.run_id() == Some(run_id) {
                matching.push(message);
            }
        }
        Ok(matching)
    }

    /// Read the last N messages from a session file.
    pub async fn read_last_n(&self, key: &str, n: usize) -> Result<Vec<serde_json::Value>> {
        let path = self.path_for(key);

        tokio::task::spawn_blocking(move || -> Result<Vec<serde_json::Value>> {
            if !path.exists() {
                return Ok(vec![]);
            }
            let file = File::open(&path)?;
            let reader = BufReader::new(file);
            let mut all: Vec<serde_json::Value> = Vec::new();
            for line in reader.lines() {
                let line = line?;
                let trimmed = line.trim();
                if trimmed.is_empty() {
                    continue;
                }
                if let Ok(val) = serde_json::from_str(trimmed) {
                    all.push(val);
                }
            }
            let start = all.len().saturating_sub(n);
            Ok(all[start..].to_vec())
        })
        .await?
    }

    /// Delete the session file, its media directory, and its persisted tool results.
    pub async fn clear(&self, key: &str) -> Result<()> {
        let path = self.path_for(key);
        let media_dir = self.media_dir_for(key);
        let tool_results_dir =
            crate::tool_results::ToolResultStore::new(self.base_dir.clone()).session_dir(key);
        let session_key = key.to_string();
        let registry = Arc::clone(&self.tail_registry);
        let tail_state = self.tail_state_for(key, &path)?;

        tokio::task::spawn_blocking(move || -> Result<()> {
            let mut tail = lock_tail_state(&tail_state, &registry, &path, &session_key)?;
            let result = (|| -> Result<()> {
                match OpenOptions::new().read(true).write(true).open(&path) {
                    Ok(file) => {
                        let mut lock = RwLock::new(file);
                        let _guard = lock
                            .write()
                            .map_err(|error| Error::lock_failed(error.to_string()))?;
                        fs::remove_file(&path)?;
                    },
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {},
                    Err(error) => return Err(error.into()),
                }
                // Deleting this parent-session media directory also breaks any fork that still
                // references the same paths; forks need containerized media snapshots.
                match fs::remove_dir_all(&media_dir) {
                    Ok(()) => {},
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {},
                    Err(error) => return Err(error.into()),
                }
                match fs::remove_dir_all(&tool_results_dir) {
                    Ok(()) => {},
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {},
                    Err(error) => return Err(error.into()),
                }
                Ok(())
            })();
            tail.invalidate();
            drop(tail);
            registry.remove(&path, &tail_state)?;
            if let Err(error) = &result {
                tracing::error!(%session_key, %error, "failed to clear session history");
            }
            result
        })
        .await??;

        Ok(())
    }

    /// List all session keys by scanning JSONL files in the base directory.
    pub fn list_keys(&self) -> Vec<String> {
        let Ok(entries) = fs::read_dir(&self.base_dir) else {
            return vec![];
        };
        entries
            .filter_map(|e| e.ok())
            .filter_map(|e| {
                let name = e.file_name().to_string_lossy().to_string();
                name.strip_suffix(".jsonl").map(|s| s.replace('_', ":"))
            })
            .collect()
    }

    /// Search all sessions for messages containing `query` (case-insensitive).
    /// Returns up to `max_results` hits, at most one per session.
    pub async fn search(&self, query: &str, max_results: usize) -> Result<Vec<SearchResult>> {
        let base = self.base_dir.clone();
        let query = query.to_lowercase();

        tokio::task::spawn_blocking(move || {
            let mut results = Vec::new();
            let entries = fs::read_dir(&base)?;

            for entry in entries.flatten() {
                if results.len() >= max_results {
                    break;
                }
                let path = entry.path();
                let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
                    continue;
                };
                let Some(key_raw) = name.strip_suffix(".jsonl") else {
                    continue;
                };
                let session_key = key_raw.replace('_', ":");

                let Ok(file) = File::open(&path) else {
                    continue;
                };
                let reader = BufReader::new(file);
                for (idx, line) in reader.lines().enumerate() {
                    let Ok(line) = line else {
                        continue;
                    };
                    let trimmed = line.trim();
                    if trimmed.is_empty() {
                        continue;
                    }
                    let Ok(val) = serde_json::from_str::<serde_json::Value>(trimmed) else {
                        continue;
                    };
                    let content = val.get("content").and_then(|v| v.as_str()).unwrap_or("");
                    if content.to_lowercase().contains(&query) {
                        let role = val
                            .get("role")
                            .and_then(|v| v.as_str())
                            .unwrap_or("unknown")
                            .to_string();

                        // Build a snippet: find the match position and extract context.
                        let lower = content.to_lowercase();
                        let pos = lower.find(&query).unwrap_or(0);
                        let start = pos.saturating_sub(40);
                        let end = pos.saturating_add(query.len()).saturating_add(60);
                        let snippet = slice_on_char_boundaries(content, start, end).to_string();

                        results.push(SearchResult {
                            session_key: session_key.clone(),
                            snippet,
                            role,
                            message_index: idx,
                        });
                        // One hit per session is enough for autocomplete.
                        break;
                    }
                }
            }

            Ok(results)
        })
        .await?
    }

    /// Replace the entire session history with the given messages.
    pub async fn replace_history(&self, key: &str, messages: Vec<serde_json::Value>) -> Result<()> {
        self.replace_serializable(key, messages).await
    }

    /// Read all messages as typed [`PersistedMessage`] values.
    ///
    /// Lines that fail to deserialize into `PersistedMessage` are skipped
    /// (with a warning), matching the behavior of [`read`].
    pub async fn read_typed(&self, key: &str) -> Result<Vec<PersistedMessage>> {
        let path = self.path_for(key);

        tokio::task::spawn_blocking(move || -> Result<Vec<PersistedMessage>> {
            if !path.exists() {
                return Ok(vec![]);
            }
            let file = File::open(&path)?;
            let reader = BufReader::new(file);
            let mut messages = Vec::new();
            for line in reader.lines() {
                let line = line?;
                let trimmed = line.trim();
                if trimmed.is_empty() {
                    continue;
                }
                match serde_json::from_str(trimmed) {
                    Ok(msg) => messages.push(msg),
                    Err(e) => {
                        tracing::warn!("skipping malformed JSONL line (typed): {e}");
                    },
                }
            }
            Ok(messages)
        })
        .await?
    }

    /// Read the last N messages as typed [`PersistedMessage`] values.
    pub async fn read_last_n_typed(&self, key: &str, n: usize) -> Result<Vec<PersistedMessage>> {
        let path = self.path_for(key);

        tokio::task::spawn_blocking(move || -> Result<Vec<PersistedMessage>> {
            if !path.exists() {
                return Ok(vec![]);
            }
            let file = File::open(&path)?;
            let reader = BufReader::new(file);
            let mut all: Vec<PersistedMessage> = Vec::new();
            for line in reader.lines() {
                let line = line?;
                let trimmed = line.trim();
                if trimmed.is_empty() {
                    continue;
                }
                if let Ok(msg) = serde_json::from_str(trimmed) {
                    all.push(msg);
                }
            }
            let start = all.len().saturating_sub(n);
            Ok(all[start..].to_vec())
        })
        .await?
    }

    /// Replace the entire session history with typed messages.
    pub async fn replace_history_typed(
        &self,
        key: &str,
        messages: &[PersistedMessage],
    ) -> Result<()> {
        let path = self.path_for(key);
        let session_key = key.to_string();
        let registry = Arc::clone(&self.tail_registry);
        let (sender, receiver) = tokio::sync::mpsc::channel(1);
        let task_session_key = session_key.clone();
        let task = tokio::task::spawn_blocking(move || {
            replace_typed_from_receiver(path, registry, receiver).inspect_err(|error| {
                tracing::error!(
                    session_key = task_session_key,
                    %error,
                    "failed to replace typed session history"
                );
            })
        });

        for message in messages {
            if sender
                .send(TypedHistoryStage::Message(Box::new(message.clone())))
                .await
                .is_err()
            {
                drop(sender);
                return task.await?;
            }
        }
        if sender.send(TypedHistoryStage::Complete).await.is_err() {
            drop(sender);
            return task.await?;
        }
        drop(sender);
        task.await?
    }

    async fn replace_serializable<T>(&self, key: &str, messages: Vec<T>) -> Result<()>
    where
        T: Send + Serialize + 'static,
    {
        let path = self.path_for(key);
        let session_key = key.to_string();
        let registry = Arc::clone(&self.tail_registry);

        tokio::task::spawn_blocking(move || -> Result<()> {
            let mut staged = stage_history(&path, &messages).inspect_err(|error| {
                tracing::error!(%session_key, %error, "failed to serialize session history");
            })?;
            let tail_state = registry.session_state(&path).inspect_err(|error| {
                tracing::error!(%session_key, %error, "failed to access session tail state");
            })?;
            let mut tail = lock_tail_state(&tail_state, &registry, &path, &session_key)?;
            let result = replace_from_staged_locked(&path, &mut staged, messages.len(), &mut tail);
            if let Err(error) = &result {
                tail.invalidate();
                tracing::error!(%session_key, %error, "failed to replace session history");
            }
            result
        })
        .await?
    }

    /// Truncate a session from a selected user message, removing that message
    /// and every subsequent message in the JSONL file.
    pub async fn truncate_from_user_message(
        &self,
        key: &str,
        target: UserMessageTarget,
    ) -> Result<TruncateTailResult> {
        let path = self.path_for(key);
        let media_dir = self.media_dir_for(key);
        let key_filename = Self::key_to_filename(key);
        let session_key = key.to_string();
        let registry = Arc::clone(&self.tail_registry);
        let tail_state = self.tail_state_for(key, &path)?;

        tokio::task::spawn_blocking(move || -> Result<TruncateTailResult> {
            let mut tail = lock_tail_state(&tail_state, &registry, &path, &session_key)?;
            let result = (|| -> Result<TruncateTailResult> {
                let file = OpenOptions::new()
                    .read(true)
                    .write(true)
                    .truncate(false)
                    .open(&path)
                    .map_err(|error| {
                        if error.kind() == std::io::ErrorKind::NotFound {
                            Error::message(format!("session '{session_key}' not found"))
                        } else {
                            error.into()
                        }
                    })?;
                let mut lock = RwLock::new(file);
                let mut guard = lock
                    .write()
                    .map_err(|error| Error::lock_failed(error.to_string()))?;

                let mut messages = read_messages_from_file(&mut guard)?;
                let original_count = messages.len();
                let target_index = find_user_message_target(&messages, target)?;
                let retained_media =
                    collect_session_media_refs(&messages[..target_index], &key_filename);
                let removed_count = original_count.saturating_sub(target_index);
                messages.truncate(target_index);

                guard.set_len(0)?;
                guard.rewind()?;
                for msg in &messages {
                    let line = serde_json::to_string(msg)?;
                    writeln!(*guard, "{line}")?;
                }
                guard.flush()?;
                let post_write_stamp = SessionFileStamp::read(&guard)?;
                tail.set(messages.len(), post_write_stamp);

                // This only checks media references retained in the current session file.
                // Forks may still reference parent media until fork snapshots get their
                // own containerized media copy without prompt-cache-sensitive URL rewrites.
                let pruned_media_count = prune_unreferenced_media(&media_dir, &retained_media)?;

                Ok(TruncateTailResult {
                    target_index,
                    kept_count: messages.len(),
                    removed_count,
                    pruned_media_count,
                })
            })();
            if let Err(error) = &result {
                tail.invalidate();
                tracing::error!(%session_key, %error, "failed to truncate session history");
            }
            result
        })
        .await?
    }

    /// Append a typed message to the session file.
    pub async fn append_typed(&self, key: &str, message: &PersistedMessage) -> Result<()> {
        self.append(key, &message.to_value()).await
    }

    /// Update one typed message by its zero-based history index.
    ///
    /// The entire read-modify-write operation is performed while holding the
    /// session file lock, so callers can safely finalize metadata on an
    /// already-persisted assistant segment without changing its position.
    pub async fn update_typed_at<F>(
        &self,
        key: &str,
        message_index: usize,
        update: F,
    ) -> Result<PersistedMessage>
    where
        F: FnOnce(PersistedMessage) -> PersistedMessage + Send + 'static,
    {
        let path = self.path_for(key);
        let session_key = key.to_string();
        let registry = Arc::clone(&self.tail_registry);
        let tail_state = self.tail_state_for(key, &path)?;

        tokio::task::spawn_blocking(move || -> Result<PersistedMessage> {
            let mut tail = lock_tail_state(&tail_state, &registry, &path, &session_key)?;
            let result = (|| -> Result<PersistedMessage> {
                let Some(parent) = path.parent() else {
                    return Err(Error::message(path.display().to_string()));
                };
                fs::create_dir_all(parent)?;

                let file = OpenOptions::new().read(true).write(true).open(&path)?;
                let mut lock = RwLock::new(file);
                let mut guard = lock
                    .write()
                    .map_err(|error| Error::lock_failed(error.to_string()))?;
                let mut lines: Vec<String> = BufReader::new(&*guard)
                    .lines()
                    .collect::<std::io::Result<Vec<_>>>()?
                    .into_iter()
                    .filter(|line| !line.trim().is_empty())
                    .collect();
                let line_count = lines.len();
                if message_index >= lines.len() {
                    return Err(Error::message(format!(
                        "message index {message_index} is outside session history"
                    )));
                }
                let existing = serde_json::from_str(&lines[message_index])?;
                let updated = update(existing);
                lines[message_index] = serde_json::to_string(&updated.to_value())?;

                guard.set_len(0)?;
                guard.rewind()?;
                for line in lines {
                    writeln!(*guard, "{line}")?;
                }
                guard.flush()?;
                let post_write_stamp = SessionFileStamp::read(&guard)?;
                tail.set(line_count, post_write_stamp);
                Ok(updated)
            })();
            if let Err(error) = &result {
                tail.invalidate();
                tracing::error!(%session_key, %error, "failed to update typed session message");
            }
            result
        })
        .await?
    }

    /// Count messages in a session file without parsing them.
    pub async fn count(&self, key: &str) -> Result<u32> {
        let path = self.path_for(key);

        tokio::task::spawn_blocking(move || -> Result<u32> {
            if !path.exists() {
                return Ok(0);
            }
            let file = File::open(&path)?;
            let reader = BufReader::new(file);
            let count = reader
                .lines()
                .map_while(std::result::Result::ok)
                .filter(|l| !l.trim().is_empty())
                .count();
            Ok(count as u32)
        })
        .await?
    }
}

struct SerializedBatch {
    bytes: Vec<u8>,
    record_count: usize,
}

enum AppendFailure {
    ExpectedIndex(Error),
    InvalidatesCursor(Error),
}

enum TypedHistoryStage {
    Message(Box<PersistedMessage>),
    Complete,
}

fn lock_tail_state<'a>(
    tail_state: &'a Arc<Mutex<SessionTailState>>,
    registry: &SessionTailRegistry,
    path: &std::path::Path,
    session_key: &str,
) -> Result<MutexGuard<'a, SessionTailState>> {
    match tail_state.lock() {
        Ok(tail) => Ok(tail),
        Err(error) => {
            let error = Error::lock_failed(error.to_string());
            if let Err(remove_error) = registry.remove(path, tail_state) {
                tracing::error!(
                    %session_key,
                    %remove_error,
                    "failed to remove inaccessible session tail state"
                );
            }
            tracing::error!(%session_key, %error, "failed to lock session tail state");
            Err(error)
        },
    }
}

fn serialize_batch<T>(messages: &[T]) -> Result<SerializedBatch>
where
    T: Serialize,
{
    let mut bytes = Vec::new();
    for message in messages {
        serde_json::to_writer(&mut bytes, message)?;
        bytes.push(b'\n');
    }
    Ok(SerializedBatch {
        bytes,
        record_count: messages.len(),
    })
}

fn append_serialized_locked(
    path: &std::path::Path,
    batch: &SerializedBatch,
    expected_index: Option<usize>,
    registry: &SessionTailRegistry,
    tail: &mut SessionTailState,
) -> std::result::Result<usize, AppendFailure> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(Error::from)
            .map_err(AppendFailure::InvalidatesCursor)?;
    }
    let file = OpenOptions::new()
        .create(true)
        .read(true)
        .append(true)
        .open(path)
        .map_err(Error::from)
        .map_err(AppendFailure::InvalidatesCursor)?;
    let mut lock = RwLock::new(file);
    #[cfg(test)]
    if registry.take_file_lock_failure() {
        return Err(AppendFailure::InvalidatesCursor(Error::lock_failed(
            "injected session file lock failure",
        )));
    }
    let mut guard = lock
        .write()
        .map_err(|error| Error::lock_failed(error.to_string()))
        .map_err(AppendFailure::InvalidatesCursor)?;
    #[cfg(test)]
    if registry.take_metadata_failure() {
        return Err(AppendFailure::InvalidatesCursor(Error::message(
            "injected session metadata failure",
        )));
    }
    let pre_write_stamp =
        SessionFileStamp::read(&guard).map_err(AppendFailure::InvalidatesCursor)?;
    let message_index = match &tail.cursor {
        Some(cursor) if cursor.file_stamp == pre_write_stamp => cursor.next_index,
        _ => {
            let message_index =
                scan_tail(&mut guard, registry).map_err(AppendFailure::InvalidatesCursor)?;
            tail.set(message_index, pre_write_stamp.clone());
            message_index
        },
    };

    if let Some(expected_index) = expected_index
        && message_index != expected_index
    {
        return Err(AppendFailure::ExpectedIndex(Error::message(format!(
            "expected message index {expected_index}, found session tail {message_index}"
        ))));
    }

    let next_index = message_index
        .checked_add(batch.record_count)
        .ok_or_else(|| {
            AppendFailure::InvalidatesCursor(Error::message("session message index overflow"))
        })?;
    write_append_batch(&mut guard, &batch.bytes, registry)
        .map_err(AppendFailure::InvalidatesCursor)?;
    guard
        .flush()
        .map_err(Error::from)
        .map_err(AppendFailure::InvalidatesCursor)?;
    let post_write_stamp =
        SessionFileStamp::read(&guard).map_err(AppendFailure::InvalidatesCursor)?;
    tail.set(next_index, post_write_stamp);
    Ok(message_index)
}

fn write_append_batch(file: &mut File, bytes: &[u8], registry: &SessionTailRegistry) -> Result<()> {
    #[cfg(test)]
    if registry.take_write_failure() {
        let partial_len = bytes.len().saturating_sub(1);
        file.write_all(&bytes[..partial_len])?;
        return Err(Error::message("injected session append write failure"));
    }

    #[cfg(not(test))]
    let _ = registry;
    file.write_all(bytes)?;
    Ok(())
}

fn stage_history<T>(path: &std::path::Path, messages: &[T]) -> Result<tempfile::NamedTempFile>
where
    T: Serialize,
{
    let parent = path
        .parent()
        .ok_or_else(|| Error::message(path.display().to_string()))?;
    fs::create_dir_all(parent)?;
    let mut staged = tempfile::NamedTempFile::new_in(parent)?;
    for message in messages {
        serde_json::to_writer(staged.as_file_mut(), message)?;
        staged.as_file_mut().write_all(b"\n")?;
    }
    staged.as_file_mut().flush()?;
    Ok(staged)
}

fn replace_typed_from_receiver(
    path: PathBuf,
    registry: Arc<SessionTailRegistry>,
    mut receiver: tokio::sync::mpsc::Receiver<TypedHistoryStage>,
) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| Error::message(path.display().to_string()))?;
    fs::create_dir_all(parent)?;
    let mut staged = tempfile::NamedTempFile::new_in(parent)?;
    let mut record_count = 0_usize;
    loop {
        match receiver.blocking_recv() {
            Some(TypedHistoryStage::Message(message)) => {
                serde_json::to_writer(staged.as_file_mut(), message.as_ref())?;
                staged.as_file_mut().write_all(b"\n")?;
                record_count = record_count
                    .checked_add(1)
                    .ok_or_else(|| Error::message("session message index overflow"))?;
            },
            Some(TypedHistoryStage::Complete) => break,
            None => {
                return Err(Error::message(
                    "typed session history stream closed before completion",
                ));
            },
        }
    }
    staged.as_file_mut().flush()?;

    let tail_state = registry.session_state(&path)?;
    let session_key = path.display().to_string();
    let mut tail = lock_tail_state(&tail_state, &registry, &path, &session_key)?;
    let result = replace_from_staged_locked(&path, &mut staged, record_count, &mut tail);
    if result.is_err() {
        tail.invalidate();
    }
    result
}

fn replace_from_staged_locked(
    path: &std::path::Path,
    staged: &mut tempfile::NamedTempFile,
    record_count: usize,
    tail: &mut SessionTailState,
) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let file = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(path)?;
    let mut lock = RwLock::new(file);
    let mut guard = lock
        .write()
        .map_err(|error| Error::lock_failed(error.to_string()))?;
    staged.as_file_mut().rewind()?;
    guard.set_len(0)?;
    guard.rewind()?;
    copy(staged.as_file_mut(), &mut *guard)?;
    guard.flush()?;
    let post_write_stamp = SessionFileStamp::read(&guard)?;
    tail.set(record_count, post_write_stamp);
    Ok(())
}

fn read_messages_from_file(file: &mut File) -> Result<Vec<serde_json::Value>> {
    file.rewind()?;
    let mut messages = Vec::new();
    {
        let reader = BufReader::new(&mut *file);
        for line in reader.lines() {
            let line = line?;
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            match serde_json::from_str(trimmed) {
                Ok(val) => messages.push(val),
                Err(e) => {
                    tracing::warn!("skipping malformed JSONL line while truncating: {e}");
                },
            }
        }
    }
    file.rewind()?;
    Ok(messages)
}

fn find_user_message_target(
    messages: &[serde_json::Value],
    target: UserMessageTarget,
) -> Result<usize> {
    let index = match target {
        UserMessageTarget::MessageIndex(idx) => {
            if idx >= messages.len() {
                return Err(Error::message(format!(
                    "messageIndex {idx} exceeds message count {}",
                    messages.len()
                )));
            }
            idx
        },
        UserMessageTarget::ClientSeq(seq) => messages
            .iter()
            .position(|msg| {
                msg.get("role").and_then(|v| v.as_str()) == Some("user")
                    && msg.get("seq").and_then(|v| v.as_u64()) == Some(seq)
            })
            .ok_or_else(|| Error::message(format!("user message with seq {seq} not found")))?,
    };

    let role = messages[index].get("role").and_then(|v| v.as_str());
    if role != Some("user") {
        return Err(Error::message(format!(
            "message at index {index} is not a user message"
        )));
    }
    Ok(index)
}

fn collect_session_media_refs(
    messages: &[serde_json::Value],
    key_filename: &str,
) -> std::collections::HashSet<String> {
    let prefix = format!("media/{key_filename}/");
    let mut refs = std::collections::HashSet::new();
    for message in messages {
        collect_media_refs_from_value(message, &prefix, &mut refs);
    }
    refs
}

fn collect_media_refs_from_value(
    value: &serde_json::Value,
    prefix: &str,
    refs: &mut std::collections::HashSet<String>,
) {
    match value {
        serde_json::Value::String(text) => {
            if let Some(rest) = text.strip_prefix(prefix)
                && !rest.is_empty()
                && !rest.contains('/')
            {
                refs.insert(rest.to_string());
            }
        },
        serde_json::Value::Array(items) => {
            for item in items {
                collect_media_refs_from_value(item, prefix, refs);
            }
        },
        serde_json::Value::Object(map) => {
            for value in map.values() {
                collect_media_refs_from_value(value, prefix, refs);
            }
        },
        _ => {},
    }
}

fn prune_unreferenced_media(
    media_dir: &std::path::Path,
    retained_media: &std::collections::HashSet<String>,
) -> Result<usize> {
    let entries = match fs::read_dir(media_dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(error) => return Err(error.into()),
    };
    let mut pruned = 0;
    for entry in entries {
        let entry = entry?;
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let Some(file_name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if retained_media.contains(file_name) {
            continue;
        }
        // Deleting this parent-session media file also breaks any fork that still
        // references the same path; forks need containerized media snapshots.
        match fs::remove_file(&path) {
            Ok(()) => pruned += 1,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {},
            Err(error) => return Err(error.into()),
        }
    }
    Ok(pruned)
}

#[allow(clippy::unwrap_used, clippy::expect_used)]
#[cfg(test)]
mod tests {
    use {
        super::*,
        serde::{Serializer, ser::Error as _},
        serde_json::json,
        std::sync::{
            atomic::{AtomicBool, Ordering},
            mpsc,
        },
    };

    fn temp_store() -> (SessionStore, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let store = SessionStore::new(dir.path().to_path_buf());
        (store, dir)
    }

    #[derive(Clone)]
    struct FailingSerialize;

    impl Serialize for FailingSerialize {
        fn serialize<S>(&self, _serializer: S) -> std::result::Result<S::Ok, S::Error>
        where
            S: Serializer,
        {
            Err(S::Error::custom("injected serialization failure"))
        }
    }

    #[derive(Clone)]
    struct LockCheckingSerialize {
        path: PathBuf,
        observed_unlocked: Arc<AtomicBool>,
    }

    impl Serialize for LockCheckingSerialize {
        fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
        where
            S: Serializer,
        {
            let file = OpenOptions::new()
                .read(true)
                .write(true)
                .open(&self.path)
                .map_err(S::Error::custom)?;
            let mut lock = RwLock::new(file);
            let _guard = lock.try_write().map_err(S::Error::custom)?;
            self.observed_unlocked.store(true, Ordering::Relaxed);
            json!({"record": "serialized-before-lock"}).serialize(serializer)
        }
    }

    #[test]
    fn slice_on_char_boundaries_handles_multibyte_boundary() {
        let content = format!("{}л{}", "a".repeat(39), "z".repeat(20));
        let snippet = slice_on_char_boundaries(&content, 0, 40);
        assert_eq!(snippet.len(), 39);
        assert!(snippet.chars().all(|c| c == 'a'));
    }

    #[tokio::test]
    async fn test_append_and_read() {
        let (store, _dir) = temp_store();

        store
            .append("main", &json!({"role": "user", "content": "hello"}))
            .await
            .unwrap();
        store
            .append("main", &json!({"role": "assistant", "content": "hi"}))
            .await
            .unwrap();

        let msgs = store.read("main").await.unwrap();
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[0]["role"], "user");
        assert_eq!(msgs[1]["role"], "assistant");
    }

    #[tokio::test]
    async fn test_read_empty() {
        let (store, _dir) = temp_store();
        let msgs = store.read("nonexistent").await.unwrap();
        assert!(msgs.is_empty());
    }

    #[tokio::test]
    async fn test_read_last_n() {
        let (store, _dir) = temp_store();

        for i in 0..10 {
            store.append("test", &json!({"i": i})).await.unwrap();
        }

        let last3 = store.read_last_n("test", 3).await.unwrap();
        assert_eq!(last3.len(), 3);
        assert_eq!(last3[0]["i"], 7);
        assert_eq!(last3[2]["i"], 9);
    }

    #[tokio::test]
    async fn test_clear() {
        let (store, _dir) = temp_store();

        store
            .append("main", &json!({"role": "user", "content": "hello"}))
            .await
            .unwrap();
        assert_eq!(store.read("main").await.unwrap().len(), 1);

        store.clear("main").await.unwrap();
        assert!(store.read("main").await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn test_count() {
        let (store, _dir) = temp_store();

        assert_eq!(store.count("main").await.unwrap(), 0);
        store
            .append("main", &json!({"role": "user"}))
            .await
            .unwrap();
        store
            .append("main", &json!({"role": "assistant"}))
            .await
            .unwrap();
        assert_eq!(store.count("main").await.unwrap(), 2);
    }

    #[tokio::test]
    async fn ui_message_count_compacts_tool_lifecycle_transitions() {
        let (store, _dir) = temp_store();
        let lifecycle_base = json!({
            "role": "tool_lifecycle",
            "toolCallId": "call-1",
            "toolName": "execute_command",
            "emittedAtMs": 1,
            "runId": "run-1"
        });

        store
            .append("main", &json!({"role": "user", "content": "run it"}))
            .await
            .unwrap();
        let mut created = lifecycle_base.clone();
        created["sequence"] = json!(1);
        created["stage"] = json!("created");
        created["providerIndex"] = json!(0);
        store.append("main", &created).await.unwrap();
        let mut completed = lifecycle_base;
        completed["sequence"] = json!(2);
        completed["stage"] = json!("completed");
        completed["arguments"] = json!({"command": "echo done"});
        completed["success"] = json!(true);
        completed["result"] = json!("{\"stdout\":\"done\",\"exitCode\":0}");
        completed["error"] = serde_json::Value::Null;
        store.append("main", &completed).await.unwrap();

        assert_eq!(store.count("main").await.unwrap(), 3);
        assert_eq!(store.ui_message_count("main").await.unwrap(), 2);
    }

    /// A streamed answer is many records but one message; the counter shown in
    /// the UI must agree with the number of bubbles, not with the record count.
    #[tokio::test]
    async fn ui_message_count_counts_a_streamed_answer_as_one_message() {
        let (store, _dir) = temp_store();

        store
            .append("main", &json!({"role": "user", "content": "hi"}))
            .await
            .unwrap();
        for sequence in 0..64 {
            store
                .append(
                    "main",
                    &json!({
                        "role": "provider_update",
                        "segmentId": "segment-1",
                        "update": {"sequence": sequence}
                    }),
                )
                .await
                .unwrap();
        }
        store
            .append(
                "main",
                &json!({"role": "provider_segment_close", "segmentId": "segment-1"}),
            )
            .await
            .unwrap();
        store
            .append(
                "main",
                &json!({
                    "role": "assistant",
                    "segmentId": "segment-1",
                    "content": "hello"
                }),
            )
            .await
            .unwrap();

        assert_eq!(store.count("main").await.unwrap(), 67);
        assert_eq!(store.ui_message_count("main").await.unwrap(), 2);
    }

    /// The count must follow the renderer in both directions.
    #[tokio::test]
    async fn ui_message_count_follows_what_the_renderer_produces() {
        let (store, _dir) = temp_store();

        // An attempt abandoned on retry: records exist, no assistant message was
        // ever written, yet the UI still renders the segment as one bubble.
        store
            .append(
                "main",
                &json!({
                    "role": "provider_update",
                    "segmentId": "abandoned",
                    "update": {"sequence": 0}
                }),
            )
            .await
            .unwrap();
        store
            .append(
                "main",
                &json!({"role": "provider_segment_close", "segmentId": "abandoned"}),
            )
            .await
            .unwrap();
        // A frame kept only for its tool calls renders no bubble of its own.
        store
            .append(
                "main",
                &json!({
                    "role": "assistant",
                    "content": "",
                    "tool_calls": [{"id": "call-1"}]
                }),
            )
            .await
            .unwrap();

        assert_eq!(store.ui_message_count("main").await.unwrap(), 1);
    }

    /// A persisted failure is shown to the user, so it counts as a message.
    #[tokio::test]
    async fn ui_message_count_includes_persisted_errors() {
        let (store, _dir) = temp_store();

        store
            .append("main", &json!({"role": "user", "content": "hi"}))
            .await
            .unwrap();
        store
            .append_typed("main", &PersistedMessage::system("[error] provider failed"))
            .await
            .unwrap();

        assert_eq!(store.ui_message_count("main").await.unwrap(), 2);
    }

    #[tokio::test]
    async fn read_by_run_id_matches_camel_case_tool_lifecycle_identity() {
        let (store, _dir) = temp_store();

        store
            .append(
                "main",
                &json!({"role": "user", "content": "run it", "run_id": "run-1"}),
            )
            .await
            .unwrap();
        store
            .append(
                "main",
                &json!({
                    "role": "tool_lifecycle",
                    "toolCallId": "call-1",
                    "toolName": "execute_command",
                    "sequence": 1,
                    "emittedAtMs": 1,
                    "runId": "run-1",
                    "stage": "completed",
                    "arguments": {"command": "echo done"},
                    "success": true,
                    "result": "{\"stdout\":\"done\",\"exitCode\":0}",
                    "error": null
                }),
            )
            .await
            .unwrap();
        store
            .append(
                "main",
                &json!({
                    "role": "tool_lifecycle",
                    "toolCallId": "call-2",
                    "toolName": "execute_command",
                    "sequence": 1,
                    "emittedAtMs": 1,
                    "runId": "run-2",
                    "stage": "created",
                    "providerIndex": 0
                }),
            )
            .await
            .unwrap();

        let messages = store.read_by_run_id("main", "run-1").await.unwrap();
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0]["role"], "user");
        assert_eq!(messages[1]["role"], "tool_lifecycle");
        assert_eq!(messages[1]["runId"], "run-1");
    }

    #[tokio::test]
    async fn test_search_matching() {
        let (store, _dir) = temp_store();

        store
            .append("s1", &json!({"role": "user", "content": "hello world"}))
            .await
            .unwrap();
        store
            .append("s1", &json!({"role": "assistant", "content": "hi there"}))
            .await
            .unwrap();
        store
            .append("s2", &json!({"role": "user", "content": "goodbye world"}))
            .await
            .unwrap();

        let results = store.search("hello", 10).await.unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].session_key, "s1");
        assert_eq!(results[0].role, "user");
        assert!(results[0].snippet.contains("hello"));
    }

    #[tokio::test]
    async fn test_search_case_insensitive() {
        let (store, _dir) = temp_store();

        store
            .append("s1", &json!({"role": "user", "content": "Hello World"}))
            .await
            .unwrap();

        let results = store.search("hello world", 10).await.unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].session_key, "s1");
    }

    #[tokio::test]
    async fn test_search_no_match() {
        let (store, _dir) = temp_store();

        store
            .append("s1", &json!({"role": "user", "content": "hello"}))
            .await
            .unwrap();

        let results = store.search("xyz", 10).await.unwrap();
        assert!(results.is_empty());
    }

    #[tokio::test]
    async fn test_search_empty_query() {
        let (store, _dir) = temp_store();

        store
            .append("s1", &json!({"role": "user", "content": "hello"}))
            .await
            .unwrap();

        // Empty query should match nothing (caller should guard against this)
        let results = store.search("", 10).await.unwrap();
        // Empty string is contained in every string, so it would match.
        // The frontend guards against empty queries, but the store doesn't — that's fine.
        assert!(!results.is_empty());
    }

    #[tokio::test]
    async fn test_search_across_sessions() {
        let (store, _dir) = temp_store();

        store
            .append("s1", &json!({"role": "user", "content": "rust is great"}))
            .await
            .unwrap();
        store
            .append(
                "s2",
                &json!({"role": "assistant", "content": "rust is awesome"}),
            )
            .await
            .unwrap();
        store
            .append("s3", &json!({"role": "user", "content": "python is nice"}))
            .await
            .unwrap();

        let results = store.search("rust", 10).await.unwrap();
        assert_eq!(results.len(), 2);
        let keys: Vec<&str> = results.iter().map(|r| r.session_key.as_str()).collect();
        assert!(keys.contains(&"s1"));
        assert!(keys.contains(&"s2"));
    }

    #[tokio::test]
    async fn test_search_max_results() {
        let (store, _dir) = temp_store();

        for i in 0..10 {
            let key = format!("s{i}");
            store
                .append(&key, &json!({"role": "user", "content": "common term"}))
                .await
                .unwrap();
        }

        let results = store.search("common", 3).await.unwrap();
        assert!(results.len() <= 3);
    }

    #[tokio::test]
    async fn test_replace_history() {
        let (store, _dir) = temp_store();

        store
            .append("main", &json!({"role": "user", "content": "hello"}))
            .await
            .unwrap();
        store
            .append("main", &json!({"role": "assistant", "content": "hi"}))
            .await
            .unwrap();
        assert_eq!(store.read("main").await.unwrap().len(), 2);

        let new_history = vec![json!({"role": "assistant", "content": "summary"})];
        store.replace_history("main", new_history).await.unwrap();

        let msgs = store.read("main").await.unwrap();
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0]["content"], "summary");
    }

    #[tokio::test]
    async fn test_replace_history_empty() {
        let (store, _dir) = temp_store();

        store
            .append("main", &json!({"role": "user", "content": "hello"}))
            .await
            .unwrap();

        store.replace_history("main", vec![]).await.unwrap();
        assert!(store.read("main").await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn test_key_sanitization() {
        let (store, _dir) = temp_store();

        store
            .append("session:abc-123", &json!({"role": "user"}))
            .await
            .unwrap();
        let msgs = store.read("session:abc-123").await.unwrap();
        assert_eq!(msgs.len(), 1);
    }

    #[tokio::test]
    async fn test_save_and_read_media() {
        let (store, _dir) = temp_store();
        let data = b"fake png data";

        let path = store.save_media("main", "call_1.png", data).await.unwrap();
        assert_eq!(path, "media/main/call_1.png");

        let read_back = store.read_media("main", "call_1.png").await.unwrap();
        assert_eq!(read_back, data);
    }

    #[tokio::test]
    async fn test_save_media_with_colon_key() {
        let (store, _dir) = temp_store();
        let data = b"screenshot bytes";

        let path = store
            .save_media("session:abc", "shot.png", data)
            .await
            .unwrap();
        assert_eq!(path, "media/session_abc/shot.png");

        let read_back = store.read_media("session:abc", "shot.png").await.unwrap();
        assert_eq!(read_back, data);
    }

    #[test]
    fn test_media_path_for_uses_session_media_dir() {
        let (store, dir) = temp_store();
        let path = store.media_path_for("session:abc", "report.pdf");
        assert_eq!(
            path,
            dir.path()
                .join("media")
                .join("session_abc")
                .join("report.pdf")
        );
    }

    #[tokio::test]
    async fn test_read_media_missing_file() {
        let (store, _dir) = temp_store();
        let result = store.read_media("main", "nonexistent.png").await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_clear_removes_media_dir() {
        let (store, dir) = temp_store();

        // Create a session and media.
        store
            .append("main", &json!({"role": "user", "content": "hello"}))
            .await
            .unwrap();
        store
            .save_media("main", "shot.png", b"img data")
            .await
            .unwrap();

        let media_dir = dir.path().join("media").join("main");
        assert!(media_dir.exists());

        store.clear("main").await.unwrap();

        assert!(!media_dir.exists());
        assert!(store.read("main").await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn test_truncate_from_user_message_removes_target_and_tail() {
        let (store, _dir) = temp_store();

        store
            .append("main", &json!({"role": "system", "content": "context"}))
            .await
            .unwrap();
        store
            .append(
                "main",
                &json!({"role": "user", "content": "remove me", "seq": 7}),
            )
            .await
            .unwrap();
        store
            .append("main", &json!({"role": "assistant", "content": "tail"}))
            .await
            .unwrap();
        store
            .append(
                "main",
                &json!({
                    "role": "tool_lifecycle",
                    "toolCallId": "call-tail",
                    "toolName": "execute_command",
                    "sequence": 1,
                    "emittedAtMs": 1,
                    "stage": "completed",
                    "arguments": {"command": "echo tail"},
                    "success": true,
                    "result": "{\"stdout\":\"tail\",\"exitCode\":0}",
                    "error": null
                }),
            )
            .await
            .unwrap();

        let result = store
            .truncate_from_user_message("main", UserMessageTarget::MessageIndex(1))
            .await
            .unwrap();

        assert_eq!(result.target_index, 1);
        assert_eq!(result.kept_count, 1);
        assert_eq!(result.removed_count, 3);
        let messages = store.read("main").await.unwrap();
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0]["role"], "system");
    }

    #[tokio::test]
    async fn test_truncate_from_user_message_can_target_client_seq() {
        let (store, _dir) = temp_store();

        store
            .append(
                "main",
                &json!({"role": "user", "content": "keep", "seq": 1}),
            )
            .await
            .unwrap();
        store
            .append("main", &json!({"role": "assistant", "content": "keep"}))
            .await
            .unwrap();
        store
            .append(
                "main",
                &json!({"role": "user", "content": "remove", "seq": 2}),
            )
            .await
            .unwrap();

        let result = store
            .truncate_from_user_message("main", UserMessageTarget::ClientSeq(2))
            .await
            .unwrap();

        assert_eq!(result.target_index, 2);
        assert_eq!(store.read("main").await.unwrap().len(), 2);
    }

    #[tokio::test]
    async fn test_truncate_from_user_message_rejects_non_user_target() {
        let (store, _dir) = temp_store();

        store
            .append("main", &json!({"role": "assistant", "content": "not user"}))
            .await
            .unwrap();

        let error = store
            .truncate_from_user_message("main", UserMessageTarget::MessageIndex(0))
            .await
            .unwrap_err();

        assert!(error.to_string().contains("not a user message"));
        assert_eq!(store.read("main").await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn test_truncate_from_user_message_rejects_out_of_range() {
        let (store, _dir) = temp_store();

        store
            .append("main", &json!({"role": "user", "content": "only"}))
            .await
            .unwrap();

        let error = store
            .truncate_from_user_message("main", UserMessageTarget::MessageIndex(5))
            .await
            .unwrap_err();

        assert!(error.to_string().contains("exceeds message count"));
        assert_eq!(store.read("main").await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn test_truncate_from_user_message_rejects_missing_session() {
        let (store, dir) = temp_store();

        let error = store
            .truncate_from_user_message("missing", UserMessageTarget::MessageIndex(0))
            .await
            .unwrap_err();

        assert!(error.to_string().contains("session 'missing' not found"));
        assert!(!dir.path().join("missing.jsonl").exists());
    }

    #[tokio::test]
    async fn test_truncate_from_user_message_prunes_unreferenced_media() {
        let (store, _dir) = temp_store();

        store.save_media("main", "keep.ogg", b"keep").await.unwrap();
        store
            .save_media("main", "remove.ogg", b"remove")
            .await
            .unwrap();
        store
            .append(
                "main",
                &json!({"role": "user", "content": "keep", "audio": "media/main/keep.ogg"}),
            )
            .await
            .unwrap();
        store
            .append(
                "main",
                &json!({"role": "user", "content": "remove", "audio": "media/main/remove.ogg"}),
            )
            .await
            .unwrap();

        let result = store
            .truncate_from_user_message("main", UserMessageTarget::MessageIndex(1))
            .await
            .unwrap();

        assert_eq!(result.pruned_media_count, 1);
        assert!(store.media_path_for("main", "keep.ogg").exists());
        assert!(!store.media_path_for("main", "remove.ogg").exists());
    }

    // --- Typed API tests ---

    #[tokio::test]
    async fn test_append_typed_and_read_typed() {
        use crate::message::PersistedMessage;

        let (store, _dir) = temp_store();

        store
            .append_typed("main", &PersistedMessage::user("hello"))
            .await
            .unwrap();
        store
            .append_typed(
                "main",
                &PersistedMessage::assistant("hi", "gpt-4o", "openai", 10, 5, None),
            )
            .await
            .unwrap();

        let msgs = store.read_typed("main").await.unwrap();
        assert_eq!(msgs.len(), 2);
        match &msgs[0] {
            PersistedMessage::User { content, .. } => {
                assert!(matches!(content, crate::message::MessageContent::Text(t) if t == "hello"));
            },
            _ => panic!("expected User message"),
        }
        match &msgs[1] {
            PersistedMessage::Assistant { content, model, .. } => {
                assert_eq!(content, "hi");
                assert_eq!(model.as_deref(), Some("gpt-4o"));
            },
            _ => panic!("expected Assistant message"),
        }
    }

    #[tokio::test]
    async fn test_append_with_index_returns_its_physical_history_index() {
        let (store, _dir) = temp_store();
        store
            .append("main", &json!({ "role": "user", "content": "before" }))
            .await
            .unwrap();
        store
            .append("main", &json!({ "role": "assistant", "content": "middle" }))
            .await
            .unwrap();

        let index = store
            .append_with_index("main", &json!({ "role": "assistant", "content": "final" }))
            .await
            .unwrap();

        assert_eq!(index, 2);
        let history = store.read("main").await.unwrap();
        assert_eq!(history.len(), 3);
        assert_eq!(history[index]["content"], "final");
    }

    #[tokio::test]
    async fn append_with_index_counts_existing_non_empty_jsonl_records() {
        let (store, dir) = temp_store();
        fs::write(
            dir.path().join("main.jsonl"),
            b"{\"record\":0}\n\n   \n{\"record\":1}\n",
        )
        .unwrap();

        let index = store
            .append_with_index("main", &json!({ "record": 2 }))
            .await
            .unwrap();

        assert_eq!(index, 2);
        assert_eq!(store.read("main").await.unwrap().len(), 3);
    }

    #[tokio::test]
    async fn append_with_index_returns_zero_for_an_empty_file() {
        let (store, dir) = temp_store();
        fs::write(dir.path().join("main.jsonl"), []).unwrap();

        let index = store
            .append_with_index("main", &json!({ "record": 0 }))
            .await
            .unwrap();

        assert_eq!(index, 0);
    }

    #[tokio::test]
    async fn append_with_index_recovers_one_hundred_existing_records() {
        let (store, dir) = temp_store();
        let history = (0..100)
            .map(|record| format!("{{\"record\":{record}}}\n"))
            .collect::<String>();
        fs::write(dir.path().join("main.jsonl"), history).unwrap();

        let index = store
            .append_with_index("main", &json!({ "record": 100 }))
            .await
            .unwrap();

        assert_eq!(index, 100);
    }

    #[tokio::test]
    async fn append_batch_at_index_returns_first_record_index() {
        let (store, _dir) = temp_store();
        store.append("main", &json!({ "record": 0 })).await.unwrap();

        let index = store
            .append_batch_at_index("main", &[json!({ "record": 1 }), json!({ "record": 2 })], 1)
            .await
            .unwrap();

        assert_eq!(index, 1);
        let history = store.read("main").await.unwrap();
        assert_eq!(history.len(), 3);
        assert_eq!(history[1]["record"], 1);
        assert_eq!(history[2]["record"], 2);
    }

    #[tokio::test]
    async fn append_indexes_remain_contiguous() {
        let (store, _dir) = temp_store();

        for expected_index in 0..32 {
            let index = store
                .append_with_index("main", &json!({ "record": expected_index }))
                .await
                .unwrap();
            assert_eq!(index, expected_index);
        }
    }

    #[tokio::test]
    async fn ordinary_append_advances_a_warm_indexed_tail() {
        let (store, _dir) = temp_store();
        assert_eq!(
            store
                .append_with_index("main", &json!({ "record": 0 }))
                .await
                .unwrap(),
            0
        );
        store.append("main", &json!({ "record": 1 })).await.unwrap();

        let index = store
            .append_with_index("main", &json!({ "record": 2 }))
            .await
            .unwrap();

        assert_eq!(index, 2);
    }

    #[tokio::test]
    async fn append_serializes_the_complete_batch_before_taking_the_file_lock() {
        let (store, dir) = temp_store();
        let path = dir.path().join("main.jsonl");
        fs::write(&path, []).unwrap();
        let observed_unlocked = Arc::new(AtomicBool::new(false));
        let message = LockCheckingSerialize {
            path,
            observed_unlocked: Arc::clone(&observed_unlocked),
        };

        store
            .append_serializable("main", &[message], None)
            .await
            .unwrap();

        assert!(observed_unlocked.load(Ordering::Relaxed));
    }

    #[tokio::test]
    async fn serialization_failure_preserves_file_and_warm_tail() {
        let (store, dir) = temp_store();
        store.append("main", &json!({ "record": 0 })).await.unwrap();
        let path = dir.path().join("main.jsonl");
        let bytes_before = fs::read(&path).unwrap();
        let tail_state = store.tail_state_for("main", &path).unwrap();
        let cursor_before = tail_state.lock().unwrap().cursor.clone();

        let error = store
            .append_serializable("main", &[FailingSerialize], None)
            .await
            .unwrap_err();

        assert!(error.to_string().contains("injected serialization failure"));
        assert_eq!(fs::read(path).unwrap(), bytes_before);
        assert_eq!(tail_state.lock().unwrap().cursor, cursor_before);
    }

    #[tokio::test]
    async fn test_append_at_index_mismatch_refuses_without_writing() {
        let (store, dir) = temp_store();
        store
            .append("main", &json!({ "role": "user", "content": "before" }))
            .await
            .unwrap();
        let session_path = dir.path().join("main.jsonl");
        let bytes_before = fs::read(&session_path).unwrap();

        let result = store
            .append_at_index(
                "main",
                &json!({ "role": "assistant", "content": "must not persist" }),
                2,
            )
            .await;

        assert!(result.is_err());
        assert_eq!(fs::read(session_path).unwrap(), bytes_before);
        let history = store.read("main").await.unwrap();
        assert_eq!(history.len(), 1);
        assert_eq!(history[0]["content"], "before");
    }

    #[tokio::test]
    async fn append_at_index_mismatch_preserves_the_warm_cursor() {
        let (store, dir) = temp_store();
        store.append("main", &json!({ "record": 0 })).await.unwrap();
        let path = dir.path().join("main.jsonl");
        let tail_state = store.tail_state_for("main", &path).unwrap();
        let cursor_before = tail_state.lock().unwrap().cursor.clone();

        store
            .append_at_index("main", &json!({ "record": 1 }), 2)
            .await
            .unwrap_err();

        assert_eq!(tail_state.lock().unwrap().cursor, cursor_before);
    }

    #[tokio::test]
    async fn repeated_cold_cas_mismatch_scans_existing_history_once() {
        let (store, dir) = temp_store();
        let history = (0..10_000)
            .map(|record| format!("{{\"record\":{record}}}\n"))
            .collect::<String>();
        let path = dir.path().join("main.jsonl");
        let bytes_before = history.as_bytes().to_vec();
        fs::write(&path, history).unwrap();

        for _ in 0..2 {
            store
                .append_at_index("main", &json!({ "record": "rejected" }), 0)
                .await
                .unwrap_err();
        }

        assert_eq!(fs::read(path).unwrap(), bytes_before);
        assert_eq!(store.tail_registry.scan_metrics().0, 1);
    }

    #[test]
    fn append_index_overflow_is_an_explicit_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("main.jsonl");
        let file = OpenOptions::new()
            .create(true)
            .read(true)
            .append(true)
            .open(&path)
            .unwrap();
        let stamp = SessionFileStamp::read(&file).unwrap();
        let registry = SessionTailRegistry::new();
        let mut tail = SessionTailState::default();
        tail.set(usize::MAX, stamp);
        let batch = serialize_batch(&[json!({ "record": 0 })]).unwrap();
        let bytes_before = fs::read(&path).unwrap();

        let result = append_serialized_locked(&path, &batch, None, &registry, &mut tail);

        assert!(matches!(
            result,
            Err(AppendFailure::InvalidatesCursor(Error::Message { message }))
                if message == "session message index overflow"
        ));
        assert_eq!(fs::read(path).unwrap(), bytes_before);
    }

    #[tokio::test]
    async fn partial_write_failure_invalidates_tail_and_corruption_remains_fail_closed() {
        let (store, dir) = temp_store();
        store.append("main", &json!({ "record": 0 })).await.unwrap();
        store.tail_registry.fail_next_write();

        let error = store
            .append_with_index("main", &json!({ "record": 1 }))
            .await
            .unwrap_err();

        assert!(
            error
                .to_string()
                .contains("injected session append write failure")
        );
        let path = dir.path().join("main.jsonl");
        let tail_state = store.tail_state_for("main", &path).unwrap();
        assert!(tail_state.lock().unwrap().cursor.is_none());
        let retry_error = store
            .append_with_index("main", &json!({ "record": 2 }))
            .await
            .unwrap_err();
        assert!(
            retry_error
                .to_string()
                .contains("session JSONL ends with an incomplete record")
        );
    }

    #[tokio::test]
    async fn file_lock_failure_is_explicit_and_the_next_append_recovers_the_tail() {
        let (store, dir) = temp_store();
        store.append("main", &json!({ "record": 0 })).await.unwrap();
        let path = dir.path().join("main.jsonl");
        let bytes_before = fs::read(&path).unwrap();
        store.tail_registry.fail_next_file_lock();

        let error = store
            .append_with_index("main", &json!({ "record": 1 }))
            .await
            .unwrap_err();

        assert!(
            error
                .to_string()
                .contains("injected session file lock failure")
        );
        assert_eq!(fs::read(&path).unwrap(), bytes_before);
        assert_eq!(
            store
                .append_with_index("main", &json!({ "record": 1 }))
                .await
                .unwrap(),
            1
        );
    }

    #[tokio::test]
    async fn metadata_failure_is_explicit_and_the_next_append_recovers_the_tail() {
        let (store, dir) = temp_store();
        store.append("main", &json!({ "record": 0 })).await.unwrap();
        let path = dir.path().join("main.jsonl");
        let bytes_before = fs::read(&path).unwrap();
        store.tail_registry.fail_next_metadata();

        let error = store
            .append_with_index("main", &json!({ "record": 1 }))
            .await
            .unwrap_err();

        assert!(
            error
                .to_string()
                .contains("injected session metadata failure")
        );
        assert_eq!(fs::read(&path).unwrap(), bytes_before);
        assert_eq!(
            store
                .append_with_index("main", &json!({ "record": 1 }))
                .await
                .unwrap(),
            1
        );
    }

    #[tokio::test]
    async fn test_update_typed_at_preserves_history_order() {
        use crate::message::PersistedMessage;

        let (store, _dir) = temp_store();
        store
            .append_typed("main", &PersistedMessage::user("before"))
            .await
            .unwrap();
        store
            .append_typed(
                "main",
                &PersistedMessage::assistant("tool segment", "gpt-4.1", "openai", 10, 2, None),
            )
            .await
            .unwrap();
        store
            .append_typed("main", &PersistedMessage::system("after"))
            .await
            .unwrap();

        store
            .update_typed_at("main", 1, |_| {
                PersistedMessage::assistant("finalized segment", "gpt-4.1", "openai", 20, 5, None)
            })
            .await
            .unwrap();

        let messages = store.read_typed("main").await.unwrap();
        assert_eq!(messages.len(), 3);
        assert!(matches!(
            &messages[0],
            PersistedMessage::User { content: crate::message::MessageContent::Text(content), .. } if content == "before"
        ));
        assert!(matches!(
            &messages[1],
            PersistedMessage::Assistant { content, input_tokens: Some(20), output_tokens: Some(5), .. } if content == "finalized segment"
        ));
        assert!(matches!(
            &messages[2],
            PersistedMessage::System { content, .. } if content == "after"
        ));
    }

    #[tokio::test]
    async fn test_update_typed_at_missing_session_does_not_create_file() {
        use crate::message::PersistedMessage;

        let (store, dir) = temp_store();
        let session_path = dir.path().join("missing.jsonl");

        let result = store
            .update_typed_at("missing", 0, |_| {
                PersistedMessage::assistant("noop", "gpt-4.1", "openai", 1, 1, None)
            })
            .await;

        assert!(result.is_err());
        assert!(!session_path.exists());
    }

    #[tokio::test]
    async fn test_read_typed_empty() {
        let (store, _dir) = temp_store();
        let msgs = store.read_typed("nonexistent").await.unwrap();
        assert!(msgs.is_empty());
    }

    #[tokio::test]
    async fn test_read_last_n_typed() {
        use crate::message::PersistedMessage;

        let (store, _dir) = temp_store();

        for i in 0..5 {
            store
                .append_typed("test", &PersistedMessage::user(format!("msg-{i}")))
                .await
                .unwrap();
        }

        let last2 = store.read_last_n_typed("test", 2).await.unwrap();
        assert_eq!(last2.len(), 2);
        match &last2[0] {
            PersistedMessage::User { content, .. } => {
                assert!(matches!(content, crate::message::MessageContent::Text(t) if t == "msg-3"));
            },
            _ => panic!("expected User message"),
        }
        match &last2[1] {
            PersistedMessage::User { content, .. } => {
                assert!(matches!(content, crate::message::MessageContent::Text(t) if t == "msg-4"));
            },
            _ => panic!("expected User message"),
        }
    }

    #[tokio::test]
    async fn test_replace_history_typed() {
        use crate::message::PersistedMessage;

        let (store, _dir) = temp_store();

        store
            .append_typed("main", &PersistedMessage::user("old"))
            .await
            .unwrap();
        assert_eq!(store.count("main").await.unwrap(), 1);

        let new_history = vec![
            PersistedMessage::user("new1"),
            PersistedMessage::assistant("new2", "gpt-4o", "openai", 10, 5, None),
        ];
        store
            .replace_history_typed("main", &new_history)
            .await
            .unwrap();

        let msgs = store.read_typed("main").await.unwrap();
        assert_eq!(msgs.len(), 2);
        match &msgs[0] {
            PersistedMessage::User { content, .. } => {
                assert!(matches!(content, crate::message::MessageContent::Text(t) if t == "new1"));
            },
            _ => panic!("expected User message"),
        }
    }

    #[tokio::test]
    async fn test_typed_roundtrip_with_value_api() {
        use crate::message::PersistedMessage;

        let (store, _dir) = temp_store();

        // Write with typed API, read with Value API.
        store
            .append_typed("main", &PersistedMessage::user("typed write"))
            .await
            .unwrap();
        let values = store.read("main").await.unwrap();
        assert_eq!(values.len(), 1);
        assert_eq!(values[0]["role"], "user");
        assert_eq!(values[0]["content"], "typed write");

        // Write with Value API, read with typed API.
        store
            .append(
                "main",
                &json!({"role": "assistant", "content": "value write"}),
            )
            .await
            .unwrap();
        let typed = store.read_typed("main").await.unwrap();
        assert_eq!(typed.len(), 2);
        match &typed[1] {
            PersistedMessage::Assistant { content, .. } => {
                assert_eq!(content, "value write");
            },
            _ => panic!("expected Assistant message"),
        }
    }

    #[tokio::test]
    async fn replace_history_publishes_its_exact_next_index() {
        let (store, _dir) = temp_store();
        store
            .append("main", &json!({ "record": "old" }))
            .await
            .unwrap();
        store
            .replace_history("main", vec![json!({ "record": 0 }), json!({ "record": 1 })])
            .await
            .unwrap();

        assert_eq!(
            store
                .append_with_index("main", &json!({ "record": 2 }))
                .await
                .unwrap(),
            2
        );
    }

    #[tokio::test]
    async fn replace_history_typed_publishes_its_exact_next_index() {
        let (store, _dir) = temp_store();
        let replacement = [
            PersistedMessage::user("first"),
            PersistedMessage::system("second"),
        ];
        store
            .replace_history_typed("main", &replacement)
            .await
            .unwrap();

        assert_eq!(
            store
                .append_with_index("main", &json!({ "role": "system", "content": "third" }))
                .await
                .unwrap(),
            2
        );
    }

    #[tokio::test]
    async fn incomplete_typed_rewrite_stream_preserves_existing_history() {
        let (store, dir) = temp_store();
        store
            .append("main", &json!({ "record": "existing" }))
            .await
            .unwrap();
        let path = dir.path().join("main.jsonl");
        let bytes_before = fs::read(&path).unwrap();
        let registry = Arc::clone(&store.tail_registry);
        let (sender, receiver) = tokio::sync::mpsc::channel(1);
        sender
            .send(TypedHistoryStage::Message(Box::new(
                PersistedMessage::system("staged"),
            )))
            .await
            .unwrap();
        drop(sender);

        let error = tokio::task::spawn_blocking({
            let path = path.clone();
            move || replace_typed_from_receiver(path, registry, receiver)
        })
        .await
        .unwrap()
        .unwrap_err();

        assert!(
            error
                .to_string()
                .contains("typed session history stream closed before completion")
        );
        assert_eq!(fs::read(path).unwrap(), bytes_before);
    }

    #[tokio::test]
    async fn truncate_publishes_the_kept_record_count() {
        let (store, _dir) = temp_store();
        store
            .append("main", &json!({ "role": "system" }))
            .await
            .unwrap();
        store
            .append(
                "main",
                &json!({ "role": "user", "content": "remove", "seq": 7 }),
            )
            .await
            .unwrap();
        store
            .append("main", &json!({ "role": "assistant", "content": "tail" }))
            .await
            .unwrap();
        store
            .truncate_from_user_message("main", UserMessageTarget::ClientSeq(7))
            .await
            .unwrap();

        assert_eq!(
            store
                .append_with_index("main", &json!({ "role": "system", "content": "next" }))
                .await
                .unwrap(),
            1
        );
    }

    #[tokio::test]
    async fn update_typed_at_preserves_tail_after_changing_line_size() {
        let (store, _dir) = temp_store();
        store
            .append_typed("main", &PersistedMessage::user("short"))
            .await
            .unwrap();
        store
            .append_typed("main", &PersistedMessage::system("tail"))
            .await
            .unwrap();
        store
            .update_typed_at("main", 0, |_| PersistedMessage::user("x".repeat(16_384)))
            .await
            .unwrap();

        assert_eq!(
            store
                .append_with_index("main", &json!({ "role": "system", "content": "next" }))
                .await
                .unwrap(),
            2
        );
    }

    #[tokio::test]
    async fn clear_removes_tail_before_recreating_the_session() {
        let (store, _dir) = temp_store();
        store.append("main", &json!({ "record": 0 })).await.unwrap();
        store.append("main", &json!({ "record": 1 })).await.unwrap();
        store.clear("main").await.unwrap();

        assert_eq!(
            store
                .append_with_index("main", &json!({ "record": "recreated" }))
                .await
                .unwrap(),
            0
        );
    }

    #[tokio::test]
    async fn fork_replace_history_publishes_the_copied_tail() {
        let (store, _dir) = temp_store();
        let fork_history = vec![json!({ "record": 0 }), json!({ "record": 1 })];
        store.replace_history("fork", fork_history).await.unwrap();

        assert_eq!(
            store
                .append_with_index("fork", &json!({ "record": 2 }))
                .await
                .unwrap(),
            2
        );
    }

    #[tokio::test]
    async fn voice_replace_history_publishes_the_updated_tail() {
        let (store, _dir) = temp_store();
        store
            .replace_history("voice", vec![
                json!({ "role": "user", "audio": "media/voice/input.ogg" }),
            ])
            .await
            .unwrap();

        assert_eq!(
            store
                .append_with_index("voice", &json!({ "role": "assistant", "content": "heard" }))
                .await
                .unwrap(),
            1
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn concurrent_appends_return_a_unique_contiguous_order() {
        let (store, _dir) = temp_store();
        let store = Arc::new(store);
        let mut tasks = Vec::new();
        for record in 0..256 {
            let store = Arc::clone(&store);
            tasks.push(tokio::spawn(async move {
                let index = store
                    .append_with_index("main", &json!({ "record": record }))
                    .await
                    .unwrap();
                (index, record)
            }));
        }

        let mut issued = Vec::new();
        for task in tasks {
            issued.push(task.await.unwrap());
        }
        issued.sort_unstable_by_key(|(index, _)| *index);

        assert_eq!(
            issued.iter().map(|(index, _)| *index).collect::<Vec<_>>(),
            (0..256).collect::<Vec<_>>()
        );
        let history = store.read("main").await.unwrap();
        assert_eq!(history.len(), 256);
        for (index, record) in issued {
            assert_eq!(history[index]["record"], record);
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn concurrent_expected_index_cas_allows_exactly_one_winner() {
        let (store, _dir) = temp_store();
        let store = Arc::new(store);
        let mut tasks = Vec::new();
        for contender in 0..128 {
            let store = Arc::clone(&store);
            tasks.push(tokio::spawn(async move {
                store
                    .append_at_index("main", &json!({ "contender": contender }), 0)
                    .await
            }));
        }

        let mut winners = 0;
        for task in tasks {
            if task.await.unwrap().is_ok() {
                winners += 1;
            }
        }

        assert_eq!(winners, 1);
        assert_eq!(store.read("main").await.unwrap().len(), 1);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn blocked_session_state_does_not_block_another_session() {
        let (store, _dir) = temp_store();
        let store = Arc::new(store);
        let blocked_path = store.path_for("blocked");
        let blocked_state = store.tail_state_for("blocked", &blocked_path).unwrap();
        let held_state = Arc::clone(&blocked_state);
        let (locked_tx, locked_rx) = mpsc::sync_channel(1);
        let (release_tx, release_rx) = mpsc::sync_channel(1);
        let holder = std::thread::spawn(move || {
            let _guard = held_state.lock().unwrap();
            locked_tx.send(()).unwrap();
            release_rx.recv().unwrap();
        });
        locked_rx.recv().unwrap();
        let blocked_store = Arc::clone(&store);
        let blocked_task = tokio::spawn(async move {
            blocked_store
                .append_with_index("blocked", &json!({ "record": 0 }))
                .await
        });
        while Arc::strong_count(&blocked_state) < 3 {
            tokio::task::yield_now().await;
        }

        let independent_result = tokio::time::timeout(
            std::time::Duration::from_secs(1),
            store.append_with_index("independent", &json!({ "record": 0 })),
        )
        .await
        .expect("independent session must not wait for another session");

        assert_eq!(independent_result.unwrap(), 0);
        release_tx.send(()).unwrap();
        assert_eq!(blocked_task.await.unwrap().unwrap(), 0);
        holder.join().unwrap();
    }

    #[tokio::test]
    async fn a_second_store_recovers_after_each_external_stamp_change() {
        let dir = tempfile::tempdir().unwrap();
        let first = SessionStore::new(dir.path().to_path_buf());
        let second = SessionStore::new(dir.path().to_path_buf());

        assert_eq!(
            first
                .append_with_index("main", &json!({ "record": 0 }))
                .await
                .unwrap(),
            0
        );
        assert_eq!(
            second
                .append_with_index("main", &json!({ "record": 1 }))
                .await
                .unwrap(),
            1
        );
        assert_eq!(
            first
                .append_with_index("main", &json!({ "record": 2 }))
                .await
                .unwrap(),
            2
        );
        assert_eq!(
            second
                .append_with_index("main", &json!({ "record": 3 }))
                .await
                .unwrap(),
            3
        );
        assert_eq!(second.tail_registry.scan_metrics().0, 2);
    }

    #[tokio::test]
    async fn external_file_append_invalidates_the_cached_stamp() {
        let (store, dir) = temp_store();
        store.append("main", &json!({ "record": 0 })).await.unwrap();
        let path = dir.path().join("main.jsonl");
        let mut file = OpenOptions::new().append(true).open(path).unwrap();
        writeln!(file, "{{\"record\":1}}").unwrap();
        file.flush().unwrap();

        let index = store
            .append_with_index("main", &json!({ "record": 2 }))
            .await
            .unwrap();

        assert_eq!(index, 2);
        assert_eq!(store.tail_registry.scan_metrics().0, 2);
    }

    #[tokio::test]
    async fn restart_scans_existing_history_once() {
        let dir = tempfile::tempdir().unwrap();
        let initial = (0..10_000)
            .map(|record| format!("{{\"record\":{record}}}\n"))
            .collect::<String>();
        fs::write(dir.path().join("main.jsonl"), initial).unwrap();
        let restarted = SessionStore::new(dir.path().to_path_buf());

        assert_eq!(
            restarted
                .append_with_index("main", &json!({ "record": 10_000 }))
                .await
                .unwrap(),
            10_000
        );
        assert_eq!(
            restarted
                .append_with_index("main", &json!({ "record": 10_001 }))
                .await
                .unwrap(),
            10_001
        );
        assert_eq!(restarted.tail_registry.scan_metrics().0, 1);
    }

    #[tokio::test]
    async fn incomplete_last_record_is_rejected_without_appending() {
        let (store, dir) = temp_store();
        let path = dir.path().join("main.jsonl");
        let damaged = b"{\"record\":0}\n{\"record\":1}";
        fs::write(&path, damaged).unwrap();

        let error = store
            .append_with_index("main", &json!({ "record": 2 }))
            .await
            .unwrap_err();

        assert!(
            error
                .to_string()
                .contains("session JSONL ends with an incomplete record")
        );
        assert_eq!(fs::read(path).unwrap(), damaged);
    }

    #[tokio::test]
    async fn scan_read_error_is_not_treated_as_an_empty_history() {
        let (store, dir) = temp_store();
        let path = dir.path().join("main.jsonl");
        fs::write(&path, [0xff, b'\n']).unwrap();

        let error = store
            .append_with_index("main", &json!({ "record": 0 }))
            .await
            .unwrap_err();

        assert!(matches!(error, Error::Io(_)));
        assert_eq!(fs::read(path).unwrap(), [0xff, b'\n']);
    }

    #[tokio::test]
    async fn open_error_is_not_treated_as_an_empty_history() {
        let (store, dir) = temp_store();
        fs::create_dir(dir.path().join("main.jsonl")).unwrap();

        let error = store
            .append_with_index("main", &json!({ "record": 0 }))
            .await
            .unwrap_err();

        assert!(matches!(error, Error::Io(_)));
    }

    #[tokio::test]
    async fn warm_indexed_appends_scan_old_bytes_exactly_once() {
        const EXISTING_RECORDS: usize = 100_000;
        const NEW_RECORDS: usize = 2_000;

        let (store, dir) = temp_store();
        let initial = (0..EXISTING_RECORDS)
            .map(|record| format!("{{\"record\":{record}}}\n"))
            .collect::<String>();
        let initial_bytes = initial.len();
        fs::write(dir.path().join("main.jsonl"), initial).unwrap();

        for offset in 0..NEW_RECORDS {
            let expected_index = EXISTING_RECORDS + offset;
            let index = store
                .append_with_index("main", &json!({ "record": expected_index }))
                .await
                .unwrap();
            assert_eq!(index, expected_index);
        }

        assert_eq!(store.tail_registry.scan_metrics(), (1, initial_bytes));
    }
}
