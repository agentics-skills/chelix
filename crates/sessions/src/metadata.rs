use std::{cmp::Ordering, collections::HashSet};

use {
    chelix_common::{ReasoningEffort, ResolvedModelReasoning},
    serde::{Deserialize, Serialize},
};

pub use crate::backing::{ExternalAgentKind, ExternalSessionIdentity, SessionBacking};
use crate::{Error, Result};

/// System-prompt persona selected for a session.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, sqlx::Type)]
#[serde(rename_all = "lowercase")]
#[sqlx(type_name = "TEXT", rename_all = "lowercase")]
pub enum PromptProfile {
    #[default]
    Chat,
    Subagent,
}

/// A single valid session entry in the metadata index.
#[derive(Debug, Clone)]
pub struct SessionEntry {
    pub id: String,
    pub key: String,
    pub label: Option<String>,
    pub backing: SessionBacking,
    pub created_at: u64,
    pub updated_at: u64,
    pub message_count: u32,
    pub last_seen_message_count: u32,
    pub project_id: Option<String>,
    pub archived: bool,
    pub worktree_branch: Option<String>,
    pub channel_binding: Option<String>,
    pub parent_session_key: Option<String>,
    pub sandbox_owner_key: Option<String>,
    pub fork_point: Option<u32>,
    pub mcp_disabled: Option<bool>,
    pub preview: Option<String>,
    pub agent_id: Option<String>,
    pub prompt_profile: PromptProfile,
    pub version: u64,
}

impl SessionEntry {
    #[must_use]
    pub fn model_reasoning(&self) -> Option<&ResolvedModelReasoning> {
        self.backing.model_reasoning()
    }

    #[must_use]
    pub fn model(&self) -> Option<&str> {
        self.backing.model_id()
    }

    #[must_use]
    pub fn reasoning_effort(&self) -> Option<&ReasoningEffort> {
        self.backing.reasoning_effort()
    }

    #[must_use]
    pub fn external_agent_kind(&self) -> Option<ExternalAgentKind> {
        self.backing.external_agent_kind()
    }

    #[must_use]
    pub fn external_session_id(&self) -> Option<&str> {
        self.backing.external_session_id()
    }
}

/// Result of atomically creating an LLM session when it is absent.
#[derive(Debug, Clone)]
pub enum EnsureLlmSessionOutcome {
    Created(SessionEntry),
    ExistingLlm(SessionEntry),
    ExistingExternal(SessionEntry),
}

impl EnsureLlmSessionOutcome {
    #[must_use]
    pub const fn created(&self) -> bool {
        matches!(self, Self::Created(_))
    }

    #[must_use]
    pub const fn entry(&self) -> &SessionEntry {
        match self {
            Self::Created(entry) | Self::ExistingLlm(entry) | Self::ExistingExternal(entry) => {
                entry
            },
        }
    }

    #[must_use]
    pub fn into_entry(self) -> SessionEntry {
        match self {
            Self::Created(entry) | Self::ExistingLlm(entry) | Self::ExistingExternal(entry) => {
                entry
            },
        }
    }
}

/// Result of atomically promoting an external-only session to LLM + external.
#[derive(Debug, Clone)]
pub enum PromoteExternalToLlmOutcome {
    Promoted(SessionEntry),
    ExistingLlm(SessionEntry),
}

impl PromoteExternalToLlmOutcome {
    #[must_use]
    pub fn into_entry(self) -> SessionEntry {
        match self {
            Self::Promoted(entry) | Self::ExistingLlm(entry) => entry,
        }
    }
}

/// Complete metadata changes applied by one atomic session patch.
#[derive(Debug, Clone, Default)]
pub struct SessionMetadataPatch {
    pub label: Option<String>,
    pub model_reasoning: Option<ResolvedModelReasoning>,
    pub archived: Option<bool>,
    pub project_id: Option<Option<String>>,
    pub worktree_branch: Option<Option<String>>,
    pub mcp_disabled: Option<Option<bool>>,
    pub parent_session_key: Option<Option<String>>,
}

impl SessionMetadataPatch {
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.label.is_none()
            && self.model_reasoning.is_none()
            && self.archived.is_none()
            && self.project_id.is_none()
            && self.worktree_branch.is_none()
            && self.mcp_disabled.is_none()
            && self.parent_session_key.is_none()
    }
}

fn now_ms() -> i64 {
    (time::OffsetDateTime::now_utc().unix_timestamp_nanos() / 1_000_000) as i64
}

fn compare_sidebar_order(lhs: &SessionEntry, rhs: &SessionEntry) -> Ordering {
    let lhs_main = lhs.key == "main";
    let rhs_main = rhs.key == "main";

    rhs_main
        .cmp(&lhs_main)
        .then_with(|| rhs.updated_at.cmp(&lhs.updated_at))
        .then_with(|| rhs.created_at.cmp(&lhs.created_at))
        .then_with(|| lhs.key.cmp(&rhs.key))
}

/// SQLite-backed session metadata store.
pub struct SqliteSessionMetadata {
    pool: sqlx::SqlitePool,
    event_bus: Option<crate::session_events::SessionEventBus>,
}

#[derive(Debug, Deserialize)]
struct PersistedChannelBinding {
    channel_type: String,
    account_id: String,
    chat_id: String,
    #[serde(default)]
    thread_id: Option<String>,
}

impl PersistedChannelBinding {
    fn default_session_key(&self) -> String {
        match self.thread_id.as_deref() {
            Some(thread_id) => format!(
                "{}:{}:{}:{}",
                self.channel_type, self.account_id, self.chat_id, thread_id
            ),
            None => format!("{}:{}:{}", self.channel_type, self.account_id, self.chat_id),
        }
    }
}

#[derive(sqlx::FromRow)]
struct SessionRow {
    key: String,
    id: String,
    label: Option<String>,
    model: Option<String>,
    reasoning_effort: Option<String>,
    created_at: i64,
    updated_at: i64,
    message_count: i32,
    last_seen_message_count: i32,
    project_id: Option<String>,
    archived: i32,
    worktree_branch: Option<String>,
    channel_binding: Option<String>,
    parent_session_key: Option<String>,
    sandbox_owner_key: Option<String>,
    fork_point: Option<i32>,
    mcp_disabled: Option<i32>,
    preview: Option<String>,
    agent_id: Option<String>,
    prompt_profile: PromptProfile,
    external_agent_kind: Option<String>,
    external_session_id: Option<String>,
    version: i64,
}

