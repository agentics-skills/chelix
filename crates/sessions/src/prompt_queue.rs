//! Durable per-session queue of user prompts.
//!
//! A prompt enters the queue when the user submits it while an agent run
//! already owns the session. The queue survives page reloads, reconnects, and
//! gateway restarts, so every connected client renders the same pending
//! prompts instead of a client-local guess.

use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::{Error, Result};

/// Maximum characters kept in the client-facing preview of a queued prompt.
const PREVIEW_MAX_CHARS: usize = 500;

/// One queued user prompt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QueuedPrompt {
    /// Stable identifier used by clients to cancel a single prompt.
    pub id: String,
    /// Session the prompt belongs to.
    pub session_key: String,
    /// Monotonic position within the session queue.
    pub position: i64,
    /// Full `chat.send` request parameters, replayed verbatim after the run.
    pub params: serde_json::Value,
    /// Plain-text preview rendered by clients.
    pub preview: String,
    /// Creation timestamp in milliseconds since the Unix epoch.
    pub created_at: i64,
}

#[derive(sqlx::FromRow)]
struct QueuedPromptRow {
    id: String,
    session_key: String,
    position: i64,
    params: String,
    preview: String,
    created_at: i64,
}

impl TryFrom<QueuedPromptRow> for QueuedPrompt {
    type Error = Error;

    fn try_from(row: QueuedPromptRow) -> Result<Self> {
        Ok(Self {
            id: row.id,
            session_key: row.session_key,
            position: row.position,
            params: serde_json::from_str(&row.params)?,
            preview: row.preview,
            created_at: row.created_at,
        })
    }
}

/// Build the client-facing preview for a prompt payload.
#[must_use]
pub fn prompt_preview(params: &serde_json::Value) -> String {
    let text = params
        .get("text")
        .or_else(|| params.get("message"))
        .and_then(serde_json::Value::as_str)
        .map(str::to_string)
        .or_else(|| multimodal_text(params))
        .unwrap_or_default();
    truncate_preview(text.trim())
}

fn multimodal_text(params: &serde_json::Value) -> Option<String> {
    let blocks = params.get("content")?.as_array()?;
    let text = blocks
        .iter()
        .filter(|block| block.get("type").and_then(serde_json::Value::as_str) == Some("text"))
        .filter_map(|block| block.get("text").and_then(serde_json::Value::as_str))
        .collect::<Vec<_>>()
        .join("\n");
    Some(text)
}

fn truncate_preview(text: &str) -> String {
    match text.char_indices().nth(PREVIEW_MAX_CHARS) {
        Some((byte_index, _)) => format!("{}\u{2026}", &text[..byte_index]),
        None => text.to_string(),
    }
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}

/// SQLite-backed prompt queue.
#[derive(Debug)]
pub struct SessionPromptQueueStore {
    pool: sqlx::SqlitePool,
}

impl SessionPromptQueueStore {
    #[must_use]
    pub fn new(pool: sqlx::SqlitePool) -> Self {
        Self { pool }
    }

    /// Append a prompt to the end of a session queue.
    pub async fn push(
        &self,
        session_key: &str,
        params: &serde_json::Value,
    ) -> Result<QueuedPrompt> {
        let prompt = QueuedPrompt {
            id: uuid::Uuid::new_v4().to_string(),
            session_key: session_key.to_string(),
            position: self.next_position(session_key).await?,
            params: params.clone(),
            preview: prompt_preview(params),
            created_at: now_ms(),
        };

        sqlx::query(
            "INSERT INTO session_prompt_queue \
             (id, session_key, position, params, preview, created_at) \
             VALUES (?, ?, ?, ?, ?, ?)",
        )
        .bind(&prompt.id)
        .bind(&prompt.session_key)
        .bind(prompt.position)
        .bind(serde_json::to_string(&prompt.params)?)
        .bind(&prompt.preview)
        .bind(prompt.created_at)
        .execute(&self.pool)
        .await?;

        Ok(prompt)
    }

    async fn next_position(&self, session_key: &str) -> Result<i64> {
        let highest = sqlx::query_scalar::<_, Option<i64>>(
            "SELECT MAX(position) FROM session_prompt_queue WHERE session_key = ?",
        )
        .bind(session_key)
        .fetch_one(&self.pool)
        .await?;
        Ok(highest.unwrap_or(0) + 1)
    }

    /// List the pending prompts of a session in submission order.
    pub async fn list(&self, session_key: &str) -> Result<Vec<QueuedPrompt>> {
        let rows = sqlx::query_as::<_, QueuedPromptRow>(
            "SELECT id, session_key, position, params, preview, created_at \
             FROM session_prompt_queue WHERE session_key = ? ORDER BY position",
        )
        .bind(session_key)
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter().map(QueuedPrompt::try_from).collect()
    }

