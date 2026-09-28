use std::{
    fs,
    path::PathBuf,
    sync::{Arc, Mutex},
};

use serde::Serialize;

use crate::{
    Error, PersistedMessage, Result,
    journal::{self, ActiveEvent, JournalImportConflict},
    owner_lock::ProcessOwnerLock,
};

/// How to locate a user message that starts a session-tail mutation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UserMessageTarget {
    /// Zero-based physical journal index.
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

pub type SearchResult = crate::ui_history_types::UiSearchHit;

/// Canonical journal in `ui-history.sqlite` plus on-disk media.
pub struct SessionStore {
    pub base_dir: PathBuf,
    pub ui_history: Arc<crate::ui_history_engine::UiHistoryEngine>,
    owner_lock: Mutex<Option<ProcessOwnerLock>>,
}

impl SessionStore {
    pub fn new(base_dir: PathBuf) -> Self {
        Self {
            ui_history: Arc::new(crate::ui_history_engine::UiHistoryEngine::new(
                base_dir.clone(),
            )),
            base_dir,
            owner_lock: Mutex::new(None),
        }
    }

    /// Hold the process owner lock until this store is dropped.
    pub fn hold_owner_lock(&self) -> Result<()> {
        let lock = ProcessOwnerLock::try_acquire(&self.base_dir.join("ui-history.owner.lock"))?;
        let mut slot = self
            .owner_lock
            .lock()
            .map_err(|error| Error::lock_failed(error.to_string()))?;
        *slot = Some(lock);
        Ok(())
    }

    /// Sanitize a session key for use as a media directory name.
    pub fn key_to_filename(key: &str) -> String {
        key.replace(':', "_")
    }

    fn media_dir_for(&self, key: &str) -> PathBuf {
        self.base_dir.join("media").join(Self::key_to_filename(key))
    }

    pub fn media_path_for(&self, key: &str, filename: &str) -> PathBuf {
        self.media_dir_for(key).join(filename)
    }

    async fn pool(&self) -> Result<&sqlx::SqlitePool> {
        self.ui_history.pool().await
    }

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

    pub async fn read_media(&self, key: &str, filename: &str) -> Result<Vec<u8>> {
        let file_path = self.media_path_for(key, filename);
        tokio::task::spawn_blocking(move || -> Result<Vec<u8>> { Ok(fs::read(&file_path)?) })
            .await?
    }

    pub async fn append(&self, key: &str, message: &serde_json::Value) -> Result<()> {
        self.append_serializable(key, std::slice::from_ref(message), None)
            .await
            .map(|_| ())
    }

    pub async fn append_with_index(&self, key: &str, message: &serde_json::Value) -> Result<usize> {
        self.append_with_expected_index(key, std::slice::from_ref(message), None)
            .await
    }

    pub async fn append_at_index(
        &self,
        key: &str,
        message: &serde_json::Value,
        expected_index: usize,
    ) -> Result<usize> {
        self.append_with_expected_index(key, std::slice::from_ref(message), Some(expected_index))
            .await
    }

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
        T: Serialize + Send + Sync,
    {
        let ui = self.ui_history.session(key).await?;
        let mut journal_state = ui.journal.lock().await;
        if let Some(expected) = expected_index
            && expected != journal_state.canonical_tail
        {
            return Err(Error::message(format!(
                "expected message index {expected}, found session tail {}",
                journal_state.canonical_tail
            )));
        }
        if messages.is_empty() {
            return Ok(journal_state.canonical_tail);
        }
        let values = messages
            .iter()
            .map(|message| serde_json::to_value(message).map_err(Error::from))
            .collect::<Result<Vec<_>>>()?;
        let records = values
            .iter()
            .map(|message| crate::ui_history_types::UiRecord::try_from(message.clone()))
            .collect::<Result<Vec<_>>>()?;
        let receipts = ui.stage_batch(records).await?;
        let pool = self.pool().await?;
        let index = journal::append(pool, key, &values, journal_state.canonical_tail)
            .await
            .inspect_err(|error| ui.fail(error))?;
        for (offset, receipt) in receipts.iter().enumerate() {
            let position = index
                .checked_add(offset)
                .ok_or_else(|| Error::message("canonical index overflow"))?;
            ui.bind(receipt, position)
                .inspect_err(|error| ui.fail(error))?;
        }
        journal_state.canonical_tail = index
            .checked_add(receipts.len())
            .ok_or_else(|| Error::message("canonical index overflow"))?;
        ui.flush_if_idle()
            .await
            .inspect_err(|error| ui.fail(error))?;
        Ok(index)
    }

