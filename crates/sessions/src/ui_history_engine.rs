//! Live semantic history with coalesced persistence and revisioned subscriptions.

use std::{
    collections::{BTreeMap, HashMap},
    path::PathBuf,
    sync::{Arc, Mutex, MutexGuard, Weak},
    time::Duration,
};

use {
    sqlx::Row,
    tokio::sync::{Mutex as AsyncMutex, watch},
};

use crate::{
    Error, PersistedMessage, Result,
    ui_history_database::{UiDatabase, UiSessionAccess, integer, unsigned},
    ui_history_projection::{self as projection, UiIngress},
    ui_history_types::*,
};

/// Owns semantic history independently of provider-context journal reads.
pub struct UiHistoryEngine {
    database: Arc<UiDatabase>,
    sessions: AsyncMutex<HashMap<String, Weak<UiHistorySession>>>,
}

#[derive(Clone)]
struct UiRegisteredRun {
    metadata: UiRunMetadata,
    segment_id: Option<chelix_common::ProviderSegmentId>,
    last_error: Option<(UiMessageId, String)>,
}

struct UiState {
    generation: UiGeneration,
    revision: u64,
    committed_revision: u64,
    next_position: u64,
    total_messages: u32,
    canonical_tail: usize,
    entries: BTreeMap<UiMessageId, UiEntry>,
    pending_appends: HashMap<UiMessageId, usize>,
    runs: HashMap<String, UiRegisteredRun>,
    tool_owners: HashMap<UiMessageId, UiMessageId>,
    failure: Option<String>,
}

struct FlushProgress {
    revision: u64,
}

/// Serializes canonical staging, receipts and journal mutations.
#[derive(Default)]
pub(crate) struct UiJournalState {
    pub(crate) canonical_tail: usize,
}

pub struct UiHistorySession {
    key: String,
    database: Arc<UiDatabase>,
    state: Mutex<UiState>,
    flush_progress: AsyncMutex<FlushProgress>,
    pub(crate) journal: AsyncMutex<UiJournalState>,
    published: watch::Sender<UiHistoryRevision>,
    dirty: watch::Sender<u64>,
}

/// A generation-bound, synchronous destination for a copied provider stream.
#[derive(Clone)]
pub struct UiHistoryRun {
    session: Arc<UiHistorySession>,
    generation: UiGeneration,
    run_id: String,
}

pub(crate) struct UiAppendReceipt {
    id: UiMessageId,
    commits_snapshot: bool,
    semantic_record: bool,
    generation: UiGeneration,
    content_version: u64,
}

#[cfg(test)]
#[path = "ui_history_tests.rs"]
mod tests;

impl UiHistoryEngine {
    #[must_use]
    pub fn new(directory: PathBuf) -> Self {
        Self {
            database: UiDatabase::new(directory),
            sessions: AsyncMutex::new(HashMap::new()),
        }
    }

    /// Initialize the database and apply its schema migrations.
    pub async fn initialize(&self) -> Result<()> {
        self.database.pool().await?;
        Ok(())
    }

    pub async fn session(&self, key: &str) -> Result<Arc<UiHistorySession>> {
        self.load_session(key, UiSessionAccess::Open).await
    }

    pub(crate) async fn session_for_clear(&self, key: &str) -> Result<Arc<UiHistorySession>> {
        self.load_session(key, UiSessionAccess::Discard).await
    }

    async fn load_session(
        &self,
        key: &str,
        access: UiSessionAccess,
    ) -> Result<Arc<UiHistorySession>> {
        let mut sessions = self.sessions.lock().await;
        if let Some(session) = sessions.get(key).and_then(Weak::upgrade) {
            return Ok(session);
        }
        let row = self.database.session(key, access).await?;
        let revision = UiHistoryRevision {
            session_key: key.to_string(),
            generation: row.generation.clone(),
            revision: row.revision,
            total_messages: row.total_messages,
            failure: row.failure.clone(),
        };
        let (published, _) = watch::channel(revision);
        let (dirty, mut pending) = watch::channel(row.revision);
        let session = Arc::new(UiHistorySession {
            key: key.to_string(),
            database: Arc::clone(&self.database),
            state: Mutex::new(UiState {
                generation: row.generation,
                revision: row.revision,
                committed_revision: row.revision,
                next_position: row.next_position,
                total_messages: row.total_messages,
                canonical_tail: row.canonical_tail,
                entries: BTreeMap::new(),
                pending_appends: HashMap::new(),
                runs: HashMap::new(),
                tool_owners: HashMap::new(),
                failure: row.failure,
            }),
            flush_progress: AsyncMutex::new(FlushProgress {
                revision: row.revision,
            }),
            journal: AsyncMutex::new(UiJournalState {
                canonical_tail: row.canonical_tail,
            }),
            published,
            dirty,
        });
        let weak = Arc::downgrade(&session);
        tokio::spawn(async move {
            while pending.changed().await.is_ok() {
                tokio::time::sleep(Duration::from_millis(50)).await;
                let Some(session) = weak.upgrade() else {
                    break;
                };
                if let Err(error) = session.flush().await {
                    session.fail(&error);
                    break;
                }
            }
        });
        sessions.retain(|_, session| session.strong_count() > 0);
        sessions.insert(key.to_string(), Arc::downgrade(&session));
        Ok(session)
    }

    pub async fn page(
        &self,
        key: &str,
        range: UiHistoryRange,
        limit: usize,
    ) -> Result<UiHistoryPage> {
        self.session(key).await?.page(range, limit).await
    }

    pub async fn count(&self, key: &str) -> Result<u32> {
        let session = self.session(key).await?;
        let state = session.lock()?;
        state.check()?;
        Ok(state.total_messages)
    }

    pub async fn history(&self, key: &str) -> Result<Vec<serde_json::Value>> {
        let page = self
            .page(key, UiHistoryRange::Latest, u32::MAX as usize)
            .await?;
        page.history.iter().map(UiSnapshot::public_value).collect()
    }