impl TryFrom<SessionRow> for SessionEntry {
    type Error = Error;

    fn try_from(row: SessionRow) -> Result<Self> {
        Ok(Self {
            key: row.key,
            id: row.id,
            label: row.label,
            backing: SessionBacking::try_from_persisted(
                row.model,
                row.reasoning_effort,
                row.external_agent_kind,
                row.external_session_id,
            )?,
            created_at: row.created_at as u64,
            updated_at: row.updated_at as u64,
            message_count: row.message_count as u32,
            last_seen_message_count: row.last_seen_message_count as u32,
            project_id: row.project_id,
            archived: row.archived != 0,
            worktree_branch: row.worktree_branch,
            channel_binding: row.channel_binding,
            parent_session_key: row.parent_session_key,
            sandbox_owner_key: row.sandbox_owner_key,
            fork_point: row.fork_point.map(|value| value as u32),
            mcp_disabled: row.mcp_disabled.map(|value| value != 0),
            preview: row.preview,
            agent_id: row.agent_id,
            prompt_profile: row.prompt_profile,
            version: row.version as u64,
        })
    }
}

fn decode_rows(rows: Vec<SessionRow>) -> Result<Vec<SessionEntry>> {
    rows.into_iter().map(TryInto::try_into).collect()
}

fn require_existing_row(key: &str, rows_affected: u64) -> Result<()> {
    if rows_affected == 1 {
        return Ok(());
    }
    Err(Error::message(format!("session '{key}' not found")))
}

impl SqliteSessionMetadata {
    #[must_use]
    pub fn new(pool: sqlx::SqlitePool) -> Self {
        Self {
            pool,
            event_bus: None,
        }
    }

    #[must_use]
    pub fn with_event_bus(
        pool: sqlx::SqlitePool,
        event_bus: crate::session_events::SessionEventBus,
    ) -> Self {
        Self {
            pool,
            event_bus: Some(event_bus),
        }
    }

    #[must_use]
    pub const fn event_bus(&self) -> Option<&crate::session_events::SessionEventBus> {
        self.event_bus.as_ref()
    }

    fn emit(&self, event: crate::session_events::SessionEvent) {
        if let Some(event_bus) = &self.event_bus {
            event_bus.publish(event);
        }
    }

    #[doc(hidden)]
    pub async fn init(pool: &sqlx::SqlitePool) -> Result<()> {
        sqlx::query(
            r#"CREATE TABLE IF NOT EXISTS sessions (
                key                     TEXT PRIMARY KEY,
                id                      TEXT NOT NULL,
                label                   TEXT,
                model                   TEXT,
                reasoning_effort        TEXT,
                created_at              INTEGER NOT NULL,
                updated_at              INTEGER NOT NULL,
                message_count           INTEGER NOT NULL DEFAULT 0,
                last_seen_message_count INTEGER NOT NULL DEFAULT 0,
                project_id              TEXT REFERENCES projects(id) ON DELETE SET NULL,
                archived                INTEGER NOT NULL DEFAULT 0,
                worktree_branch         TEXT,
                channel_binding         TEXT,
                parent_session_key      TEXT,
                sandbox_owner_key       TEXT,
                fork_point              INTEGER,
                mcp_disabled            INTEGER,
                preview                 TEXT,
                agent_id                TEXT,
                prompt_profile          TEXT NOT NULL DEFAULT 'chat',
                external_agent_kind     TEXT,
                external_session_id     TEXT,
                version                 INTEGER NOT NULL DEFAULT 0,
                CHECK (
                    (model IS NULL AND reasoning_effort IS NULL)
                    OR (
                        model IS NOT NULL
                        AND model <> ''
                        AND reasoning_effort IS NOT NULL
                        AND reasoning_effort <> ''
                    )
                ),
                CHECK (model IS NOT NULL OR external_agent_kind IS NOT NULL),
                CHECK (
                    external_agent_kind IS NULL
                    OR external_agent_kind IN ('claude-code', 'opencode', 'codex', 'pi-agent', 'acp')
                ),
                CHECK (external_session_id IS NULL OR external_agent_kind IS NOT NULL)
            )"#,
        )
        .execute(pool)
        .await?;