    pub async fn with_active_records<F>(&self, key: &str, visit: F) -> Result<usize>
    where
        F: FnMut(ActiveEvent) -> Result<()>,
    {
        let _ui = self.ui_history.session(key).await?;
        journal::with_active_records(self.pool().await?, key, visit).await
    }

    pub async fn read(&self, key: &str) -> Result<Vec<serde_json::Value>> {
        let pool = self.pool().await?;
        match journal::pointers(pool, key).await {
            Ok(pointers) => journal::read_range(pool, key, 0, pointers.canonical_tail).await,
            Err(Error::NoCanonicalJournal { .. }) => Ok(Vec::new()),
            Err(error) => Err(error),
        }
    }

    pub async fn ui_message_count(&self, key: &str) -> Result<u32> {
        self.ui_history.count(key).await
    }

    pub async fn read_by_run_id(&self, key: &str, run_id: &str) -> Result<Vec<serde_json::Value>> {
        self.ui_history
            .session(key)
            .await?
            .read_run(run_id)
            .await?
            .iter()
            .map(crate::ui_history_types::UiSnapshot::public_value)
            .collect()
    }

    pub async fn read_record(&self, key: &str, index: usize) -> Result<serde_json::Value> {
        journal::read_record(self.pool().await?, key, index).await
    }

    pub async fn read_range(
        &self,
        key: &str,
        start: usize,
        end: usize,
    ) -> Result<Vec<serde_json::Value>> {
        journal::read_range(self.pool().await?, key, start, end).await
    }

    pub async fn read_last_n(&self, key: &str, n: usize) -> Result<Vec<serde_json::Value>> {
        let pool = self.pool().await?;
        match journal::pointers(pool, key).await {
            Ok(_) => journal::read_last_n(pool, key, n).await,
            Err(Error::NoCanonicalJournal { .. }) => Ok(Vec::new()),
            Err(error) => Err(error),
        }
    }

    pub async fn clear(&self, key: &str) -> Result<()> {
        let ui = self.ui_history.session_for_clear(key).await?;
        let mut journal_state = ui.journal.lock().await;
        let media_dir = self.media_dir_for(key);
        let tool_results_dir =
            crate::tool_results::ToolResultStore::new(self.base_dir.clone()).session_dir(key);
        let session_key = key.to_string();
        let cleanup = tokio::task::spawn_blocking(move || -> Result<()> {
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
        })
        .await
        .map_err(Error::from)
        .and_then(std::convert::identity);
        if let Err(error) = cleanup {
            ui.fail(&error);
            ui.flush().await?;
            return Err(error);
        }
        journal::clear_journal(self.pool().await?, &session_key)
            .await
            .inspect_err(|error| ui.fail(error))?;
        ui.truncate(0).await.inspect_err(|error| ui.fail(error))?;
        journal_state.canonical_tail = 0;
        Ok(())
    }

    pub async fn list_keys(&self) -> Result<Vec<String>> {
        journal::list_keys(self.pool().await?).await
    }

    pub async fn search(
        &self,
        keys: &[String],
        query: &str,
        max_results: usize,
    ) -> Result<Vec<SearchResult>> {
        self.ui_history.search(keys, query, max_results).await
    }

    pub async fn fork_history(
        &self,
        parent: &str,
        key: &str,
        fork_point: Option<u64>,
    ) -> Result<crate::ui_history_types::UiForkResult> {
        if parent == key {
            return Err(Error::message(
                "fork destination must differ from its parent",
            ));
        }
        let source = self.ui_history.session(parent).await?;
        let _source_journal = source.journal.lock().await;
        let snapshot = source.fork_snapshot(fork_point).await?;
        let result = snapshot.result.clone();
        let boundary = snapshot.canonical_tail;
        let destination = self.ui_history.session(key).await?;
        let mut destination_journal = destination
            .journal
            .try_lock()
            .map_err(|error| Error::message(format!("fork destination is busy: {error}")))?;
        destination.ensure_empty()?;
        journal::copy_prefix(self.pool().await?, parent, key, boundary).await?;
        destination
            .copy_prefix_from(snapshot)
            .await
            .inspect_err(|error| destination.fail(error))?;
        destination_journal.canonical_tail = boundary;
        Ok(result)
    }