    pub async fn search(
        &self,
        keys: &[String],
        query: &str,
        limit: usize,
    ) -> Result<Vec<UiSearchHit>> {
        if query.trim().is_empty() || limit == 0 {
            return Ok(Vec::new());
        }
        let query = query.to_lowercase();
        let mut hits = Vec::new();
        for key in keys {
            let session = match self.session(key).await {
                Ok(session) => session,
                Err(error @ Error::MissingUiSnapshots { .. }) => {
                    tracing::warn!(session_key = %key, %error, "excluding session without UI snapshots from search");
                    continue;
                },
                Err(error) => return Err(error),
            };
            if let Some(hit) = session.search(&query).await? {
                hits.push(hit);
            }
            if hits.len() == limit {
                break;
            }
        }
        Ok(hits)
    }
}

impl UiState {
    fn prepare(&self, entries: BTreeMap<UiMessageId, UiEntry>) -> Self {
        Self {
            generation: self.generation.clone(),
            revision: self.revision,
            committed_revision: self.committed_revision,
            next_position: self.next_position,
            total_messages: self.total_messages,
            canonical_tail: self.canonical_tail,
            entries,
            pending_appends: HashMap::new(),
            runs: self.runs.clone(),
            tool_owners: self.tool_owners.clone(),
            failure: self.failure.clone(),
        }
    }

    fn project(&mut self, id: UiMessageId, record: UiRecord, ingress: UiIngress) -> Result<()> {
        let run = record
            .message
            .run_id()
            .and_then(|run_id| self.runs.get(run_id))
            .map(|run| &run.metadata);
        let mut entry = self.entries.get(&id).cloned().unwrap_or_else(|| {
            projection::initial_entry(id.clone(), self.next_position, record.clone())
        });
        if !projection::project(&mut entry, record, ingress, run)? {
            return Ok(());
        }
        if !self.entries.contains_key(&id) {
            entry.snapshot.position = self.allocate()?;
        }
        entry.content_version = entry
            .content_version
            .checked_add(1)
            .ok_or_else(|| Error::message("UI content version overflow"))?;
        entry.snapshot.canonical_committed = false;
        entry.snapshot.assistant_id = self
            .tool_owners
            .get(&id)
            .cloned()
            .or(entry.snapshot.assistant_id);
        if let UiContent::Record(record) = &entry.snapshot.content
            && let PersistedMessage::Assistant {
                tool_calls: Some(calls),
                run_id: Some(run_id),
                ..
            } = &record.message
        {
            for call in calls {
                self.tool_owners
                    .insert(UiMessageId::tool(run_id, &call.id), id.clone());
            }
        }
        if ingress == UiIngress::Live
            && let Some(run_id) = entry.snapshot.content.run_id()
            && let Some(run) = self.runs.get_mut(run_id)
            && let Some(materializer) = &entry.materializer
        {
            run.segment_id = materializer.segment.segment_id.clone();
        }
        entry.snapshot.revision = self.advance()?;
        self.entries.insert(id, entry);
        Ok(())
    }

    fn commit(&mut self, prepared: Self) {
        self.revision = prepared.revision;
        self.next_position = prepared.next_position;
        self.total_messages = prepared.total_messages;
        self.entries.extend(prepared.entries);
        self.tool_owners = prepared.tool_owners;
        self.runs = prepared.runs;
        for (id, entry) in &mut self.entries {
            if let Some(owner) = self.tool_owners.get(id)
                && entry.snapshot.assistant_id.as_ref() != Some(owner)
            {
                entry.snapshot.assistant_id = Some(owner.clone());
                entry.snapshot.revision = self.revision;
            }
        }
    }

    fn check(&self) -> Result<()> {
        if let Some(failure) = &self.failure {
            return Err(Error::message(failure.clone()));
        }
        Ok(())
    }

    fn advance(&mut self) -> Result<u64> {
        self.revision = self
            .revision
            .checked_add(1)
            .ok_or_else(|| Error::message("UI history revision overflow"))?;
        integer(self.revision)?;
        Ok(self.revision)
    }

    fn allocate(&mut self) -> Result<u64> {
        let position = self.next_position;
        self.next_position = position
            .checked_add(1)
            .ok_or_else(|| Error::message("UI history position overflow"))?;
        integer(self.next_position)?;
        self.total_messages = self
            .total_messages
            .checked_add(1)
            .ok_or_else(|| Error::message("UI history count overflow"))?;
        Ok(position)
    }
}