        sqlx::query("CREATE INDEX IF NOT EXISTS idx_sessions_created_at ON sessions(created_at)")
            .execute(pool)
            .await?;
        sqlx::query(
            "CREATE INDEX IF NOT EXISTS idx_sessions_parent ON sessions(parent_session_key)",
        )
        .execute(pool)
        .await?;
        sqlx::query(
            r#"CREATE TABLE IF NOT EXISTS channel_sessions (
                channel_type TEXT NOT NULL,
                account_id   TEXT NOT NULL,
                chat_id      TEXT NOT NULL,
                thread_id    TEXT NOT NULL DEFAULT '',
                session_key  TEXT NOT NULL,
                updated_at   INTEGER NOT NULL,
                PRIMARY KEY (channel_type, account_id, chat_id, thread_id)
            )"#,
        )
        .execute(pool)
        .await?;

        Ok(())
    }

    async fn fetch_entry_in_transaction(
        transaction: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
        key: &str,
    ) -> Result<Option<SessionEntry>> {
        sqlx::query_as::<_, SessionRow>("SELECT * FROM sessions WHERE key = ?")
            .bind(key)
            .fetch_optional(&mut **transaction)
            .await?
            .map(TryInto::try_into)
            .transpose()
    }

    pub async fn get(&self, key: &str) -> Result<Option<SessionEntry>> {
        sqlx::query_as::<_, SessionRow>("SELECT * FROM sessions WHERE key = ?")
            .bind(key)
            .fetch_optional(&self.pool)
            .await?
            .map(TryInto::try_into)
            .transpose()
    }

    pub async fn try_get(&self, key: &str) -> Result<Option<SessionEntry>> {
        self.get(key).await
    }

    pub async fn create_llm_session(
        &self,
        key: &str,
        label: Option<&str>,
        model_reasoning: &ResolvedModelReasoning,
        agent_id: Option<&str>,
    ) -> Result<SessionEntry> {
        let now = now_ms();
        let id = uuid::Uuid::new_v4().to_string();
        let mut transaction = self.pool.begin().await?;
        sqlx::query(
            r#"INSERT INTO sessions (
                   key, id, label, model, reasoning_effort, created_at, updated_at, agent_id, version
               ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, 0)"#,
        )
        .bind(key)
        .bind(id)
        .bind(label)
        .bind(model_reasoning.model_id())
        .bind(model_reasoning.reasoning_effort().as_str())
        .bind(now)
        .bind(now)
        .bind(agent_id)
        .execute(&mut *transaction)
        .await?;
        let entry = Self::fetch_entry_in_transaction(&mut transaction, key)
            .await?
            .ok_or_else(|| Error::message(format!("session '{key}' disappeared during create")))?;
        transaction.commit().await?;
        self.emit(crate::session_events::SessionEvent::Created {
            session_key: key.to_string(),
        });
        Ok(entry)
    }

    pub async fn ensure_llm_session(
        &self,
        key: &str,
        label: Option<&str>,
        model_reasoning: &ResolvedModelReasoning,
        agent_id: Option<&str>,
    ) -> Result<EnsureLlmSessionOutcome> {
        let now = now_ms();
        let id = uuid::Uuid::new_v4().to_string();
        let mut transaction = self.pool.begin().await?;
        let result = sqlx::query(
            r#"INSERT INTO sessions (
                   key, id, label, model, reasoning_effort, created_at, updated_at, agent_id, version
               ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, 0)
               ON CONFLICT(key) DO NOTHING"#,
        )
        .bind(key)
        .bind(id)
        .bind(label)
        .bind(model_reasoning.model_id())
        .bind(model_reasoning.reasoning_effort().as_str())
        .bind(now)
        .bind(now)
        .bind(agent_id)
        .execute(&mut *transaction)
        .await?;
        let created = result.rows_affected() == 1;
        let entry = Self::fetch_entry_in_transaction(&mut transaction, key)
            .await?
            .ok_or_else(|| Error::message(format!("session '{key}' disappeared during ensure")))?;
        let outcome = if created {
            EnsureLlmSessionOutcome::Created(entry)
        } else if entry.model_reasoning().is_some() {
            EnsureLlmSessionOutcome::ExistingLlm(entry)
        } else {
            EnsureLlmSessionOutcome::ExistingExternal(entry)
        };
        transaction.commit().await?;
        if created {
            self.emit(crate::session_events::SessionEvent::Created {
                session_key: key.to_string(),
            });
        }
        Ok(outcome)
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn create_subagent_session(
        &self,
        key: &str,
        label: &str,
        parent_session_key: &str,
        sandbox_owner_key: &str,
        agent_id: &str,
        model_reasoning: &ResolvedModelReasoning,
    ) -> Result<SessionEntry> {
        let now = now_ms();
        let id = uuid::Uuid::new_v4().to_string();
        let mut transaction = self.pool.begin().await?;
        sqlx::query(
            r#"INSERT INTO sessions (
                   key, id, label, model, reasoning_effort, created_at, updated_at,
                   parent_session_key, sandbox_owner_key, agent_id, prompt_profile, version
               ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, 0)"#,
        )
        .bind(key)
        .bind(id)
        .bind(label)
        .bind(model_reasoning.model_id())
        .bind(model_reasoning.reasoning_effort().as_str())
        .bind(now)
        .bind(now)
        .bind(parent_session_key)
        .bind(sandbox_owner_key)
        .bind(agent_id)
        .bind(PromptProfile::Subagent)
        .execute(&mut *transaction)
        .await?;
        let entry = Self::fetch_entry_in_transaction(&mut transaction, key)
            .await?
            .ok_or_else(|| Error::message(format!("session '{key}' disappeared during create")))?;
        transaction.commit().await?;
        self.emit(crate::session_events::SessionEvent::Created {
            session_key: key.to_string(),
        });
        Ok(entry)
    }

    pub async fn create_or_assign_agent(
        &self,
        key: &str,
        agent_id: &str,
        model_reasoning: &ResolvedModelReasoning,
    ) -> Result<SessionEntry> {
        let now = now_ms();
        let id = uuid::Uuid::new_v4().to_string();
        let mut transaction = self.pool.begin().await?;
        let existed = Self::fetch_entry_in_transaction(&mut transaction, key)
            .await?
            .is_some();
        sqlx::query(
            r#"INSERT INTO sessions (
                   key, id, model, reasoning_effort, created_at, updated_at, agent_id, version
               ) VALUES (?, ?, ?, ?, ?, ?, ?, 0)
               ON CONFLICT(key) DO UPDATE SET
                   agent_id = excluded.agent_id,
                   model = excluded.model,
                   reasoning_effort = excluded.reasoning_effort,
                   external_agent_kind = NULL,
                   external_session_id = NULL,
                   updated_at = excluded.updated_at,
                   version = sessions.version + 1"#,
        )
        .bind(key)
        .bind(id)
        .bind(model_reasoning.model_id())
        .bind(model_reasoning.reasoning_effort().as_str())
        .bind(now)
        .bind(now)
        .bind(agent_id)
        .execute(&mut *transaction)
        .await?;
        let entry = Self::fetch_entry_in_transaction(&mut transaction, key)
            .await?
            .ok_or_else(|| Error::message(format!("session '{key}' disappeared during assign")))?;
        transaction.commit().await?;
        self.emit(if existed {
            crate::session_events::SessionEvent::Patched {
                session_key: key.to_string(),
            }
        } else {
            crate::session_events::SessionEvent::Created {
                session_key: key.to_string(),
            }
        });
        Ok(entry)
    }

    pub async fn assign_agent(
        &self,
        key: &str,
        agent_id: &str,
        model_reasoning: &ResolvedModelReasoning,
    ) -> Result<SessionEntry> {
        let now = now_ms();
        let mut transaction = self.pool.begin().await?;
        let result = sqlx::query(
            r#"UPDATE sessions
               SET agent_id = ?, model = ?, reasoning_effort = ?, updated_at = ?, version = version + 1
               WHERE key = ?"#,
        )
        .bind(agent_id)
        .bind(model_reasoning.model_id())
        .bind(model_reasoning.reasoning_effort().as_str())
        .bind(now)
        .bind(key)
        .execute(&mut *transaction)
        .await?;
        require_existing_row(key, result.rows_affected())?;
        let entry = Self::fetch_entry_in_transaction(&mut transaction, key)
            .await?
            .ok_or_else(|| Error::message(format!("session '{key}' disappeared during assign")))?;
        transaction.commit().await?;
        self.emit(crate::session_events::SessionEvent::Patched {
            session_key: key.to_string(),
        });
        Ok(entry)
    }

    pub async fn assign_llm_agent(
        &self,
        key: &str,
        agent_id: &str,
        model_reasoning: &ResolvedModelReasoning,
    ) -> Result<SessionEntry> {
        let now = now_ms();
        let mut transaction = self.pool.begin().await?;
        let result = sqlx::query(
            r#"UPDATE sessions
               SET agent_id = ?, model = ?, reasoning_effort = ?, external_agent_kind = NULL,
                   external_session_id = NULL, updated_at = ?, version = version + 1
               WHERE key = ?"#,
        )
        .bind(agent_id)
        .bind(model_reasoning.model_id())
        .bind(model_reasoning.reasoning_effort().as_str())
        .bind(now)
        .bind(key)
        .execute(&mut *transaction)
        .await?;
        require_existing_row(key, result.rows_affected())?;
        let entry = Self::fetch_entry_in_transaction(&mut transaction, key)
            .await?
            .ok_or_else(|| {
                Error::message(format!(
                    "session '{key}' disappeared during LLM agent assignment"
                ))
            })?;
        transaction.commit().await?;
        self.emit(crate::session_events::SessionEvent::Patched {
            session_key: key.to_string(),
        });
        Ok(entry)
    }

    async fn validate_parent_patch_in_transaction(
        transaction: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
        key: &str,
        entry: &SessionEntry,
        parent_session_key: Option<&str>,
    ) -> Result<()> {
        if entry.prompt_profile == PromptProfile::Subagent {
            return Err(Error::message(format!(
                "session '{key}' is a sub-agent session and cannot be reparented"
            )));
        }
        let Some(parent_session_key) = parent_session_key else {
            return Ok(());
        };
        if parent_session_key == key {
            return Err(Error::message("a session cannot be its own parent"));
        }
        if Self::fetch_entry_in_transaction(transaction, parent_session_key)
            .await?
            .is_none()
        {
            return Err(Error::message(format!(
                "parent session '{parent_session_key}' not found"
            )));
        }
        let creates_cycle = sqlx::query_scalar::<_, i64>(
            r#"WITH RECURSIVE ancestors(key) AS (
                   SELECT ?
                   UNION
                   SELECT sessions.parent_session_key
                   FROM sessions
                   JOIN ancestors ON sessions.key = ancestors.key
                   WHERE sessions.parent_session_key IS NOT NULL
               )
               SELECT EXISTS(SELECT 1 FROM ancestors WHERE key = ?)"#,
        )
        .bind(parent_session_key)
        .bind(key)
        .fetch_one(&mut **transaction)
        .await?
            != 0;
        if creates_cycle {
            return Err(Error::message("parent assignment would create a cycle"));
        }
        Ok(())
    }

    async fn validate_archive_patch_in_transaction(
        transaction: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
        entry: &SessionEntry,
    ) -> Result<()> {
        if entry.key == "main" {
            return Err(Error::message("session 'main' cannot be archived"));
        }
        let Some(binding_json) = entry.channel_binding.as_deref() else {
            return Ok(());
        };
        let binding: PersistedChannelBinding = serde_json::from_str(binding_json)?;
        let active_key = sqlx::query_scalar::<_, String>(
            r#"SELECT session_key FROM channel_sessions
               WHERE channel_type = ? AND account_id = ? AND chat_id = ? AND thread_id = ?"#,
        )
        .bind(&binding.channel_type)
        .bind(&binding.account_id)
        .bind(&binding.chat_id)
        .bind(binding.thread_id.as_deref().unwrap_or(""))
        .fetch_optional(&mut **transaction)
        .await?
        .unwrap_or_else(|| binding.default_session_key());
        if active_key == entry.key {
            return Err(Error::message(format!(
                "session '{}' cannot be archived",
                entry.key
            )));
        }
        Ok(())
    }

    pub async fn patch_session(
        &self,
        key: &str,
        patch: SessionMetadataPatch,
    ) -> Result<SessionEntry> {
        let mut transaction = self.pool.begin().await?;
        let current = Self::fetch_entry_in_transaction(&mut transaction, key)
            .await?
            .ok_or_else(|| Error::message(format!("session '{key}' not found")))?;
        if patch.is_empty() {
            transaction.commit().await?;
            return Ok(current);
        }
        if let Some(parent_session_key) = patch.parent_session_key.as_ref() {
            Self::validate_parent_patch_in_transaction(
                &mut transaction,
                key,
                &current,
                parent_session_key.as_deref(),
            )
            .await?;
        }
        if patch.archived == Some(true) {
            Self::validate_archive_patch_in_transaction(&mut transaction, &current).await?;
        }

        let now = now_ms();
        let label_changed = patch.label.is_some();
        let pair_changed = patch.model_reasoning.is_some();
        let archived_changed = patch.archived.is_some();
        let project_changed = patch.project_id.is_some();
        let worktree_changed = patch.worktree_branch.is_some();
        let mcp_changed = patch.mcp_disabled.is_some();
        let parent_changed = patch.parent_session_key.is_some();
        let model = patch
            .model_reasoning
            .as_ref()
            .map(ResolvedModelReasoning::model_id);
        let reasoning_effort = patch
            .model_reasoning
            .as_ref()
            .map(ResolvedModelReasoning::reasoning_effort)
            .map(ReasoningEffort::as_str);
        let project_id = patch.project_id.as_ref().and_then(|value| value.as_deref());
        let worktree_branch = patch
            .worktree_branch
            .as_ref()
            .and_then(|value| value.as_deref());
        let mcp_disabled = patch
            .mcp_disabled
            .as_ref()
            .and_then(|value| value.map(i32::from));
        let parent_session_key = patch
            .parent_session_key
            .as_ref()
            .and_then(|value| value.as_deref());
        let result = sqlx::query(
            r#"UPDATE sessions SET
                   label = CASE WHEN ? THEN ? ELSE label END,
                   model = CASE WHEN ? THEN ? ELSE model END,
                   reasoning_effort = CASE WHEN ? THEN ? ELSE reasoning_effort END,
                   archived = CASE WHEN ? THEN ? ELSE archived END,
                   project_id = CASE WHEN ? THEN ? ELSE project_id END,
                   worktree_branch = CASE WHEN ? THEN ? ELSE worktree_branch END,
                   mcp_disabled = CASE WHEN ? THEN ? ELSE mcp_disabled END,
                   parent_session_key = CASE WHEN ? THEN ? ELSE parent_session_key END,
                   fork_point = CASE WHEN ? THEN NULL ELSE fork_point END,
                   updated_at = ?,
                   version = version + 1
               WHERE key = ?"#,
        )
        .bind(label_changed)
        .bind(patch.label.as_deref())
        .bind(pair_changed)
        .bind(model)
        .bind(pair_changed)
        .bind(reasoning_effort)
        .bind(archived_changed)
        .bind(patch.archived.map(i32::from))
        .bind(project_changed)
        .bind(project_id)
        .bind(worktree_changed)
        .bind(worktree_branch)
        .bind(mcp_changed)
        .bind(mcp_disabled)
        .bind(parent_changed)
        .bind(parent_session_key)
        .bind(parent_changed)
        .bind(now)
        .bind(key)
        .execute(&mut *transaction)
        .await?;
        require_existing_row(key, result.rows_affected())?;
        let entry = Self::fetch_entry_in_transaction(&mut transaction, key)
            .await?
            .ok_or_else(|| Error::message(format!("session '{key}' disappeared during patch")))?;
        transaction.commit().await?;
        self.emit(crate::session_events::SessionEvent::Patched {
            session_key: key.to_string(),
        });
        Ok(entry)
    }

    pub async fn update_label(&self, key: &str, label: Option<&str>) -> Result<SessionEntry> {
        let now = now_ms();
        let mut transaction = self.pool.begin().await?;
        let result = sqlx::query(
            "UPDATE sessions SET label = ?, updated_at = ?, version = version + 1 WHERE key = ?",
        )
        .bind(label)
        .bind(now)
        .bind(key)
        .execute(&mut *transaction)
        .await?;
        require_existing_row(key, result.rows_affected())?;
        let entry = Self::fetch_entry_in_transaction(&mut transaction, key)
            .await?
            .ok_or_else(|| {
                Error::message(format!("session '{key}' disappeared during label update"))
            })?;
        transaction.commit().await?;
        self.emit(crate::session_events::SessionEvent::Patched {
            session_key: key.to_string(),
        });
        Ok(entry)
    }

    pub async fn set_model_reasoning(
        &self,
        key: &str,
        model_reasoning: &ResolvedModelReasoning,
    ) -> Result<SessionEntry> {
        let now = now_ms();
        let mut transaction = self.pool.begin().await?;
        let result = sqlx::query(
            r#"UPDATE sessions
               SET model = ?, reasoning_effort = ?, updated_at = ?, version = version + 1
               WHERE key = ?"#,
        )
        .bind(model_reasoning.model_id())
        .bind(model_reasoning.reasoning_effort().as_str())
        .bind(now)
        .bind(key)
        .execute(&mut *transaction)
        .await?;
        require_existing_row(key, result.rows_affected())?;
        let entry = Self::fetch_entry_in_transaction(&mut transaction, key)
            .await?
            .ok_or_else(|| {
                Error::message(format!("session '{key}' disappeared during pair update"))
            })?;
        transaction.commit().await?;
        self.emit(crate::session_events::SessionEvent::Patched {
            session_key: key.to_string(),
        });
        Ok(entry)
    }

    pub async fn promote_external_to_llm(
        &self,
        key: &str,
        model_reasoning: &ResolvedModelReasoning,
        agent_id: &str,
    ) -> Result<PromoteExternalToLlmOutcome> {
        let now = now_ms();
        let mut transaction = self.pool.begin().await?;
        let entry = Self::fetch_entry_in_transaction(&mut transaction, key)
            .await?
            .ok_or_else(|| Error::message(format!("session '{key}' not found")))?;
        if entry.model_reasoning().is_some() {
            transaction.commit().await?;
            return Ok(PromoteExternalToLlmOutcome::ExistingLlm(entry));
        }

        let result = sqlx::query(
            r#"UPDATE sessions
               SET model = ?, reasoning_effort = ?, agent_id = ?, updated_at = ?,
                   version = version + 1
               WHERE key = ? AND model IS NULL AND reasoning_effort IS NULL"#,
        )
        .bind(model_reasoning.model_id())
        .bind(model_reasoning.reasoning_effort().as_str())
        .bind(agent_id)
        .bind(now)
        .bind(key)
        .execute(&mut *transaction)
        .await?;
        require_existing_row(key, result.rows_affected())?;
        let entry = Self::fetch_entry_in_transaction(&mut transaction, key)
            .await?
            .ok_or_else(|| {
                Error::message(format!("session '{key}' disappeared during promotion"))
            })?;
        transaction.commit().await?;
        self.emit(crate::session_events::SessionEvent::Patched {
            session_key: key.to_string(),
        });
        Ok(PromoteExternalToLlmOutcome::Promoted(entry))
    }

    pub async fn replace_external_with_llm(
        &self,
        key: &str,
        expected_version: u64,
        agent_id: &str,
        model_reasoning: &ResolvedModelReasoning,
    ) -> Result<SessionEntry> {
        let now = now_ms();
        let mut transaction = self.pool.begin().await?;
        let entry = Self::fetch_entry_in_transaction(&mut transaction, key)
            .await?
            .ok_or_else(|| Error::message(format!("session '{key}' not found")))?;
        if entry.version != expected_version
            || !matches!(entry.backing, SessionBacking::External { .. })
        {
            return Err(Error::message(format!(
                "session '{key}' external-only transition conflict"
            )));
        }
        let result = sqlx::query(
            r#"UPDATE sessions
               SET agent_id = ?, model = ?, reasoning_effort = ?, external_agent_kind = NULL,
                   external_session_id = NULL, updated_at = ?, version = version + 1
               WHERE key = ? AND version = ? AND model IS NULL AND reasoning_effort IS NULL"#,
        )
        .bind(agent_id)
        .bind(model_reasoning.model_id())
        .bind(model_reasoning.reasoning_effort().as_str())
        .bind(now)
        .bind(key)
        .bind(expected_version as i64)
        .execute(&mut *transaction)
        .await?;
        require_existing_row(key, result.rows_affected())?;
        let entry = Self::fetch_entry_in_transaction(&mut transaction, key)
            .await?
            .ok_or_else(|| {
                Error::message(format!("session '{key}' disappeared during transition"))
            })?;
        transaction.commit().await?;
        self.emit(crate::session_events::SessionEvent::Patched {
            session_key: key.to_string(),
        });
        Ok(entry)
    }

    pub async fn bind_external(
        &self,
        key: &str,
        label: Option<&str>,
        identity: &ExternalSessionIdentity,
    ) -> Result<SessionEntry> {
        let now = now_ms();
        let mut transaction = self.pool.begin().await?;
        let existed = Self::fetch_entry_in_transaction(&mut transaction, key)
            .await?
            .is_some();
        if existed {
            sqlx::query(
                r#"UPDATE sessions
                   SET external_agent_kind = ?, external_session_id = ?, updated_at = ?, version = version + 1
                   WHERE key = ?"#,
            )
            .bind(identity.kind().as_str())
            .bind(identity.external_session_id())
            .bind(now)
            .bind(key)
            .execute(&mut *transaction)
            .await?;
        } else {
            let id = uuid::Uuid::new_v4().to_string();
            sqlx::query(
                r#"INSERT INTO sessions (
                       key, id, label, created_at, updated_at, external_agent_kind,
                       external_session_id, version
                   ) VALUES (?, ?, ?, ?, ?, ?, ?, 0)"#,
            )
            .bind(key)
            .bind(id)
            .bind(label)
            .bind(now)
            .bind(now)
            .bind(identity.kind().as_str())
            .bind(identity.external_session_id())
            .execute(&mut *transaction)
            .await?;
        }
        let entry = Self::fetch_entry_in_transaction(&mut transaction, key)
            .await?
            .ok_or_else(|| Error::message(format!("session '{key}' disappeared during bind")))?;
        transaction.commit().await?;
        self.emit(if existed {
            crate::session_events::SessionEvent::Patched {
                session_key: key.to_string(),
            }
        } else {
            crate::session_events::SessionEvent::Created {
                session_key: key.to_string(),
            }
        });
        Ok(entry)
    }

    pub async fn update_external_session_id(
        &self,
        key: &str,
        external_session_id: Option<&str>,
    ) -> Result<SessionEntry> {
        let now = now_ms();
        let mut transaction = self.pool.begin().await?;
        let entry = Self::fetch_entry_in_transaction(&mut transaction, key)
            .await?
            .ok_or_else(|| Error::message(format!("session '{key}' not found")))?;
        if entry.external_agent_kind().is_none() {
            return Err(Error::message(format!(
                "session '{key}' has no external binding"
            )));
        }
        sqlx::query(
            r#"UPDATE sessions
               SET external_session_id = ?, updated_at = ?, version = version + 1
               WHERE key = ?"#,
        )
        .bind(external_session_id)
        .bind(now)
        .bind(key)
        .execute(&mut *transaction)
        .await?;
        let entry = Self::fetch_entry_in_transaction(&mut transaction, key)
            .await?
            .ok_or_else(|| Error::message(format!("session '{key}' disappeared during update")))?;
        transaction.commit().await?;
        self.emit(crate::session_events::SessionEvent::Patched {
            session_key: key.to_string(),
        });
        Ok(entry)
    }

    pub async fn unbind_llm_external(
        &self,
        key: &str,
        expected_version: u64,
    ) -> Result<SessionEntry> {
        let now = now_ms();
        let mut transaction = self.pool.begin().await?;
        let entry = Self::fetch_entry_in_transaction(&mut transaction, key)
            .await?
            .ok_or_else(|| Error::message(format!("session '{key}' not found")))?;
        if entry.version != expected_version
            || !matches!(entry.backing, SessionBacking::LlmExternal { .. })
        {
            return Err(Error::message(format!(
                "session '{key}' LLM external unbind conflict"
            )));
        }
        let result = sqlx::query(
            r#"UPDATE sessions
               SET external_agent_kind = NULL, external_session_id = NULL,
                   updated_at = ?, version = version + 1
               WHERE key = ? AND version = ? AND model IS NOT NULL
                   AND reasoning_effort IS NOT NULL"#,
        )
        .bind(now)
        .bind(key)
        .bind(expected_version as i64)
        .execute(&mut *transaction)
        .await?;
        require_existing_row(key, result.rows_affected())?;
        let entry = Self::fetch_entry_in_transaction(&mut transaction, key)
            .await?
            .ok_or_else(|| Error::message(format!("session '{key}' disappeared during unbind")))?;
        transaction.commit().await?;
        self.emit(crate::session_events::SessionEvent::Patched {
            session_key: key.to_string(),
        });
        Ok(entry)
    }

    pub async fn unbind_external(&self, key: &str) -> Result<Option<SessionEntry>> {
        let now = now_ms();
        let mut transaction = self.pool.begin().await?;
        let entry = Self::fetch_entry_in_transaction(&mut transaction, key)
            .await?
            .ok_or_else(|| Error::message(format!("session '{key}' not found")))?;
        let result = match entry.backing {
            SessionBacking::LlmExternal { .. } => {
                sqlx::query(
                    r#"UPDATE sessions
                       SET external_agent_kind = NULL, external_session_id = NULL,
                           updated_at = ?, version = version + 1
                       WHERE key = ?"#,
                )
                .bind(now)
                .bind(key)
                .execute(&mut *transaction)
                .await?;
                Self::fetch_entry_in_transaction(&mut transaction, key).await?
            },
            SessionBacking::External { .. } => {
                sqlx::query("DELETE FROM sessions WHERE key = ?")
                    .bind(key)
                    .execute(&mut *transaction)
                    .await?;
                None
            },
            SessionBacking::Llm { .. } => {
                return Err(Error::message(format!(
                    "session '{key}' has no external binding"
                )));
            },
        };
        transaction.commit().await?;
        self.emit(if result.is_some() {
            crate::session_events::SessionEvent::Patched {
                session_key: key.to_string(),
            }
        } else {
            crate::session_events::SessionEvent::Deleted {
                session_key: key.to_string(),
            }
        });
        Ok(result)
    }

    pub async fn touch(&self, key: &str, message_count: u32) -> Result<()> {
        let now = now_ms();
        let result = sqlx::query(
            "UPDATE sessions SET message_count = ?, updated_at = ?, version = version + 1 WHERE key = ?",
        )
        .bind(message_count as i32)
        .bind(now)
        .bind(key)
        .execute(&self.pool)
        .await?;
        require_existing_row(key, result.rows_affected())?;
        self.emit(crate::session_events::SessionEvent::Patched {
            session_key: key.to_string(),
        });
        Ok(())
    }

    pub async fn set_timestamps_and_counts(
        &self,
        key: &str,
        created_at: u64,
        updated_at: u64,
        message_count: u32,
        last_seen_message_count: u32,
    ) -> Result<()> {
        let result = sqlx::query(
            r#"UPDATE sessions
               SET created_at = ?, updated_at = ?, message_count = ?,
                   last_seen_message_count = ?, version = version + 1
               WHERE key = ?"#,
        )
        .bind(created_at as i64)
        .bind(updated_at as i64)
        .bind(message_count as i32)
        .bind(last_seen_message_count as i32)
        .bind(key)
        .execute(&self.pool)
        .await?;
        require_existing_row(key, result.rows_affected())
    }

    pub async fn set_preview(&self, key: &str, preview: Option<&str>) -> Result<()> {
        let result =
            sqlx::query("UPDATE sessions SET preview = ?, version = version + 1 WHERE key = ?")
                .bind(preview)
                .bind(key)
                .execute(&self.pool)
                .await?;
        require_existing_row(key, result.rows_affected())?;
        self.emit(crate::session_events::SessionEvent::Patched {
            session_key: key.to_string(),
        });
        Ok(())
    }

    pub async fn mark_seen(&self, key: &str) -> Result<()> {
        let result = sqlx::query(
            "UPDATE sessions SET last_seen_message_count = message_count, version = version + 1 WHERE key = ?",
        )
        .bind(key)
        .execute(&self.pool)
        .await?;
        require_existing_row(key, result.rows_affected())?;
        self.emit(crate::session_events::SessionEvent::Patched {
            session_key: key.to_string(),
        });
        Ok(())
    }

    pub async fn set_project_id(&self, key: &str, project_id: Option<&str>) -> Result<()> {
        self.update_optional_text(key, "project_id", project_id)
            .await
    }

    pub async fn set_worktree_branch(&self, key: &str, branch: Option<&str>) -> Result<()> {
        self.update_optional_text(key, "worktree_branch", branch)
            .await
    }

    pub async fn set_channel_binding(&self, key: &str, binding: Option<&str>) -> Result<()> {
        self.update_optional_text(key, "channel_binding", binding)
            .await
    }

    pub async fn set_sandbox_owner_key(&self, key: &str, owner_key: Option<&str>) -> Result<()> {
        self.update_optional_text(key, "sandbox_owner_key", owner_key)
            .await
    }

    async fn update_optional_text(
        &self,
        key: &str,
        column: &'static str,
        value: Option<&str>,
    ) -> Result<()> {
        let now = now_ms();
        let statement = format!(
            "UPDATE sessions SET {column} = ?, updated_at = ?, version = version + 1 WHERE key = ?"
        );
        let result = sqlx::query(&statement)
            .bind(value)
            .bind(now)
            .bind(key)
            .execute(&self.pool)
            .await?;
        require_existing_row(key, result.rows_affected())?;
        self.emit(crate::session_events::SessionEvent::Patched {
            session_key: key.to_string(),
        });
        Ok(())
    }

    pub async fn set_archived(&self, key: &str, archived: bool) -> Result<()> {
        let now = now_ms();
        let result = sqlx::query(
            "UPDATE sessions SET archived = ?, updated_at = ?, version = version + 1 WHERE key = ?",
        )
        .bind(if archived {
            1_i32
        } else {
            0_i32
        })
        .bind(now)
        .bind(key)
        .execute(&self.pool)
        .await?;
        require_existing_row(key, result.rows_affected())?;
        self.emit(crate::session_events::SessionEvent::Patched {
            session_key: key.to_string(),
        });
        Ok(())
    }

    pub async fn set_mcp_disabled(&self, key: &str, disabled: Option<bool>) -> Result<()> {
        let now = now_ms();
        let result = sqlx::query(
            "UPDATE sessions SET mcp_disabled = ?, updated_at = ?, version = version + 1 WHERE key = ?",
        )
        .bind(disabled.map(|value| if value { 1_i32 } else { 0_i32 }))
        .bind(now)
        .bind(key)
        .execute(&self.pool)
        .await?;
        require_existing_row(key, result.rows_affected())?;
        self.emit(crate::session_events::SessionEvent::Patched {
            session_key: key.to_string(),
        });
        Ok(())
    }

    pub async fn set_prompt_profile(&self, key: &str, prompt_profile: PromptProfile) -> Result<()> {
        let now = now_ms();
        let result = sqlx::query(
            "UPDATE sessions SET prompt_profile = ?, updated_at = ?, version = version + 1 WHERE key = ?",
        )
        .bind(prompt_profile)
        .bind(now)
        .bind(key)
        .execute(&self.pool)
        .await?;
        require_existing_row(key, result.rows_affected())?;
        self.emit(crate::session_events::SessionEvent::Patched {
            session_key: key.to_string(),
        });
        Ok(())
    }

    pub async fn set_parent(
        &self,
        key: &str,
        parent_key: Option<&str>,
        fork_point: Option<u32>,
    ) -> Result<()> {
        let now = now_ms();
        let result = sqlx::query(
            r#"UPDATE sessions
               SET parent_session_key = ?, fork_point = ?, updated_at = ?, version = version + 1
               WHERE key = ?"#,
        )
        .bind(parent_key)
        .bind(fork_point.map(|value| value as i32))
        .bind(now)
        .bind(key)
        .execute(&self.pool)
        .await?;
        require_existing_row(key, result.rows_affected())?;
        self.emit(crate::session_events::SessionEvent::Patched {
            session_key: key.to_string(),
        });
        Ok(())
    }

    pub async fn list(&self) -> Result<Vec<SessionEntry>> {
        let mut entries = decode_rows(
            sqlx::query_as::<_, SessionRow>("SELECT * FROM sessions")
                .fetch_all(&self.pool)
                .await?,
        )?;
        entries.sort_by(compare_sidebar_order);
        Ok(entries)
    }

    pub async fn list_by_agent_id(&self, agent_id: &str) -> Result<Vec<SessionEntry>> {
        decode_rows(
            sqlx::query_as::<_, SessionRow>(
                "SELECT * FROM sessions WHERE agent_id = ? ORDER BY created_at ASC",
            )
            .bind(agent_id)
            .fetch_all(&self.pool)
            .await?,
        )
    }

    pub async fn delete_by_agent_id(&self, agent_id: &str) -> Result<u64> {
        Ok(sqlx::query("DELETE FROM sessions WHERE agent_id = ?")
            .bind(agent_id)
            .execute(&self.pool)
            .await?
            .rows_affected())
    }

    pub async fn list_children_result(&self, parent_key: &str) -> Result<Vec<SessionEntry>> {
        self.list_children(parent_key).await
    }

    pub async fn list_children(&self, parent_key: &str) -> Result<Vec<SessionEntry>> {
        decode_rows(
            sqlx::query_as::<_, SessionRow>(
                "SELECT * FROM sessions WHERE parent_session_key = ? ORDER BY created_at ASC",
            )
            .bind(parent_key)
            .fetch_all(&self.pool)
            .await?,
        )
    }

    pub async fn remove_session_tree(
        &self,
        root_key: &str,
        expected_keys: &[String],
    ) -> Result<Vec<SessionEntry>> {
        let expected: HashSet<&str> = expected_keys.iter().map(String::as_str).collect();
        if expected.len() != expected_keys.len() || !expected.contains(root_key) {
            return Err(Error::message("invalid expected session delete set"));
        }

        let mut transaction = self.pool.begin().await?;
        let actual_entries = decode_rows(
            sqlx::query_as::<_, SessionRow>(
                r#"WITH RECURSIVE descendants(key) AS (
                       SELECT key FROM sessions WHERE key = ?
                       UNION
                       SELECT sessions.key
                       FROM sessions
                       JOIN descendants ON sessions.parent_session_key = descendants.key
                   )
                   SELECT sessions.*
                   FROM sessions
                   JOIN descendants ON sessions.key = descendants.key"#,
            )
            .bind(root_key)
            .fetch_all(&mut *transaction)
            .await?,
        )?;
        let actual: HashSet<&str> = actual_entries
            .iter()
            .map(|entry| entry.key.as_str())
            .collect();
        if actual != expected {
            return Err(Error::message(format!(
                "session delete tree changed before commit: expected {expected:?}, found {actual:?}"
            )));
        }

        let mut deleted_entries = Vec::with_capacity(expected_keys.len());
        for key in expected_keys {
            let entry = actual_entries
                .iter()
                .find(|entry| entry.key == *key)
                .cloned()
                .ok_or_else(|| Error::message(format!("session '{key}' not found")))?;
            sqlx::query("DELETE FROM channel_sessions WHERE session_key = ?")
                .bind(key)
                .execute(&mut *transaction)
                .await?;
            let result = sqlx::query("DELETE FROM sessions WHERE key = ?")
                .bind(key)
                .execute(&mut *transaction)
                .await?;
            require_existing_row(key, result.rows_affected())?;
            deleted_entries.push(entry);
        }
        transaction.commit().await?;
        for entry in &deleted_entries {
            self.emit(crate::session_events::SessionEvent::Deleted {
                session_key: entry.key.clone(),
            });
        }
        Ok(deleted_entries)
    }

    pub async fn remove(&self, key: &str) -> Result<Option<SessionEntry>> {
        let mut transaction = self.pool.begin().await?;
        let entry = Self::fetch_entry_in_transaction(&mut transaction, key).await?;
        if entry.is_none() {
            transaction.commit().await?;
            return Ok(None);
        }
        sqlx::query("DELETE FROM sessions WHERE key = ?")
            .bind(key)
            .execute(&mut *transaction)
            .await?;
        transaction.commit().await?;
        self.emit(crate::session_events::SessionEvent::Deleted {
            session_key: key.to_string(),
        });
        Ok(entry)
    }

    pub async fn get_active_session(
        &self,
        channel_type: &str,
        account_id: &str,
        chat_id: &str,
        thread_id: Option<&str>,
    ) -> Result<Option<String>> {
        Ok(sqlx::query_scalar::<_, String>(
            r#"SELECT session_key FROM channel_sessions
               WHERE channel_type = ? AND account_id = ? AND chat_id = ? AND thread_id = ?"#,
        )
        .bind(channel_type)
        .bind(account_id)
        .bind(chat_id)
        .bind(thread_id.unwrap_or(""))
        .fetch_optional(&self.pool)
        .await?)
    }

    pub async fn set_active_session(
        &self,
        channel_type: &str,
        account_id: &str,
        chat_id: &str,
        thread_id: Option<&str>,
        session_key: &str,
    ) -> Result<()> {
        let now = now_ms();
        sqlx::query(
            r#"INSERT INTO channel_sessions (
                   channel_type, account_id, chat_id, thread_id, session_key, updated_at
               ) VALUES (?, ?, ?, ?, ?, ?)
               ON CONFLICT(channel_type, account_id, chat_id, thread_id) DO UPDATE SET
                   session_key = excluded.session_key,
                   updated_at = excluded.updated_at"#,
        )
        .bind(channel_type)
        .bind(account_id)
        .bind(chat_id)
        .bind(thread_id.unwrap_or(""))
        .bind(session_key)
        .bind(now)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn clear_active_session_mappings(&self, session_key: &str) -> Result<()> {
        sqlx::query("DELETE FROM channel_sessions WHERE session_key = ?")
            .bind(session_key)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn list_channel_sessions(
        &self,
        channel_type: &str,
        account_id: &str,
        chat_id: &str,
    ) -> Result<Vec<SessionEntry>> {
        let binding_pattern = format!(
            r#"%"channel_type":"{channel_type}"%"account_id":"{account_id}"%"chat_id":"{chat_id}"%"#,
        );
        decode_rows(
            sqlx::query_as::<_, SessionRow>(
                "SELECT * FROM sessions WHERE channel_binding LIKE ? ORDER BY created_at ASC",
            )
            .bind(binding_pattern)
            .fetch_all(&self.pool)
            .await?,
        )
    }

    pub async fn list_account_sessions(
        &self,
        channel_type: &str,
        account_id: &str,
    ) -> Result<Vec<SessionEntry>> {
        let pattern = format!(r#"%"channel_type":"{channel_type}"%"account_id":"{account_id}"%"#,);
        decode_rows(
            sqlx::query_as::<_, SessionRow>(
                "SELECT * FROM sessions WHERE channel_binding LIKE ? ORDER BY created_at ASC",
            )
            .bind(pattern)
            .fetch_all(&self.pool)
            .await?,
        )
    }

    pub async fn list_active_sessions(
        &self,
        channel_type: &str,
        account_id: &str,
    ) -> Result<Vec<(String, String)>> {
        Ok(sqlx::query_as::<_, (String, String)>(
            "SELECT chat_id, session_key FROM channel_sessions WHERE channel_type = ? AND account_id = ?",
        )
        .bind(channel_type)
        .bind(account_id)
        .fetch_all(&self.pool)
        .await?)
    }
}

#[cfg(test)]
mod tests;