    pub async fn read_typed(&self, key: &str) -> Result<Vec<PersistedMessage>> {
        self.read(key)
            .await?
            .into_iter()
            .map(|value| serde_json::from_value(value).map_err(Error::from))
            .collect()
    }

    pub async fn child_snapshot_messages(&self, key: &str) -> Result<Vec<PersistedMessage>> {
        let values = match journal::child_suffix(self.pool().await?, key).await {
            Ok(values) => values,
            Err(Error::NoCanonicalJournal { .. }) => return Ok(Vec::new()),
            Err(error) => return Err(error),
        };
        values
            .into_iter()
            .map(|value| serde_json::from_value(value).map_err(Error::from))
            .collect()
    }

    pub async fn pointers(&self, key: &str) -> Result<journal::JournalPointers> {
        journal::pointers(self.pool().await?, key).await
    }

    pub async fn visible_tool_names(&self, key: &str) -> Result<std::collections::HashSet<String>> {
        journal::visible_tool_names(self.pool().await?, key).await
    }

    pub async fn token_totals(&self, key: &str) -> Result<journal::TokenTotals> {
        journal::token_totals(self.pool().await?, key).await
    }

    pub async fn assistant_payloads(&self, key: &str) -> Result<Vec<serde_json::Value>> {
        journal::assistant_payloads(self.pool().await?, key).await
    }

    pub async fn truncate_from_user_message(
        &self,
        key: &str,
        target: UserMessageTarget,
    ) -> Result<TruncateTailResult> {
        let ui = self.ui_history.session(key).await?;
        let mut journal_state = ui.journal.lock().await;
        let boundary = self.resolve_user_target(key, target).await?;
        ui.validate_canonical_cut(boundary).await?;
        let original = journal_state.canonical_tail;
        let kept = self.read_range(key, 0, boundary).await?;
        let retained_media = collect_session_media_refs(&kept, &Self::key_to_filename(key));
        journal::truncate_journal(self.pool().await?, key, boundary).await?;
        let media_dir = self.media_dir_for(key);
        let pruned_media_count = tokio::task::spawn_blocking(move || {
            prune_unreferenced_media(&media_dir, &retained_media)
        })
        .await??;
        ui.truncate(boundary)
            .await
            .inspect_err(|error| ui.fail(error))?;
        journal_state.canonical_tail = boundary;
        Ok(TruncateTailResult {
            target_index: boundary,
            kept_count: boundary,
            removed_count: original.saturating_sub(boundary),
            pruned_media_count,
        })
    }

    async fn resolve_user_target(&self, key: &str, target: UserMessageTarget) -> Result<usize> {
        let pool = self.pool().await?;
        let tail = match journal::pointers(pool, key).await {
            Ok(pointers) => pointers.canonical_tail,
            Err(Error::NoCanonicalJournal { .. }) => 0,
            Err(error) => return Err(error),
        };
        match target {
            UserMessageTarget::MessageIndex(index) => {
                if index >= tail {
                    return Err(Error::message(format!(
                        "messageIndex {index} exceeds message count {tail}"
                    )));
                }
                let role = journal::record_role(pool, key, index).await?;
                if role != "user" {
                    return Err(Error::message(format!(
                        "message at index {index} is not a user message"
                    )));
                }
                Ok(index)
            },
            UserMessageTarget::ClientSeq(seq) => journal::user_index_for_seq(pool, key, seq).await,
        }
    }

    pub async fn append_typed(&self, key: &str, message: &PersistedMessage) -> Result<()> {
        self.append(key, &message.to_value()).await
    }

    pub async fn update_typed_at<F>(
        &self,
        key: &str,
        message_index: usize,
        update: F,
    ) -> Result<PersistedMessage>
    where
        F: FnOnce(PersistedMessage) -> PersistedMessage + Send + 'static,
    {
        let value = self
            .update_value_at(key, message_index, move |value| {
                let mut record = crate::ui_history_types::UiRecord::try_from(value)?;
                record.message = update(record.message);
                Ok(serde_json::to_value(record)?)
            })
            .await?;
        Ok(serde_json::from_value(value)?)
    }

