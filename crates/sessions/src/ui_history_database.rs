//! SQLite storage for semantic snapshots and canonical bindings.

use std::{path::PathBuf, sync::Arc};

use {
    sqlx::{
        Row, SqlitePool,
        sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions},
    },
    tokio::sync::OnceCell,
};

use crate::{
    Error, Result,
    ui_history_types::{UiEntry, UiGeneration, UiMessageId},
};

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum UiSessionAccess {
    Open,
    Discard,
}

pub(crate) struct UiDatabase {
    directory: PathBuf,
    pool: OnceCell<SqlitePool>,
}

pub(crate) struct UiSessionRow {
    pub generation: UiGeneration,
    pub revision: u64,
    pub next_position: u64,
    pub total_messages: u32,
    pub canonical_tail: usize,
    pub failure: Option<String>,
}

pub(crate) fn integer(value: u64) -> Result<i64> {
    i64::try_from(value)
        .map_err(|error| Error::message(format!("UI history integer overflow: {error}")))
}

pub(crate) fn unsigned(value: i64) -> Result<u64> {
    u64::try_from(value)
        .map_err(|error| Error::message(format!("invalid UI history integer: {error}")))
}

pub(crate) async fn write_entries(
    transaction: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    key: &str,
    entries: &[UiEntry],
) -> Result<()> {
    for entry in entries {
        sqlx::query("INSERT INTO ui_history_snapshots (session_key, message_id, position, revision, snapshot_json, search_text, run_id, canonical_start, canonical_end, canonical_record) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?) ON CONFLICT(session_key, message_id) DO UPDATE SET revision = excluded.revision, snapshot_json = excluded.snapshot_json, search_text = excluded.search_text, run_id = excluded.run_id, canonical_start = excluded.canonical_start, canonical_end = excluded.canonical_end, canonical_record = excluded.canonical_record")
            .bind(key).bind(&entry.snapshot.id.0).bind(integer(entry.snapshot.position)?)
            .bind(integer(entry.snapshot.revision)?).bind(serde_json::to_string(entry)?)
            .bind(crate::ui_history_projection::search_text(&entry.snapshot)?.to_lowercase()).bind(entry.snapshot.content.run_id())
            .bind(entry.canonical.map(|binding| i64::try_from(binding.start)).transpose().map_err(|error| Error::message(error.to_string()))?)
            .bind(entry.canonical.map(|binding| i64::try_from(binding.end)).transpose().map_err(|error| Error::message(error.to_string()))?)
            .bind(entry.canonical.and_then(|binding| binding.record_index).map(i64::try_from).transpose().map_err(|error| Error::message(error.to_string()))?)
            .execute(&mut **transaction).await?;
    }
    Ok(())
}

impl UiDatabase {
    pub(crate) fn new(directory: PathBuf) -> Arc<Self> {
        Arc::new(Self {
            directory,
            pool: OnceCell::new(),
        })
    }

    pub(crate) async fn pool(&self) -> Result<&SqlitePool> {
        self.pool
            .get_or_try_init(|| async {
                tokio::fs::create_dir_all(&self.directory).await?;
                let options = SqliteConnectOptions::new()
                    .filename(self.directory.join("ui-history.sqlite"))
                    .create_if_missing(true)
                    .foreign_keys(true)
                    .journal_mode(SqliteJournalMode::Wal);
                let pool = SqlitePoolOptions::new().connect_with(options).await?;
                crate::run_ui_history_migrations(&pool).await?;
                Ok(pool)
            })
            .await
    }