impl UiHistorySession {
    fn lock(&self) -> Result<MutexGuard<'_, UiState>> {
        self.state
            .lock()
            .map_err(|error| Error::lock_failed(error.to_string()))
    }

    fn publish(&self, state: &UiState) {
        self.published.send_replace(UiHistoryRevision {
            session_key: self.key.clone(),
            generation: state.generation.clone(),
            revision: state.revision,
            total_messages: state.total_messages,
            failure: state.failure.clone(),
        });
        self.dirty.send_replace(state.revision);
    }

    pub fn subscribe(&self) -> watch::Receiver<UiHistoryRevision> {
        self.published.subscribe()
    }

    pub fn begin_run(self: &Arc<Self>, metadata: UiRunMetadata) -> Result<UiHistoryRun> {
        let mut state = self.lock()?;
        state.check()?;
        let run_id = metadata.run_id.clone();
        if state.runs.contains_key(&run_id) {
            return Err(Error::message("UI history run is already registered"));
        }
        state.runs.insert(run_id.clone(), UiRegisteredRun {
            metadata,
            segment_id: None,
            last_error: None,
        });
        Ok(UiHistoryRun {
            session: Arc::clone(self),
            generation: state.generation.clone(),
            run_id,
        })
    }

    pub fn fail(&self, error: &Error) {
        tracing::error!(session_key = self.key, %error, "UI history failed");
        if let Ok(mut state) = self.lock()
            && state.failure.is_none()
        {
            state.failure = Some(error.to_string());
            if let Err(revision_error) = state.advance() {
                tracing::error!(%revision_error, "failed to advance UI failure revision");
            }
            self.publish(&state);
        }
    }

    pub fn record_error(&self, error: UiProviderError) -> Result<UiMessageId> {
        self.insert_error(error, None)
    }

    fn insert_error(
        &self,
        mut error: UiProviderError,
        expected: Option<&UiGeneration>,
    ) -> Result<UiMessageId> {
        let mut state = self.lock()?;
        state.check()?;
        if expected.is_some_and(|generation| {
            generation != &state.generation || !state.runs.contains_key(&error.run_id)
        }) {
            return Err(Error::message("obsolete UI history run"));
        }
        let id = UiMessageId(uuid::Uuid::new_v4().to_string());
        if let Some(run) = state.runs.get_mut(&error.run_id) {
            if error.segment_id.is_none() {
                error.segment_id = run.segment_id.clone();
            }
            run.last_error = Some((id.clone(), error.raw.clone()));
        }
        let position = state.allocate()?;
        let revision = state.advance()?;
        state.entries.insert(id.clone(), UiEntry {
            snapshot: UiSnapshot {
                id: id.clone(),
                position,
                revision,
                canonical_committed: false,
                content: UiContent::Error(UiErrorMessage::Error { error }),
                presentation: UiPresentation::default(),
                accumulated_arguments: None,
                assistant_id: None,
                outcome: None,
            },
            canonical: None,
            content_version: 0,
            materializer: None,
        });
        self.publish(&state);
        Ok(id)
    }

    async fn entry(&self, id: &UiMessageId) -> Result<Option<UiEntry>> {
        loop {
            let (generation, committed) = {
                let state = self.lock()?;
                state.check()?;
                if let Some(entry) = state.entries.get(id) {
                    return Ok(Some(entry.clone()));
                }
                (state.generation.clone(), state.committed_revision)
            };
            let stored = self.database.entry(&self.key, id).await?;
            let state = self.lock()?;
            state.check()?;
            if state.generation != generation {
                return Err(Error::message("UI history generation changed"));
            }
            if state.committed_revision != committed {
                continue;
            }
            return Ok(state.entries.get(id).cloned().or(stored));
        }
    }

    fn ingest(
        &self,
        record: UiRecord,
        ingress: UiIngress,
        expected: Option<&UiGeneration>,
    ) -> Result<UiMessageId> {
        let mut state = self.lock()?;
        state.check()?;
        if expected.is_some_and(|expected| expected != &state.generation)
            || record
                .message
                .run_id()
                .is_none_or(|run_id| !state.runs.contains_key(run_id))
        {
            return Err(Error::message(
                "copied stream belongs to an inactive UI history run",
            ));
        }
        let id = projection::identity(&record)?
            .unwrap_or_else(|| UiMessageId(uuid::Uuid::new_v4().to_string()));
        let entries = state
            .entries
            .get(&id)
            .map(|entry| (id.clone(), entry.clone()))
            .into_iter()
            .collect();
        let mut prepared = state.prepare(entries);
        prepared.project(id.clone(), record, ingress)?;
        state.commit(prepared);
        self.publish(&state);
        Ok(id)
    }

    pub(crate) async fn stage_batch(&self, records: Vec<UiRecord>) -> Result<Vec<UiAppendReceipt>> {
        let generation = self.lock()?.generation.clone();
        let mut loaded = BTreeMap::new();
        let mut identified = Vec::with_capacity(records.len());
        for record in records {
            let id = projection::identity(&record)?
                .unwrap_or_else(|| UiMessageId(uuid::Uuid::new_v4().to_string()));
            if let Some(entry) = self.entry(&id).await? {
                loaded.insert(id.clone(), entry);
            }
            identified.push((id, record));
        }
        let mut state = self.lock()?;
        state.check()?;
        if state.generation != generation {
            return Err(Error::message("UI history changed during staging"));
        }
        for (id, _) in &identified {
            if let Some(entry) = state.entries.get(id) {
                loaded.insert(id.clone(), entry.clone());
            }
        }
        let mut prepared = state.prepare(loaded);
        let mut receipts = Vec::with_capacity(identified.len());
        for (id, record) in identified {
            if matches!(record.message, PersistedMessage::User { .. })
                && prepared.entries.contains_key(&id)
            {
                return Err(Error::message(
                    "clientMessageId already exists in this session",
                ));
            }
            let semantic_record = !matches!(
                record.message,
                PersistedMessage::ProviderUpdate { .. }
                    | PersistedMessage::ProviderSegmentClose { .. }
            );
            let mut commits_snapshot =
                !matches!(record.message, PersistedMessage::ProviderUpdate { .. });
            if let PersistedMessage::ToolLifecycle { lifecycle } = &record.message
                && let Some(entry) = prepared.entries.get(&id)
                && let UiContent::Record(existing) = &entry.snapshot.content
                && let PersistedMessage::ToolLifecycle {
                    lifecycle: previous,
                } = &existing.message
            {
                commits_snapshot = lifecycle.sequence >= previous.sequence;
            }
            prepared.project(id.clone(), record, UiIngress::Journal)?;
            let content_version = prepared
                .entries
                .get(&id)
                .ok_or_else(|| Error::message("staged UI entry is missing"))?
                .content_version;
            receipts.push(UiAppendReceipt {
                id,
                commits_snapshot,
                semantic_record,
                generation: generation.clone(),
                content_version,
            });
        }
        state.commit(prepared);
        for receipt in &receipts {
            *state.pending_appends.entry(receipt.id.clone()).or_default() += 1;
        }
        self.publish(&state);
        Ok(receipts)
    }

    pub(crate) fn bind(&self, receipt: &UiAppendReceipt, index: usize) -> Result<()> {
        let mut state = self.lock()?;
        state.check()?;
        if state.generation != receipt.generation
            || !state.pending_appends.contains_key(&receipt.id)
        {
            return Err(Error::message("obsolete or duplicate UI append receipt"));
        }
        let end = index
            .checked_add(1)
            .ok_or_else(|| Error::message("canonical history index overflow"))?;
        let revision = state.advance()?;
        let entry = state
            .entries
            .get_mut(&receipt.id)
            .ok_or_else(|| Error::message("UI append receipt has no snapshot"))?;
        entry.canonical = Some(match entry.canonical {
            Some(binding) => UiCanonicalBinding {
                start: binding.start.min(index),
                end: binding.end.max(end),
                record_index: if receipt.semantic_record {
                    Some(index)
                } else {
                    binding.record_index
                },
            },
            None => UiCanonicalBinding {
                start: index,
                end,
                record_index: receipt.semantic_record.then_some(index),
            },
        });
        entry.snapshot.canonical_committed |=
            receipt.commits_snapshot && entry.content_version == receipt.content_version;
        entry.snapshot.revision = revision;
        state.canonical_tail = state.canonical_tail.max(end);
        if let Some(pending) = state.pending_appends.get_mut(&receipt.id) {
            *pending -= 1;
            if *pending == 0 {
                state.pending_appends.remove(&receipt.id);
            }
        }
        self.publish(&state);
        Ok(())
    }

    pub(crate) async fn flush_if_idle(&self) -> Result<()> {
        let idle = self.lock()?.runs.is_empty();
        if idle {
            self.flush().await?;
        }
        Ok(())
    }

    /// Wait for all snapshots observed at entry, independently of socket listeners.
    pub async fn flush(&self) -> Result<()> {
        let mut progress = self.flush_progress.lock().await;
        let (generation, revision, next_position, count, canonical_tail, failure, entries) = {
            let state = self.lock()?;
            if state.revision == progress.revision {
                return state.check();
            }
            (
                state.generation.clone(),
                state.revision,
                state.next_position,
                state.total_messages,
                state.canonical_tail,
                state.failure.clone(),
                state
                    .entries
                    .values()
                    .filter(|entry| entry.snapshot.revision > progress.revision)
                    .cloned()
                    .collect::<Vec<_>>(),
            )
        };
        let pool = self.database.pool().await?;
        let mut transaction = pool.begin().await?;
        let updated = sqlx::query("UPDATE ui_history_sessions SET revision = ?, next_position = ?, total_messages = ?, canonical_tail = ?, failure = ? WHERE session_key = ? AND generation = ? AND revision = ?")
            .bind(integer(revision)?).bind(integer(next_position)?).bind(i64::from(count))
            .bind(i64::try_from(canonical_tail).map_err(|error| Error::message(error.to_string()))?).bind(&failure)
            .bind(&self.key).bind(&generation.0).bind(integer(progress.revision)?)
            .execute(&mut *transaction).await?.rows_affected();
        if updated != 1 {
            return Err(Error::message(
                "UI history persistence generation/revision conflict",
            ));
        }
        crate::ui_history_database::write_entries(&mut transaction, &self.key, &entries).await?;
        transaction.commit().await?;
        let mut state = self.lock()?;
        if state.generation != generation {
            return Err(Error::message("UI history changed during flush"));
        }
        progress.revision = revision;
        state.committed_revision = revision;
        let active_runs = state
            .runs
            .keys()
            .cloned()
            .collect::<std::collections::HashSet<_>>();
        let pending = state
            .pending_appends
            .keys()
            .cloned()
            .collect::<std::collections::HashSet<_>>();
        state.entries.retain(|id, entry| {
            pending.contains(id)
                || entry.snapshot.revision > revision
                || projection::active(entry)
                || entry
                    .snapshot
                    .content
                    .run_id()
                    .is_some_and(|run_id| active_runs.contains(run_id))
        });
        state.check()
    }

    pub async fn page(&self, range: UiHistoryRange, limit: usize) -> Result<UiHistoryPage> {
        if limit == 0 {
            return Err(Error::message("UI history page limit must be positive"));
        }
        let around_target = if let UiHistoryRange::Around { message_id } = &range {
            let generation = self.lock()?.generation.clone();
            let entry = self
                .entry(message_id)
                .await?
                .ok_or_else(|| Error::message("UI history target was removed"))?;
            Some((generation, entry.snapshot.position))
        } else {
            None
        };
        loop {
            let (generation, committed, lower, upper, newest, around) = {
                let state = self.lock()?;
                state.check()?;
                let (lower, upper, newest, around) = match &range {
                    UiHistoryRange::Latest => (None, None, true, None),
                    UiHistoryRange::Before { position } => (None, Some(*position), true, None),
                    UiHistoryRange::After { position } => (Some(*position), None, false, None),
                    UiHistoryRange::Window { start, end } => {
                        if end.is_some_and(|end| end <= *start) {
                            return Err(Error::message("invalid UI history window"));
                        }
                        (start.checked_sub(1), *end, end.is_none(), None)
                    },
                    UiHistoryRange::Around { .. } => {
                        let (generation, position) = around_target
                            .as_ref()
                            .ok_or_else(|| Error::message("UI history target is missing"))?;
                        if &state.generation != generation {
                            return Err(Error::message("UI history generation changed"));
                        }
                        (None, None, true, Some(*position))
                    },
                };
                (
                    state.generation.clone(),
                    state.committed_revision,
                    lower,
                    upper,
                    newest,
                    around,
                )
            };
            let mut stored = if let Some(position) = around {
                let mut rows = self
                    .database
                    .entries(&self.key, None, Some(position), true, limit)
                    .await?;
                rows.extend(
                    self.database
                        .entries(&self.key, position.checked_sub(1), None, false, limit)
                        .await?,
                );
                rows
            } else {
                self.database
                    .entries(&self.key, lower, upper, newest, limit)
                    .await?
            };
            let state = self.lock()?;
            state.check()?;
            if state.generation != generation || state.committed_revision != committed {
                continue;
            }
            let mut merged = BTreeMap::new();
            for entry in stored.drain(..) {
                merged.insert(entry.snapshot.id.clone(), entry.snapshot);
            }
            for entry in state.entries.values() {
                merged.insert(entry.snapshot.id.clone(), entry.snapshot.clone());
            }
            let mut history = merged
                .into_values()
                .filter(|snapshot| {
                    lower.is_none_or(|lower| snapshot.position > lower)
                        && upper.is_none_or(|upper| snapshot.position < upper)
                })
                .collect::<Vec<_>>();
            if let Some(position) = around {
                history.sort_by_key(|snapshot| {
                    (snapshot.position.abs_diff(position), snapshot.position)
                });
            } else if newest {
                history.sort_by_key(|snapshot| std::cmp::Reverse(snapshot.position));
            } else {
                history.sort_by_key(|snapshot| snapshot.position);
            }
            history.truncate(limit);
            history.sort_by_key(|snapshot| snapshot.position);
            let first_position = history.first().map(|snapshot| snapshot.position);
            let last_position = history.last().map(|snapshot| snapshot.position);
            return Ok(UiHistoryPage {
                generation,
                revision: state.revision,
                total_messages: state.total_messages,
                has_older: first_position.is_some_and(|position| position > 0),
                has_newer: last_position.is_some_and(|position| position + 1 < state.next_position),
                first_position,
                last_position,
                history,
            });
        }
    }

    async fn search(&self, query: &str) -> Result<Option<UiSearchHit>> {
        loop {
            let (generation, committed, dirty_count) = {
                let state = self.lock()?;
                state.check()?;
                (
                    state.generation.clone(),
                    state.committed_revision,
                    state.entries.len(),
                )
            };
            let rows = self
                .database
                .search(&self.key, query, dirty_count.saturating_add(1))
                .await?;
            let state = self.lock()?;
            state.check()?;
            if state.generation != generation
                || state.committed_revision != committed
                || state.entries.len() > dirty_count
            {
                continue;
            }
            let mut entries = rows
                .into_iter()
                .map(|entry| (entry.snapshot.id.clone(), entry))
                .collect::<BTreeMap<_, _>>();
            entries.extend(state.entries.clone());
            let mut candidates = entries.into_values().collect::<Vec<_>>();
            candidates.sort_by_key(|entry| entry.snapshot.position);
            for entry in candidates {
                let text = projection::search_text(&entry.snapshot)?;
                let Some(snippet) = projection::search_snippet(&text, query) else {
                    continue;
                };
                let value = entry.snapshot.public_value()?;
                let role = value
                    .get("role")
                    .and_then(serde_json::Value::as_str)
                    .ok_or_else(|| Error::message("semantic snapshot has no role"))?
                    .to_string();
                return Ok(Some(UiSearchHit {
                    session_key: self.key.clone(),
                    generation,
                    message_id: entry.snapshot.id,
                    position: entry.snapshot.position,
                    snippet,
                    role,
                }));
            }
            return Ok(None);
        }
    }

    pub async fn updates_since(
        &self,
        generation: &UiGeneration,
        revision: u64,
        limit: usize,
    ) -> Result<Option<UiHistoryBatch>> {
        if limit == 0 {
            return Err(Error::message("UI update limit must be positive"));
        }
        let query_limit = limit
            .checked_add(1)
            .and_then(|value| i64::try_from(value).ok())
            .ok_or_else(|| Error::message("UI update limit overflow"))?;
        loop {
            let committed = {
                let state = self.lock()?;
                state.check()?;
                if &state.generation != generation {
                    return Ok(None);
                }
                if revision > state.revision {
                    return Err(Error::message("UI revision is ahead of the server"));
                }
                state.committed_revision
            };
            let rows = if committed > revision {
                sqlx::query_scalar::<_, String>("SELECT snapshot_json FROM ui_history_snapshots WHERE session_key = ? AND revision > ? ORDER BY position LIMIT ?")
                    .bind(&self.key).bind(integer(revision)?).bind(query_limit).fetch_all(self.database.pool().await?).await?
            } else {
                Vec::new()
            };
            let mut entries = BTreeMap::new();
            for json in rows {
                let entry: UiEntry = serde_json::from_str(&json)?;
                entries.insert(entry.snapshot.id.clone(), entry.snapshot);
            }
            let state = self.lock()?;
            state.check()?;
            if &state.generation != generation {
                return Ok(None);
            }
            if state.committed_revision != committed {
                continue;
            }
            if entries.len() > limit {
                return Ok(None);
            }
            for entry in state
                .entries
                .values()
                .filter(|entry| entry.snapshot.revision > revision)
            {
                entries.insert(entry.snapshot.id.clone(), entry.snapshot.clone());
                if entries.len() > limit {
                    return Ok(None);
                }
            }
            let mut history = entries.into_values().collect::<Vec<_>>();
            history.sort_by_key(|snapshot| snapshot.position);
            return Ok(Some(UiHistoryBatch {
                generation: state.generation.clone(),
                from_revision: revision,
                revision: state.revision,
                total_messages: state.total_messages,
                history,
            }));
        }
    }

    pub async fn read_run(&self, run_id: &str) -> Result<Vec<UiSnapshot>> {
        loop {
            let (generation, committed) = {
                let state = self.lock()?;
                state.check()?;
                (state.generation.clone(), state.committed_revision)
            };
            let rows = sqlx::query_scalar::<_, String>("SELECT snapshot_json FROM ui_history_snapshots WHERE session_key = ? AND run_id = ? ORDER BY position")
                .bind(&self.key).bind(run_id).fetch_all(self.database.pool().await?).await?;
            let mut entries = BTreeMap::new();
            for json in rows {
                let entry: UiEntry = serde_json::from_str(&json)?;
                entries.insert(entry.snapshot.id.clone(), entry.snapshot);
            }
            let state = self.lock()?;
            state.check()?;
            if state.generation != generation || state.committed_revision != committed {
                continue;
            }
            for entry in state.entries.values() {
                entries.remove(&entry.snapshot.id);
                if entry.snapshot.content.run_id() == Some(run_id) {
                    entries.insert(entry.snapshot.id.clone(), entry.snapshot.clone());
                }
            }
            let mut snapshots = entries.into_values().collect::<Vec<_>>();
            snapshots.sort_by_key(|snapshot| snapshot.position);
            return Ok(snapshots);
        }
    }

    pub(crate) async fn prepare_record_update(&self, index: usize) -> Result<UiRecordUpdate> {
        loop {
            let (generation, committed) = {
                let state = self.lock()?;
                state.check()?;
                (state.generation.clone(), state.committed_revision)
            };
            let stored = sqlx::query_scalar::<_, String>("SELECT snapshot_json FROM ui_history_snapshots WHERE session_key = ? AND canonical_record = ?")
                .bind(&self.key).bind(i64::try_from(index).map_err(|error| Error::message(error.to_string()))?)
                .fetch_optional(self.database.pool().await?).await?;
            let state = self.lock()?;
            state.check()?;
            if state.generation != generation || state.committed_revision != committed {
                continue;
            }
            let entry = state
                .entries
                .values()
                .find(|entry| {
                    entry
                        .canonical
                        .is_some_and(|binding| binding.record_index == Some(index))
                })
                .cloned()
                .or(stored
                    .map(|json| serde_json::from_str::<UiEntry>(&json))
                    .transpose()?)
                .ok_or_else(|| Error::message("canonical record has no semantic snapshot"))?;
            if state.pending_appends.contains_key(&entry.snapshot.id) {
                return Err(Error::message("canonical record still has pending appends"));
            }
            return Ok(UiRecordUpdate { generation, entry });
        }
    }

    pub(crate) fn validate_record_update(entry: &UiEntry, record: UiRecord) -> Result<UiEntry> {
        if !entry.snapshot.canonical_committed || projection::active(entry) {
            return Err(Error::message(
                "only confirmed inactive records can be updated",
            ));
        }
        let UiContent::Record(existing) = &entry.snapshot.content else {
            return Err(Error::message(
                "canonical update requires a semantic record",
            ));
        };
        if std::mem::discriminant(&existing.message) != std::mem::discriminant(&record.message)
            || existing.message.run_id() != record.message.run_id()
            || existing.client_message_id != record.client_message_id
            || projection::identity(existing)? != projection::identity(&record)?
        {
            return Err(Error::message(
                "canonical update cannot change message identity",
            ));
        }
        let mut updated = entry.clone();
        if !projection::project(&mut updated, record, UiIngress::Journal, None)? {
            return Err(Error::message(
                "canonical update did not advance its semantic record",
            ));
        }
        updated.content_version = updated
            .content_version
            .checked_add(1)
            .ok_or_else(|| Error::message("UI content version overflow"))?;
        Ok(updated)
    }

    pub(crate) async fn update_record(
        &self,
        expected: &UiRecordUpdate,
        record: UiRecord,
    ) -> Result<()> {
        let index = expected
            .entry
            .canonical
            .and_then(|binding| binding.record_index)
            .ok_or_else(|| Error::message("canonical update target has no record index"))?;
        let current = self.prepare_record_update(index).await?;
        {
            let mut state = self.lock()?;
            state.check()?;
            let current = state
                .entries
                .get(&current.entry.snapshot.id)
                .unwrap_or(&current.entry);
            if state.generation != expected.generation
                || current.snapshot.id != expected.entry.snapshot.id
                || current.canonical != expected.entry.canonical
                || current.content_version != expected.entry.content_version
                || state.pending_appends.contains_key(&current.snapshot.id)
            {
                return Err(Error::message(
                    "canonical update target changed during file write",
                ));
            }
            let mut entry = Self::validate_record_update(current, record)?;
            entry.snapshot.revision = state.advance()?;
            state.entries.insert(entry.snapshot.id.clone(), entry);
            self.publish(&state);
        }
        self.flush().await
    }

    pub(crate) async fn fork_snapshot(
        &self,
        position: Option<u64>,
    ) -> Result<crate::ui_history_fork::UiForkSnapshot> {
        loop {
            let (generation, committed) = {
                let state = self.lock()?;
                state.check()?;
                if !state.pending_appends.is_empty() {
                    return Err(Error::message("canonical appends are still pending"));
                }
                (state.generation.clone(), state.committed_revision)
            };
            let stored = self
                .database
                .entries(&self.key, None, None, false, u32::MAX as usize)
                .await?;
            let state = self.lock()?;
            state.check()?;
            if state.generation != generation || state.committed_revision != committed {
                continue;
            }
            let mut entries = stored
                .into_iter()
                .map(|entry| (entry.snapshot.id.clone(), entry))
                .collect::<BTreeMap<_, _>>();
            entries.extend(state.entries.clone());
            return crate::ui_history_fork::select_prefix(
                entries.into_values().collect(),
                position,
                state.next_position,
            );
        }
    }

    pub(crate) fn ensure_empty(&self) -> Result<()> {
        let state = self.lock()?;
        state.check()?;
        if state.total_messages != 0
            || state.canonical_tail != 0
            || !state.runs.is_empty()
            || !state.pending_appends.is_empty()
        {
            return Err(Error::message("fork destination is not empty"));
        }
        Ok(())
    }

    pub(crate) async fn validate_canonical_cut(&self, boundary: usize) -> Result<()> {
        self.flush().await?;
        let boundary =
            i64::try_from(boundary).map_err(|error| Error::message(error.to_string()))?;
        let crossing: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM ui_history_snapshots WHERE session_key = ? AND canonical_start < ? AND canonical_end > ?)")
            .bind(&self.key).bind(boundary).bind(boundary).fetch_one(self.database.pool().await?).await?;
        if crossing {
            return Err(Error::message(
                "mutation would split an interleaved canonical segment",
            ));
        }
        Ok(())
    }

    pub(crate) async fn copy_prefix_from(
        &self,
        snapshot: crate::ui_history_fork::UiForkSnapshot,
    ) -> Result<()> {
        let mut progress = self.flush_progress.lock().await;
        self.ensure_empty()?;
        let canonical_tail = snapshot.canonical_tail;
        let pool = self.database.pool().await?;
        let mut transaction = pool.begin().await?;
        crate::ui_history_database::write_entries(&mut transaction, &self.key, &snapshot.entries)
            .await?;
        let row = sqlx::query("SELECT COUNT(*) AS count, COALESCE(MAX(revision), 0) AS revision, COALESCE(MAX(position) + 1, 0) AS next_position FROM ui_history_snapshots WHERE session_key = ?")
            .bind(&self.key).fetch_one(&mut *transaction).await?;
        let count = u32::try_from(unsigned(row.try_get("count")?)?)
            .map_err(|error| Error::message(error.to_string()))?;
        let revision = unsigned(row.try_get("revision")?)?;
        let next_position = unsigned(row.try_get("next_position")?)?;
        sqlx::query("UPDATE ui_history_sessions SET revision = ?, next_position = ?, total_messages = ?, canonical_tail = ? WHERE session_key = ?")
            .bind(integer(revision)?).bind(integer(next_position)?).bind(i64::from(count))
            .bind(i64::try_from(canonical_tail).map_err(|error| Error::message(error.to_string()))?).bind(&self.key)
            .execute(&mut *transaction).await?;
        transaction.commit().await?;
        let mut state = self.lock()?;
        state.revision = revision;
        state.committed_revision = revision;
        state.next_position = next_position;
        state.total_messages = count;
        state.canonical_tail = canonical_tail;
        progress.revision = revision;
        self.publish(&state);
        Ok(())
    }

    pub async fn canonical_index(&self, target: &UiHistoryTarget) -> Result<usize> {
        let stored = self.entry(&target.message_id).await?;
        let state = self.lock()?;
        state.check()?;
        if state.generation != target.generation {
            return Err(Error::message("obsolete UI history generation"));
        }
        let entry = state
            .entries
            .get(&target.message_id)
            .or(stored.as_ref())
            .ok_or_else(|| Error::message("UI message was removed"))?;
        if !entry.snapshot.canonical_committed {
            return Err(Error::message(
                "UI message has not been committed to the canonical journal",
            ));
        }
        entry
            .canonical
            .and_then(|binding| binding.record_index)
            .ok_or_else(|| Error::message("UI message has no canonical record"))
    }

    pub async fn update_presentation(
        &self,
        target: &UiHistoryTarget,
        presentation: UiPresentation,
    ) -> Result<()> {
        let stored = self.entry(&target.message_id).await?;
        {
            let mut state = self.lock()?;
            state.check()?;
            if state.generation != target.generation {
                return Err(Error::message("obsolete UI history generation"));
            }
            let mut entry = state
                .entries
                .get(&target.message_id)
                .cloned()
                .or(stored)
                .ok_or_else(|| Error::message("UI message was removed"))?;
            entry.snapshot.presentation = presentation;
            entry.snapshot.revision = state.advance()?;
            state.entries.insert(target.message_id.clone(), entry);
            self.publish(&state);
        }
        self.flush().await
    }

    pub async fn truncate(&self, canonical_tail: usize) -> Result<()> {
        let mut progress = self.flush_progress.lock().await;
        let (previous, revision) = {
            let state = self.lock()?;
            (
                state.generation.clone(),
                state
                    .revision
                    .checked_add(1)
                    .ok_or_else(|| Error::message("UI revision overflow"))?,
            )
        };
        let pool = self.database.pool().await?;
        let mut transaction = pool.begin().await?;
        let boundary =
            i64::try_from(canonical_tail).map_err(|error| Error::message(error.to_string()))?;
        let cutoff: Option<i64> = sqlx::query_scalar("SELECT MIN(position) FROM ui_history_snapshots WHERE session_key = ? AND canonical_start >= ?")
            .bind(&self.key).bind(boundary).fetch_one(&mut *transaction).await?;
        if canonical_tail == 0 {
            sqlx::query("DELETE FROM ui_history_snapshots WHERE session_key = ?")
                .bind(&self.key)
                .execute(&mut *transaction)
                .await?;
        } else {
            sqlx::query("DELETE FROM ui_history_snapshots WHERE session_key = ? AND (canonical_end > ? OR (? IS NOT NULL AND position >= ?))")
                .bind(&self.key).bind(boundary).bind(cutoff).bind(cutoff).execute(&mut *transaction).await?;
        }
        let row = sqlx::query("SELECT COUNT(*) AS count, COALESCE(MAX(position) + 1, 0) AS next_position FROM ui_history_snapshots WHERE session_key = ?")
            .bind(&self.key).fetch_one(&mut *transaction).await?;
        let count = u32::try_from(unsigned(row.try_get("count")?)?)
            .map_err(|error| Error::message(error.to_string()))?;
        let next_position = unsigned(row.try_get("next_position")?)?;
        let generation = UiGeneration(uuid::Uuid::new_v4().to_string());
        let affected = sqlx::query("UPDATE ui_history_sessions SET generation = ?, revision = ?, next_position = ?, total_messages = ?, canonical_tail = ?, failure = NULL WHERE session_key = ? AND generation = ?")
            .bind(&generation.0).bind(integer(revision)?).bind(integer(next_position)?).bind(i64::from(count))
            .bind(boundary).bind(&self.key).bind(&previous.0).execute(&mut *transaction).await?.rows_affected();
        if affected != 1 {
            return Err(Error::message("UI mutation generation conflict"));
        }
        transaction.commit().await?;
        let mut state = self.lock()?;
        state.generation = generation;
        state.revision = revision;
        state.committed_revision = revision;
        state.next_position = next_position;
        state.total_messages = count;
        state.canonical_tail = canonical_tail;
        state.entries.clear();
        state.pending_appends.clear();
        state.runs.clear();
        state.tool_owners.clear();
        state.failure = None;
        progress.revision = revision;
        self.publish(&state);
        Ok(())
    }
}