    pub async fn update_value_at<F>(
        &self,
        key: &str,
        message_index: usize,
        update: F,
    ) -> Result<serde_json::Value>
    where
        F: FnOnce(serde_json::Value) -> Result<serde_json::Value> + Send + 'static,
    {
        let ui = self.ui_history.session(key).await?;
        let _journal = ui.journal.lock().await;
        let existing = self.read_record(key, message_index).await?;
        let updated = update(existing)?;
        let prepared = ui.prepare_record_update(message_index).await?;
        crate::ui_history_engine::UiHistorySession::validate_record_update(
            &prepared.entry,
            crate::ui_history_types::UiRecord::try_from(updated.clone())?,
        )?;
        journal::update_record(self.pool().await?, key, message_index, &updated).await?;
        ui.update_record(
            &prepared,
            crate::ui_history_types::UiRecord::try_from(updated.clone())?,
        )
        .await
        .inspect_err(|error| ui.fail(error))?;
        Ok(updated)
    }

    pub async fn latest_matching_assistant(
        &self,
        key: &str,
        tail: usize,
        expected_ids: &[&str],
    ) -> Result<Option<usize>> {
        journal::latest_matching_assistant(self.pool().await?, key, tail, expected_ids).await
    }

    pub async fn latest_user_before(&self, key: &str, before: usize) -> Result<Option<usize>> {
        journal::latest_user_before(self.pool().await?, key, before).await
    }

    pub async fn latest_user_text_contained_by(
        &self,
        key: &str,
        tail: usize,
        expected: &str,
    ) -> Result<Option<usize>> {
        journal::latest_user_text_contained_by(self.pool().await?, key, tail, expected).await
    }

    pub async fn record_role(&self, key: &str, index: usize) -> Result<String> {
        journal::record_role(self.pool().await?, key, index).await
    }

    pub async fn import_journal(
        &self,
        snapshot: &std::path::Path,
        conflict: JournalImportConflict,
    ) -> Result<Vec<String>> {
        let pool = self.pool().await?;
        let mut conn = pool.acquire().await?;
        let escaped = snapshot.to_string_lossy().replace('\'', "''");
        sqlx::query(&format!("ATTACH DATABASE '{escaped}' AS import_db"))
            .execute(&mut *conn)
            .await?;
        let import_result = async {
            let keys = journal::import_attached_keys(&mut conn).await?;
            let mut errors = Vec::new();
            for key in keys {
                let mut skipped = false;
                let idle = self
                    .ui_history
                    .import_key_if_idle(&key, async {
                        let imported = journal::import_one(&mut conn, &key, conflict).await?;
                        skipped = !imported;
                        Ok(())
                    })
                    .await?;
                if !idle {
                    errors.push(format!(
                        "session '{key}' is in use (open in the UI or running); retry when it is idle"
                    ));
                } else if skipped {
                    errors.push(format!("session '{key}' already exists; skipped"));
                }
            }
            Ok(errors)
        }
        .await;
        let detached = sqlx::query("DETACH DATABASE import_db")
            .execute(&mut *conn)
            .await;
        if let Err(error) = detached {
            tracing::error!(%error, "failed to detach imported session journal");
            if let Err(close_error) = conn.close().await {
                tracing::error!(
                    %close_error,
                    "failed to close session journal connection after detach"
                );
            }
            return match import_result {
                Err(error) => Err(error),
                Ok(mut errors) => {
                    errors.push(
                        "session journal rows were committed, but detaching the import snapshot failed"
                            .to_string(),
                    );
                    Ok(errors)
                },
            };
        }
        import_result
    }
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
    use {super::*, serde_json::json};

    fn temp_store() -> (SessionStore, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let store = SessionStore::new(dir.path().to_path_buf());
        (store, dir)
    }

    #[tokio::test]
    async fn append_assigns_indexes_and_pointers() {
        let (store, _dir) = temp_store();
        let first = store
            .append_with_index("main", &PersistedMessage::user("first").to_value())
            .await
            .unwrap();
        let second = store
            .append_with_index("main", &PersistedMessage::user("second").to_value())
            .await
            .unwrap();
        assert_eq!((first, second), (0, 1));
        let pointers = store.pointers("main").await.unwrap();
        assert_eq!(pointers.canonical_tail, 2);
        assert_eq!(pointers.first_user_index, Some(0));
        assert_eq!(pointers.last_checkpoint_index, None);
        store
            .append(
                "main",
                &PersistedMessage::checkpoint("sum", "model", "provider", 1, 2, 2).to_value(),
            )
            .await
            .unwrap();
        let pointers = store.pointers("main").await.unwrap();
        assert_eq!(pointers.last_checkpoint_index, Some(2));
        assert_eq!(pointers.first_user_index, Some(0));
    }