    pub(crate) async fn session(&self, key: &str, access: UiSessionAccess) -> Result<UiSessionRow> {
        let pool = self.pool().await?;
        let row = sqlx::query("SELECT * FROM ui_history_sessions WHERE session_key = ?")
            .bind(key)
            .fetch_optional(pool)
            .await?;
        if let Some(row) = row {
            return Ok(UiSessionRow {
                generation: UiGeneration(row.try_get("generation")?),
                revision: unsigned(row.try_get("revision")?)?,
                next_position: unsigned(row.try_get("next_position")?)?,
                total_messages: u32::try_from(unsigned(row.try_get("total_messages")?)?)
                    .map_err(|error| Error::message(error.to_string()))?,
                canonical_tail: usize::try_from(unsigned(row.try_get("canonical_tail")?)?)
                    .map_err(|error| Error::message(error.to_string()))?,
                failure: row.try_get("failure")?,
            });
        }
        let journal = self.directory.join(format!(
            "{}.jsonl",
            crate::store::SessionStore::key_to_filename(key)
        ));
        match tokio::fs::metadata(&journal).await {
            Ok(metadata) if metadata.len() > 0 && access == UiSessionAccess::Open => {
                return Err(Error::MissingUiSnapshots {
                    session_key: key.to_string(),
                });
            },
            Ok(_) => {},
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {},
            Err(error) => return Err(error.into()),
        }
        let generation = UiGeneration(uuid::Uuid::new_v4().to_string());
        sqlx::query("INSERT INTO ui_history_sessions (session_key, generation, revision, next_position, total_messages, canonical_tail) VALUES (?, ?, 0, 0, 0, 0)")
            .bind(key).bind(&generation.0).execute(pool).await?;
        Ok(UiSessionRow {
            generation,
            revision: 0,
            next_position: 0,
            total_messages: 0,
            canonical_tail: 0,
            failure: None,
        })
    }

    pub(crate) async fn entry(&self, key: &str, id: &UiMessageId) -> Result<Option<UiEntry>> {
        let json = sqlx::query_scalar::<_, String>("SELECT snapshot_json FROM ui_history_snapshots WHERE session_key = ? AND message_id = ?")
            .bind(key).bind(&id.0).fetch_optional(self.pool().await?).await?;
        json.map(|json| serde_json::from_str(&json).map_err(Error::from))
            .transpose()
    }

    pub(crate) async fn entries(
        &self,
        key: &str,
        lower: Option<u64>,
        upper: Option<u64>,
        newest_first: bool,
        limit: usize,
    ) -> Result<Vec<UiEntry>> {
        let sql = if newest_first {
            "SELECT snapshot_json FROM ui_history_snapshots WHERE session_key = ? AND (? IS NULL OR position > ?) AND (? IS NULL OR position < ?) ORDER BY position DESC LIMIT ?"
        } else {
            "SELECT snapshot_json FROM ui_history_snapshots WHERE session_key = ? AND (? IS NULL OR position > ?) AND (? IS NULL OR position < ?) ORDER BY position ASC LIMIT ?"
        };
        let lower = lower.map(integer).transpose()?;
        let upper = upper.map(integer).transpose()?;
        let rows = sqlx::query_scalar::<_, String>(sql)
            .bind(key)
            .bind(lower)
            .bind(lower)
            .bind(upper)
            .bind(upper)
            .bind(i64::try_from(limit).map_err(|error| Error::message(error.to_string()))?)
            .fetch_all(self.pool().await?)
            .await?;
        rows.into_iter()
            .map(|json| serde_json::from_str(&json).map_err(Error::from))
            .collect()
    }

    pub(crate) async fn search(
        &self,
        key: &str,
        query: &str,
        limit: usize,
    ) -> Result<Vec<UiEntry>> {
        let rows = sqlx::query_scalar::<_, String>("SELECT snapshot_json FROM ui_history_snapshots WHERE session_key = ? AND instr(search_text, ?) > 0 ORDER BY position LIMIT ?")
            .bind(key).bind(query)
            .bind(i64::try_from(limit).map_err(|error| Error::message(error.to_string()))?)
            .fetch_all(self.pool().await?).await?;
        rows.into_iter()
            .map(|json| serde_json::from_str(&json).map_err(Error::from))
            .collect()
    }
}