    /// Remove and return every pending prompt of a session.
    ///
    /// Claiming the queue in one statement is what makes a replay exclusive:
    /// a prompt is either still queued and cancellable, or already claimed by
    /// a replay, never both.
    pub async fn claim_all(&self, session_key: &str) -> Result<Vec<QueuedPrompt>> {
        let prompts = self.list(session_key).await?;
        if prompts.is_empty() {
            return Ok(prompts);
        }
        sqlx::query("DELETE FROM session_prompt_queue WHERE session_key = ?")
            .bind(session_key)
            .execute(&self.pool)
            .await?;
        Ok(prompts)
    }

    /// Put claimed prompts back at the head of the session queue.
    ///
    /// Used when a replay could not be started, so the prompts stay pending
    /// instead of being lost. Claiming empties the queue, which restarts
    /// [`SessionPromptQueueStore::next_position`] from one, so restored
    /// prompts are placed *below* whatever was queued in the meantime. That
    /// keeps them ahead of later prompts and avoids colliding positions, which
    /// would make the queue order ambiguous.
    pub async fn restore(&self, session_key: &str, prompts: &[QueuedPrompt]) -> Result<()> {
        let Some(last) = prompts.last() else {
            return Ok(());
        };
        let lowest = sqlx::query_scalar::<_, Option<i64>>(
            "SELECT MIN(position) FROM session_prompt_queue WHERE session_key = ?",
        )
        .bind(session_key)
        .fetch_one(&self.pool)
        .await?;
        // Without newer prompts the original positions are still free.
        let base = lowest.unwrap_or(last.position + 1);
        let count = i64::try_from(prompts.len()).unwrap_or(i64::MAX);

        for (offset, prompt) in prompts.iter().enumerate() {
            let position = base - count + i64::try_from(offset).unwrap_or(0);
            sqlx::query(
                "INSERT OR REPLACE INTO session_prompt_queue \
                 (id, session_key, position, params, preview, created_at) \
                 VALUES (?, ?, ?, ?, ?, ?)",
            )
            .bind(&prompt.id)
            .bind(session_key)
            .bind(position)
            .bind(serde_json::to_string(&prompt.params)?)
            .bind(&prompt.preview)
            .bind(prompt.created_at)
            .execute(&self.pool)
            .await?;
        }
        Ok(())
    }

    /// Remove one prompt by id. Returns `true` when a row was removed.
    pub async fn remove(&self, session_key: &str, prompt_id: &str) -> Result<bool> {
        let result =
            sqlx::query("DELETE FROM session_prompt_queue WHERE session_key = ? AND id = ?")
                .bind(session_key)
                .bind(prompt_id)
                .execute(&self.pool)
                .await?;
        Ok(result.rows_affected() > 0)
    }

    /// Remove every prompt of a session. Returns the number of removed rows.
    pub async fn clear(&self, session_key: &str) -> Result<u64> {
        let result = sqlx::query("DELETE FROM session_prompt_queue WHERE session_key = ?")
            .bind(session_key)
            .execute(&self.pool)
            .await?;
        Ok(result.rows_affected())
    }
}

#[allow(clippy::unwrap_used, clippy::expect_used)]
#[cfg(test)]
mod tests {
    use super::*;

    fn previews_of(prompts: &[QueuedPrompt]) -> Vec<String> {
        prompts.iter().map(|p| p.preview.clone()).collect()
    }

    fn ids_of(prompts: &[QueuedPrompt]) -> Vec<String> {
        prompts.iter().map(|p| p.id.clone()).collect()
    }