    #[tokio::test]
    async fn expected_index_mismatch_writes_nothing() {
        let (store, _dir) = temp_store();
        store
            .append("main", &PersistedMessage::user("one").to_value())
            .await
            .unwrap();
        assert!(
            store
                .append_at_index("main", &PersistedMessage::user("nope").to_value(), 0)
                .await
                .is_err()
        );
        assert_eq!(store.read("main").await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn active_read_keeps_the_pre_checkpoint_continuation() {
        let (store, _dir) = temp_store();
        for (index, value) in [
            json!({"role": "user", "content": "old"}),
            json!({"role": "checkpoint", "summary": "first", "messagesSummarized": 1}),
            json!({"role": "user", "content": "between"}),
            json!({"role": "assistant", "content": "working"}),
            json!({"role": "checkpoint", "summary": "second", "messagesSummarized": 3}),
            json!({"role": "user", "content": "after"}),
        ]
        .into_iter()
        .enumerate()
        {
            store.append_at_index("main", &value, index).await.unwrap();
        }
        let mut rows = Vec::new();
        store
            .with_active_records("main", |event| {
                if let ActiveEvent::Row { payload, .. } = event {
                    rows.push(
                        payload["content"]
                            .as_str()
                            .unwrap_or("checkpoint")
                            .to_string(),
                    );
                    if payload["role"] == "checkpoint" {
                        rows.push(payload["summary"].as_str().unwrap().to_string());
                    }
                }
                Ok(())
            })
            .await
            .unwrap();
        assert_eq!(rows, vec!["checkpoint", "second", "working", "after"]);
    }

    #[tokio::test]
    async fn disclosures_follow_the_journal_boundary_and_survive_checkpoint() {
        let (store, _dir) = temp_store();
        store
            .append("main", &PersistedMessage::user("keep").to_value())
            .await
            .unwrap();
        store
            .append(
                "main",
                &json!({
                    "role": "assistant",
                    "content": "call",
                    "tool_calls": [{"id": "1", "type": "function", "function": {"name": "read_file", "arguments": "{}"}}]
                }),
            )
            .await
            .unwrap();
        store
            .append(
                "main",
                &PersistedMessage::checkpoint("sum", "model", "provider", 1, 1, 1).to_value(),
            )
            .await
            .unwrap();
        assert!(
            store
                .visible_tool_names("main")
                .await
                .unwrap()
                .contains("read_file")
        );
        store.fork_history("main", "child", None).await.unwrap();
        assert!(
            store
                .visible_tool_names("child")
                .await
                .unwrap()
                .contains("read_file")
        );
        store
            .append("main", &PersistedMessage::user("cut").to_value())
            .await
            .unwrap();
        store
            .append(
                "main",
                &json!({
                    "role": "assistant",
                    "content": "later",
                    "tool_calls": [{"id": "2", "type": "function", "function": {"name": "other_tool", "arguments": "{}"}}]
                }),
            )
            .await
            .unwrap();
        store
            .truncate_from_user_message("main", UserMessageTarget::MessageIndex(3))
            .await
            .unwrap();
        let visible = store.visible_tool_names("main").await.unwrap();
        assert!(visible.contains("read_file"));
        assert!(!visible.contains("other_tool"));
        let pointers = store.pointers("main").await.unwrap();
        assert_eq!(pointers.canonical_tail, 3);
        assert_eq!(pointers.last_checkpoint_index, Some(2));
        assert_eq!(pointers.first_user_index, Some(0));
    }

    #[tokio::test]
    async fn fork_boundary_past_the_parent_tail_writes_nothing() {
        let (store, _dir) = temp_store();
        store
            .append("main", &PersistedMessage::user("one").to_value())
            .await
            .unwrap();
        let error = journal::copy_prefix(store.pool().await.unwrap(), "main", "child", 5)
            .await
            .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("fork boundary is outside canonical journal")
        );
        let child_rows = sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM session_journal WHERE session_key = 'child'",
        )
        .fetch_one(store.pool().await.unwrap())
        .await
        .unwrap();
        assert_eq!(child_rows, 0);
    }