impl UiHistoryRun {
    pub fn metadata(&self) -> Result<UiRunMetadata> {
        let state = self.session.lock()?;
        state.check()?;
        if state.generation != self.generation {
            return Err(Error::message("obsolete UI history run"));
        }
        state
            .runs
            .get(&self.run_id)
            .map(|run| run.metadata.clone())
            .ok_or_else(|| Error::message("UI history run is inactive"))
    }

    pub fn start_attempt(
        &self,
        segment_id: Option<chelix_common::ProviderSegmentId>,
    ) -> Result<()> {
        let mut state = self.session.lock()?;
        state.check()?;
        if state.generation != self.generation {
            return Err(Error::message("obsolete UI history run"));
        }
        let run = state
            .runs
            .get_mut(&self.run_id)
            .ok_or_else(|| Error::message("UI history run is inactive"))?;
        run.segment_id = segment_id;
        run.last_error = None;
        Ok(())
    }

    pub fn recorded_error(&self, raw: &str) -> Result<Option<UiMessageId>> {
        let state = self.session.lock()?;
        state.check()?;
        if state.generation != self.generation {
            return Err(Error::message("obsolete UI history run"));
        }
        let run = state
            .runs
            .get(&self.run_id)
            .ok_or_else(|| Error::message("UI history run is inactive"))?;
        Ok(run
            .last_error
            .as_ref()
            .filter(|(_, previous)| previous == raw)
            .map(|(id, _)| id.clone()))
    }