    async fn test_store() -> SessionPromptQueueStore {
        let pool = sqlx::SqlitePool::connect("sqlite::memory:").await.unwrap();
        sqlx::query(
            r#"CREATE TABLE session_prompt_queue (
                id          TEXT PRIMARY KEY,
                session_key TEXT NOT NULL,
                position    INTEGER NOT NULL,
                params      TEXT NOT NULL,
                preview     TEXT NOT NULL,
                created_at  INTEGER NOT NULL
            )"#,
        )
        .execute(&pool)
        .await
        .unwrap();
        SessionPromptQueueStore::new(pool)
    }

    fn text_params(text: &str) -> serde_json::Value {
        serde_json::json!({ "text": text })
    }

    #[tokio::test]
    async fn push_assigns_increasing_positions() {
        let store = test_store().await;

        let first = store.push("s1", &text_params("one")).await.unwrap();
        let second = store.push("s1", &text_params("two")).await.unwrap();

        assert!(second.position > first.position);
    }

    #[tokio::test]
    async fn list_returns_submission_order() {
        let store = test_store().await;
        store.push("s1", &text_params("one")).await.unwrap();
        store.push("s1", &text_params("two")).await.unwrap();

        let prompts = store.list("s1").await.unwrap();

        let previews: Vec<&str> = prompts.iter().map(|p| p.preview.as_str()).collect();
        assert_eq!(previews, vec!["one", "two"]);
    }

    #[tokio::test]
    async fn list_is_session_scoped() {
        let store = test_store().await;
        store.push("s1", &text_params("one")).await.unwrap();
        store.push("s2", &text_params("other")).await.unwrap();

        let prompts = store.list("s1").await.unwrap();

        assert_eq!(prompts.len(), 1);
        assert_eq!(prompts[0].preview, "one");
    }

    #[tokio::test]
    async fn claim_all_empties_the_session_queue() {
        let store = test_store().await;
        store.push("s1", &text_params("one")).await.unwrap();
        store.push("s1", &text_params("two")).await.unwrap();

        let claimed = store.claim_all("s1").await.unwrap();

        assert_eq!(claimed.len(), 2);
        assert!(store.list("s1").await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn claim_all_keeps_other_sessions() {
        let store = test_store().await;
        store.push("s1", &text_params("one")).await.unwrap();
        store.push("s2", &text_params("other")).await.unwrap();

        store.claim_all("s1").await.unwrap();

        assert_eq!(store.list("s2").await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn restore_puts_claimed_prompts_back_in_order() {
        let store = test_store().await;
        store.push("s1", &text_params("one")).await.unwrap();
        store.push("s1", &text_params("two")).await.unwrap();
        let claimed = store.claim_all("s1").await.unwrap();

        store.restore("s1", &claimed).await.unwrap();

        let restored = store.list("s1").await.unwrap();
        assert_eq!(previews_of(&restored), vec!["one", "two"]);
        assert_eq!(ids_of(&restored), ids_of(&claimed));
    }

    /// A prompt queued while the replay was starting must not overtake the
    /// prompts the user submitted earlier.
    #[tokio::test]
    async fn restore_keeps_prompts_queued_after_the_claim() {
        let store = test_store().await;
        store.push("s1", &text_params("one")).await.unwrap();
        store.push("s1", &text_params("two")).await.unwrap();
        let claimed = store.claim_all("s1").await.unwrap();
        store.push("s1", &text_params("late")).await.unwrap();

        store.restore("s1", &claimed).await.unwrap();

        let restored = store.list("s1").await.unwrap();
        assert_eq!(previews_of(&restored), vec!["one", "two", "late"]);
    }

    /// Restoring twice must stay ordered: positions may not collide.
    #[tokio::test]
    async fn restore_is_stable_across_repeated_attempts() {
        let store = test_store().await;
        store.push("s1", &text_params("one")).await.unwrap();
        store.push("s1", &text_params("two")).await.unwrap();

        for _ in 0..3 {
            let claimed = store.claim_all("s1").await.unwrap();
            store.restore("s1", &claimed).await.unwrap();
        }

        let restored = store.list("s1").await.unwrap();
        assert_eq!(previews_of(&restored), vec!["one", "two"]);
    }

    #[tokio::test]
    async fn restore_ignores_an_empty_claim() {
        let store = test_store().await;
        store.push("s1", &text_params("one")).await.unwrap();

        store.restore("s1", &[]).await.unwrap();

        assert_eq!(previews_of(&store.list("s1").await.unwrap()), vec!["one"]);
    }

    #[tokio::test]
    async fn remove_deletes_a_single_prompt() {
        let store = test_store().await;
        let first = store.push("s1", &text_params("one")).await.unwrap();
        store.push("s1", &text_params("two")).await.unwrap();

        assert!(store.remove("s1", &first.id).await.unwrap());

        let prompts = store.list("s1").await.unwrap();
        assert_eq!(prompts.len(), 1);
        assert_eq!(prompts[0].preview, "two");
    }

    #[tokio::test]
    async fn remove_reports_missing_prompt() {
        let store = test_store().await;

        assert!(!store.remove("s1", "missing").await.unwrap());
    }

    #[tokio::test]
    async fn clear_reports_removed_count() {
        let store = test_store().await;
        store.push("s1", &text_params("one")).await.unwrap();
        store.push("s1", &text_params("two")).await.unwrap();

        assert_eq!(store.clear("s1").await.unwrap(), 2);
    }

    #[tokio::test]
    async fn params_round_trip_through_storage() {
        let store = test_store().await;
        let params = serde_json::json!({
            "text": "hello",
            "model": "gpt-5",
            "_document_files": [{ "stored_filename": "a.pdf" }],
        });

        store.push("s1", &params).await.unwrap();

        let prompts = store.list("s1").await.unwrap();
        assert_eq!(prompts[0].params, params);
    }

    #[test]
    fn preview_uses_multimodal_text_blocks() {
        let params = serde_json::json!({
            "content": [
                { "type": "text", "text": "describe this" },
                { "type": "image_url", "image_url": { "url": "data:image/png;base64,AA" } },
            ]
        });

        assert_eq!(prompt_preview(&params), "describe this");
    }

    #[test]
    fn preview_truncates_long_text() {
        let params = serde_json::json!({ "text": "\u{43f}".repeat(PREVIEW_MAX_CHARS + 10) });

        let preview = prompt_preview(&params);

        assert_eq!(preview.chars().count(), PREVIEW_MAX_CHARS + 1);
        assert!(preview.ends_with('\u{2026}'));
    }

    #[test]
    fn preview_is_empty_without_text() {
        let params = serde_json::json!({ "model": "gpt-5" });

        assert_eq!(prompt_preview(&params), "");
    }
}