    #[tokio::test]
    async fn failed_import_rolls_back_and_later_attach_works() {
        let (source, dir) = temp_store();
        source
            .append("main", &PersistedMessage::user("from-archive").to_value())
            .await
            .unwrap();
        let good = dir.path().join("good.sqlite");
        let good_sql = good.to_string_lossy().replace('\'', "''");
        sqlx::query(&format!("VACUUM INTO '{good_sql}'"))
            .execute(source.pool().await.unwrap())
            .await
            .unwrap();

        let bad = dir.path().join("bad.sqlite");
        let bad_pool = sqlx::sqlite::SqlitePoolOptions::new()
            .connect_with(
                sqlx::sqlite::SqliteConnectOptions::new()
                    .filename(&bad)
                    .create_if_missing(true),
            )
            .await
            .unwrap();
        sqlx::query(
            "CREATE TABLE session_journal (session_key TEXT PRIMARY KEY, canonical_tail INTEGER NOT NULL, last_checkpoint_index INTEGER, first_user_index INTEGER)",
        )
        .execute(&bad_pool)
        .await
        .unwrap();
        sqlx::query("INSERT INTO session_journal (session_key, canonical_tail) VALUES ('main', 1)")
            .execute(&bad_pool)
            .await
            .unwrap();
        bad_pool.close().await;

        let (store, _dir) = temp_store();
        assert!(
            store
                .import_journal(&bad, JournalImportConflict::Overwrite)
                .await
                .is_err()
        );
        let warnings = store
            .import_journal(&good, JournalImportConflict::Overwrite)
            .await
            .unwrap();
        assert!(warnings.is_empty());
        let records = store.read("main").await.unwrap();
        assert_eq!(records[0]["content"], "from-archive");
    }

    #[tokio::test]
    async fn truncate_targets_client_seq_and_rejects_a_non_user_or_missing_index() {
        let (store, _dir) = temp_store();
        store
            .append("main", &json!({"role": "user", "content": "cut", "seq": 4}))
            .await
            .unwrap();
        store
            .append("main", &PersistedMessage::user("later").to_value())
            .await
            .unwrap();
        let truncated = store
            .truncate_from_user_message("main", UserMessageTarget::ClientSeq(4))
            .await
            .unwrap();
        assert_eq!(truncated.target_index, 0);
        assert_eq!(store.pointers("main").await.unwrap().canonical_tail, 0);

        store
            .append("main", &json!({"role": "assistant", "content": "not-user"}))
            .await
            .unwrap();
        let role_error = store
            .truncate_from_user_message("main", UserMessageTarget::MessageIndex(0))
            .await
            .unwrap_err();
        assert!(role_error.to_string().contains("not a user message"));
        let range_error = store
            .truncate_from_user_message("main", UserMessageTarget::MessageIndex(5))
            .await
            .unwrap_err();
        assert!(range_error.to_string().contains("exceeds message count"));
    }

    #[tokio::test]
    async fn truncate_prunes_media_that_the_removed_tail_alone_referenced() {
        let (store, _dir) = temp_store();
        let media = store.save_media("main", "pic.bin", b"data").await.unwrap();
        store
            .append("main", &json!({"role": "user", "content": media}))
            .await
            .unwrap();
        let truncated = store
            .truncate_from_user_message("main", UserMessageTarget::MessageIndex(0))
            .await
            .unwrap();
        assert_eq!(truncated.pruned_media_count, 1);
        assert!(!store.media_path_for("main", "pic.bin").exists());
    }