    pub async fn flush(&self) -> Result<()> {
        self.session
            .flush()
            .await
            .inspect_err(|error| self.session.fail(error))
    }

    pub fn copy(&self, mut message: PersistedMessage) -> Result<UiMessageId> {
        if message.run_id().is_some_and(|run_id| run_id != self.run_id) {
            return Err(Error::message(
                "copied provider item has a different run ID",
            ));
        }
        match &mut message {
            PersistedMessage::ProviderUpdate { run_id, .. }
            | PersistedMessage::ProviderSegmentClose { run_id, .. }
            | PersistedMessage::Assistant { run_id, .. }
            | PersistedMessage::User { run_id, .. } => *run_id = Some(self.run_id.clone()),
            PersistedMessage::ToolLifecycle { lifecycle } => {
                lifecycle.run_id = Some(self.run_id.clone())
            },
            _ => {},
        }
        self.session
            .ingest(message.into(), UiIngress::Live, Some(&self.generation))
    }

    pub fn merge_metadata(
        &self,
        id: &UiMessageId,
        metadata: BTreeMap<String, serde_json::Value>,
    ) -> Result<()> {
        self.mutate_presentation(id, |presentation| presentation.metadata.extend(metadata))
    }

    pub fn present(&self, id: &UiMessageId, presentation: UiPresentation) -> Result<()> {
        self.mutate_presentation(id, |current| {
            current.document = presentation.document;
            current.metadata.extend(presentation.metadata);
        })
    }