    #[tokio::test]
    async fn import_skip_keeps_rows_overwrite_replaces_them_and_a_live_session_is_left_idle() {
        let (source, dir) = temp_store();
        source
            .append(
                "imported",
                &PersistedMessage::user("from-archive").to_value(),
            )
            .await
            .unwrap();
        let snapshot = dir.path().join("snapshot.sqlite");
        let snapshot_sql = snapshot.to_string_lossy().replace('\'', "''");
        sqlx::query(&format!("VACUUM INTO '{snapshot_sql}'"))
            .execute(source.pool().await.unwrap())
            .await
            .unwrap();

        let (store, _dir) = temp_store();
        store
            .append("imported", &PersistedMessage::user("local").to_value())
            .await
            .unwrap();
        let skipped = store
            .import_journal(&snapshot, JournalImportConflict::Skip)
            .await
            .unwrap();
        assert!(
            skipped
                .iter()
                .any(|warning| warning.contains("already exists"))
        );
        assert_eq!(store.read("imported").await.unwrap()[0]["content"], "local");

        let live = store.ui_history.session("imported").await.unwrap();
        let busy = store
            .import_journal(&snapshot, JournalImportConflict::Overwrite)
            .await
            .unwrap();
        assert!(busy.iter().any(|warning| warning.contains("is in use")));
        assert_eq!(store.read("imported").await.unwrap()[0]["content"], "local");
        drop(live);

        assert!(
            store
                .import_journal(&snapshot, JournalImportConflict::Overwrite)
                .await
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            store.read("imported").await.unwrap()[0]["content"],
            "from-archive"
        );
    }

    #[tokio::test]
    async fn active_segment_ids_include_a_pre_boundary_segment_referenced_after_it() {
        let (store, _dir) = temp_store();
        store
            .append(
                "main",
                &json!({"role": "assistant", "content": "old", "segmentId": "seg-old"}),
            )
            .await
            .unwrap();
        store
            .append(
                "main",
                &json!({"role": "checkpoint", "summary": "s", "messagesSummarized": 1}),
            )
            .await
            .unwrap();
        store
            .append(
                "main",
                &json!({
                    "role": "provider_update",
                    "segmentId": "seg-old",
                    "itemId": "msg",
                    "position": 1,
                    "updateSeq": 1,
                    "payload": {"update_type": "message_done", "text": "x"}
                }),
            )
            .await
            .unwrap();
        let mut ids = std::collections::HashSet::new();
        store
            .with_active_records("main", |event| {
                if let ActiveEvent::Start { segment_ids, .. } = event {
                    ids = segment_ids;
                }
                Ok(())
            })
            .await
            .unwrap();
        assert!(ids.contains(&chelix_common::ProviderSegmentId("seg-old".to_string())));
    }

    #[tokio::test]
    async fn active_read_returns_a_full_page_and_a_pre_checkpoint_page_past_it() {
        let (store, _dir) = temp_store();
        for index in 0..64 {
            store
                .append(
                    "page",
                    &PersistedMessage::user(index.to_string()).to_value(),
                )
                .await
                .unwrap();
        }
        let mut page_rows = 0;
        store
            .with_active_records("page", |event| {
                if matches!(event, ActiveEvent::Row { .. }) {
                    page_rows += 1;
                }
                Ok(())
            })
            .await
            .unwrap();
        assert_eq!(page_rows, 64);

        for index in 0..70 {
            store
                .append(
                    "before",
                    &PersistedMessage::user(index.to_string()).to_value(),
                )
                .await
                .unwrap();
        }
        store
            .append(
                "before",
                &json!({"role": "checkpoint", "summary": "s", "messagesSummarized": 0}),
            )
            .await
            .unwrap();
        let mut before_rows = 0;
        store
            .with_active_records("before", |event| {
                if let ActiveEvent::Row { payload, .. } = event
                    && payload["role"] == "user"
                {
                    before_rows += 1;
                }
                Ok(())
            })
            .await
            .unwrap();
        assert_eq!(before_rows, 70);
    }

    #[tokio::test]
    async fn import_overwrite_replaces_a_ui_row_that_has_no_journal() {
        let (source, dir) = temp_store();
        source
            .append("legacy", &PersistedMessage::user("from-archive").to_value())
            .await
            .unwrap();
        let snapshot = dir.path().join("snapshot.sqlite");
        let snapshot_sql = snapshot.to_string_lossy().replace('\'', "''");
        sqlx::query(&format!("VACUUM INTO '{snapshot_sql}'"))
            .execute(source.pool().await.unwrap())
            .await
            .unwrap();

        let (store, _dir) = temp_store();
        sqlx::query(
            "INSERT INTO ui_history_sessions (session_key, generation, revision, next_position, total_messages) VALUES ('legacy', 'old', 0, 0, 1)",
        )
        .execute(store.pool().await.unwrap())
        .await
        .unwrap();
        let skipped = store
            .import_journal(&snapshot, JournalImportConflict::Skip)
            .await
            .unwrap();
        assert!(
            skipped
                .iter()
                .any(|warning| warning.contains("already exists"))
        );
        let journals = sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM session_journal WHERE session_key = 'legacy'",
        )
        .fetch_one(store.pool().await.unwrap())
        .await
        .unwrap();
        assert_eq!(journals, 0);

        assert!(
            store
                .import_journal(&snapshot, JournalImportConflict::Overwrite)
                .await
                .unwrap()
                .is_empty()
        );
        store
            .ui_history
            .page(
                "legacy",
                crate::ui_history_types::UiHistoryRange::Latest,
                10,
            )
            .await
            .unwrap();
        assert_eq!(
            store.read("legacy").await.unwrap()[0]["content"],
            "from-archive"
        );
    }

    #[tokio::test]
    async fn child_snapshot_without_a_journal_is_empty() {
        let (store, _dir) = temp_store();
        let messages = store.child_snapshot_messages("missing").await.unwrap();
        assert!(messages.is_empty());
    }

    #[tokio::test]
    async fn import_ignores_empty_journals_and_still_imports_nonempty_ones() {
        let (source, dir) = temp_store();
        source.ui_history.session("empty").await.unwrap();
        source
            .append("full", &PersistedMessage::user("from-archive").to_value())
            .await
            .unwrap();
        let snapshot = dir.path().join("snapshot.sqlite");
        let snapshot_sql = snapshot.to_string_lossy().replace('\'', "''");
        sqlx::query(&format!("VACUUM INTO '{snapshot_sql}'"))
            .execute(source.pool().await.unwrap())
            .await
            .unwrap();

        for conflict in [
            JournalImportConflict::Skip,
            JournalImportConflict::Overwrite,
        ] {
            let (store, _dir) = temp_store();
            store
                .append("empty", &PersistedMessage::user("local-empty").to_value())
                .await
                .unwrap();
            store
                .append("full", &PersistedMessage::user("local-full").to_value())
                .await
                .unwrap();
            let warnings = store.import_journal(&snapshot, conflict).await.unwrap();
            assert!(
                !warnings.iter().any(|warning| warning.contains("'empty'")),
                "{warnings:?}"
            );
            assert_eq!(
                store.read("empty").await.unwrap()[0]["content"],
                "local-empty"
            );
            let full = store.read("full").await.unwrap()[0]["content"].clone();
            match conflict {
                JournalImportConflict::Skip => {
                    assert_eq!(full, "local-full");
                    assert!(warnings.iter().any(|warning| warning.contains("'full'")));
                },
                JournalImportConflict::Overwrite => {
                    assert_eq!(full, "from-archive");
                    assert!(warnings.is_empty(), "{warnings:?}");
                },
            }
        }
    }

    #[tokio::test]
    async fn token_totals_include_every_role_and_keep_the_last_assistant() {
        let (store, _dir) = temp_store();
        store
            .append(
                "main",
                &json!({
                    "role": "assistant",
                    "content": "first",
                    "inputTokens": 10,
                    "outputTokens": 2,
                    "cacheReadTokens": 3,
                    "cacheWriteTokens": 4
                }),
            )
            .await
            .unwrap();
        store
            .append(
                "main",
                &json!({
                    "role": "assistant",
                    "content": "second",
                    "inputTokens": 1,
                    "outputTokens": 1,
                    "cacheReadTokens": 1,
                    "cacheWriteTokens": 1
                }),
            )
            .await
            .unwrap();
        store
            .append(
                "main",
                &PersistedMessage::checkpoint("sum", "model", "provider", 7, 8, 2).to_value(),
            )
            .await
            .unwrap();
        let totals = store.token_totals("main").await.unwrap();
        assert_eq!(totals.input_tokens, 18);
        assert_eq!(totals.output_tokens, 11);
        assert_eq!(totals.cache_read_tokens, 4);
        assert_eq!(totals.cache_write_tokens, 5);
        assert_eq!(totals.last_assistant.unwrap()["content"], "second");
        let payloads = store.assistant_payloads("main").await.unwrap();
        assert_eq!(payloads.len(), 2);
        assert_eq!(payloads[0]["content"], "first");
        assert_eq!(payloads[1]["content"], "second");
    }
}