    fn mutate_presentation(
        &self,
        id: &UiMessageId,
        update: impl FnOnce(&mut UiPresentation),
    ) -> Result<()> {
        let mut state = self.session.lock()?;
        state.check()?;
        if state.generation != self.generation || !state.runs.contains_key(&self.run_id) {
            return Err(Error::message("obsolete UI history run"));
        }
        if state
            .entries
            .get(id)
            .and_then(|entry| entry.snapshot.content.run_id())
            != Some(self.run_id.as_str())
        {
            return Err(Error::message(
                "UI presentation target does not belong to this run",
            ));
        }
        let revision = state.advance()?;
        let entry = state
            .entries
            .get_mut(id)
            .ok_or_else(|| Error::message("UI presentation target is missing"))?;
        update(&mut entry.snapshot.presentation);
        entry.snapshot.revision = revision;
        self.session.publish(&state);
        Ok(())
    }

    pub fn error(&self, error: UiProviderError) -> Result<UiMessageId> {
        if error.run_id != self.run_id {
            return Err(Error::message("provider error has a different run ID"));
        }
        self.session.insert_error(error, Some(&self.generation))
    }

    pub fn health(&self) -> watch::Receiver<UiHistoryRevision> {
        self.session.subscribe()
    }

    pub async fn finish(&self) -> Result<()> {
        self.flush().await?;
        let result = self.finish_registered();
        if result.is_err() {
            self.flush().await?;
        }
        result
    }

    fn finish_registered(&self) -> Result<()> {
        let mut state = self.session.lock()?;
        if state.generation != self.generation || !state.runs.contains_key(&self.run_id) {
            return Err(Error::message("obsolete UI history run"));
        }
        let failure = if state.entries.iter().any(|(id, entry)| {
            state.pending_appends.contains_key(id)
                && entry.snapshot.content.run_id() == Some(self.run_id.as_str())
        }) {
            Some("UI run still has pending canonical receipts")
        } else if state.entries.values().any(|entry| {
            entry.snapshot.content.run_id() == Some(self.run_id.as_str())
                && matches!(entry.snapshot.content, UiContent::Record(_))
                && (!entry.snapshot.canonical_committed || projection::active(entry))
        }) {
            Some("UI run contains unfinished canonical content")
        } else {
            None
        };
        if let Some(failure) = failure {
            let error = Error::message(failure);
            state.failure = Some(error.to_string());
            state.advance()?;
            self.session.publish(&state);
            tracing::error!(run_id = self.run_id, %error, "UI run finalization refused");
            return Err(error);
        }
        state.runs.remove(&self.run_id);
        let committed = state.committed_revision;
        state.entries.retain(|_, entry| {
            entry.snapshot.revision > committed
                || entry.snapshot.content.run_id() != Some(self.run_id.as_str())
        });
        state.tool_owners.retain(|tool, _| {
            !tool
                .0
                .starts_with(&format!("tool:{}:{}:", self.run_id.len(), self.run_id))
        });
        state.check()
    }
}
