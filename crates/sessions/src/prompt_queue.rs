//! Primitive FIFO service for prompts submitted while a session turn is active.

use {
    chelix_channels::{ChannelMessageKind, ChannelReplyTarget, ChannelType},
    chelix_common::MessageMedium,
    serde::{Deserialize, Serialize},
    sqlx::{Executor, Sqlite},
    tokio::sync::{mpsc, oneshot},
};

use crate::{Error, Result, SessionKey};

const COMMAND_CHANNEL_CAPACITY: usize = 1;

/// Plain text or ordered multimodal content of one queued user prompt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum QueuedPromptMessageContent {
    Text(String),
    Multimodal(Vec<QueuedPromptContentBlock>),
}

/// One closed multimodal content block.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum QueuedPromptContentBlock {
    Text { text: String },
    ImageUrl { image_url: QueuedPromptImageUrl },
}

/// Image reference carried by a multimodal content block.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QueuedPromptImageUrl {
    pub url: String,
}

/// Saved inbound document metadata required to reconstruct a user message.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct QueuedPromptDocument {
    pub display_name: String,
    pub stored_filename: String,
    pub mime_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size_bytes: Option<u64>,
    pub media_ref: String,
}

/// Closed channel metadata retained with an unchanged user message.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QueuedPromptChannelMetadata {
    pub channel_type: ChannelType,
    pub sender_name: Option<String>,
    pub username: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sender_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message_kind: Option<ChannelMessageKind>,
}

/// Closed content of one queued user prompt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct QueuedPromptContent {
    pub content: QueuedPromptMessageContent,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub documents: Vec<QueuedPromptDocument>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub audio: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub client_sequence: Option<u64>,
    pub input_medium: MessageMedium,
    pub reply_medium: MessageMedium,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub channel: Option<QueuedPromptChannelMetadata>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub channel_reply_target: Option<ChannelReplyTarget>,
}

impl QueuedPromptContent {
    fn to_json(&self) -> Result<String> {
        Ok(serde_json::to_string(self)?)
    }

    fn from_json(json: &str) -> Result<Self> {
        Ok(serde_json::from_str(json)?)
    }
}

/// One queued prompt in durable FIFO order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct QueuedPrompt {
    pub id: i64,
    #[serde(rename = "sessionKey")]
    pub session_id: SessionKey,
    pub content: QueuedPromptContent,
}

/// Canonical full queue status for one session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct QueuedPromptsStatus {
    #[serde(rename = "sessionKey")]
    pub session_id: SessionKey,
    pub prompts: Vec<QueuedPrompt>,
}

/// A removed prompt batch and the canonical status after its removal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueuedPromptsDrain {
    pub prompts: Vec<QueuedPrompt>,
    pub status: QueuedPromptsStatus,
}

#[derive(sqlx::FromRow)]
struct QueuedPromptRow {
    id: i64,
    session_key: String,
    content: String,
}

impl TryFrom<QueuedPromptRow> for QueuedPrompt {
    type Error = Error;

    fn try_from(row: QueuedPromptRow) -> Result<Self> {
        Ok(Self {
            id: row.id,
            session_id: SessionKey::new(row.session_key),
            content: QueuedPromptContent::from_json(&row.content)?,
        })
    }
}

struct QueuedPromptsStore {
    pool: sqlx::SqlitePool,
}

impl QueuedPromptsStore {
    fn new(pool: sqlx::SqlitePool) -> Self {
        Self { pool }
    }

    async fn enqueue(
        &self,
        session_id: &SessionKey,
        content: &QueuedPromptContent,
    ) -> Result<QueuedPromptsStatus> {
        sqlx::query("INSERT INTO session_prompt_queue (session_key, content) VALUES (?, ?)")
            .bind(session_id.as_str())
            .bind(content.to_json()?)
            .execute(&self.pool)
            .await?;
        self.status(session_id).await
    }

    async fn remove(&self, id: i64) -> Result<QueuedPromptsStatus> {
        let mut transaction = self.pool.begin().await?;
        let session_key = sqlx::query_scalar::<_, String>(
            "SELECT session_key FROM session_prompt_queue WHERE id = ?",
        )
        .bind(id)
        .fetch_optional(&mut *transaction)
        .await?
        .ok_or_else(|| Error::message(format!("queued prompt '{id}' not found")))?;
        let session_id = SessionKey::new(session_key);

        sqlx::query("DELETE FROM session_prompt_queue WHERE id = ?")
            .bind(id)
            .execute(&mut *transaction)
            .await?;
        let status = Self::read_status(&mut *transaction, &session_id).await?;
        transaction.commit().await?;
        Ok(status)
    }

    async fn drain(&self, session_id: &SessionKey) -> Result<QueuedPromptsDrain> {
        let mut transaction = self.pool.begin().await?;
        let prompts = Self::read_prompts(&mut *transaction, session_id).await?;
        sqlx::query("DELETE FROM session_prompt_queue WHERE session_key = ?")
            .bind(session_id.as_str())
            .execute(&mut *transaction)
            .await?;
        let status = Self::read_status(&mut *transaction, session_id).await?;
        transaction.commit().await?;
        Ok(QueuedPromptsDrain { prompts, status })
    }

    async fn clear(&self, session_id: &SessionKey) -> Result<()> {
        sqlx::query("DELETE FROM session_prompt_queue WHERE session_key = ?")
            .bind(session_id.as_str())
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    async fn status(&self, session_id: &SessionKey) -> Result<QueuedPromptsStatus> {
        Self::read_status(&self.pool, session_id).await
    }

    async fn read_status<'executor, E>(
        executor: E,
        session_id: &SessionKey,
    ) -> Result<QueuedPromptsStatus>
    where
        E: Executor<'executor, Database = Sqlite>,
    {
        Ok(QueuedPromptsStatus {
            session_id: session_id.clone(),
            prompts: Self::read_prompts(executor, session_id).await?,
        })
    }

    async fn read_prompts<'executor, E>(
        executor: E,
        session_id: &SessionKey,
    ) -> Result<Vec<QueuedPrompt>>
    where
        E: Executor<'executor, Database = Sqlite>,
    {
        let rows = sqlx::query_as::<_, QueuedPromptRow>(
            "SELECT id, session_key, content FROM session_prompt_queue \
             WHERE session_key = ? ORDER BY id ASC",
        )
        .bind(session_id.as_str())
        .fetch_all(executor)
        .await?;
        rows.into_iter().map(QueuedPrompt::try_from).collect()
    }
}

enum Command {
    Enqueue {
        session_id: SessionKey,
        content: Box<QueuedPromptContent>,
        reply: oneshot::Sender<Result<QueuedPromptsStatus>>,
    },
    Remove {
        id: i64,
        reply: oneshot::Sender<Result<QueuedPromptsStatus>>,
    },
    Drain {
        session_id: SessionKey,
        reply: oneshot::Sender<Result<QueuedPromptsDrain>>,
    },
    Clear {
        session_id: SessionKey,
        reply: oneshot::Sender<Result<()>>,
    },
    Status {
        session_id: SessionKey,
        reply: oneshot::Sender<Result<QueuedPromptsStatus>>,
    },
}

/// Primitive queued-prompts service backed by one sequential command receiver.
#[derive(Clone)]
pub struct QueuedPrompts {
    commands: mpsc::Sender<Command>,
}

impl QueuedPrompts {
    #[must_use]
    pub fn new(pool: sqlx::SqlitePool) -> Self {
        let (commands, receiver) = mpsc::channel(COMMAND_CHANNEL_CAPACITY);
        tokio::spawn(run_command_loop(QueuedPromptsStore::new(pool), receiver));
        Self { commands }
    }

    pub async fn enqueue(
        &self,
        session_id: SessionKey,
        content: QueuedPromptContent,
    ) -> Result<QueuedPromptsStatus> {
        let (reply, result) = oneshot::channel();
        self.commands
            .send(Command::Enqueue {
                session_id,
                content: Box::new(content),
                reply,
            })
            .await
            .map_err(|_| command_loop_unavailable())?;
        result.await.map_err(|_| command_loop_unavailable())?
    }

    pub async fn remove(&self, id: i64) -> Result<QueuedPromptsStatus> {
        let (reply, result) = oneshot::channel();
        self.commands
            .send(Command::Remove { id, reply })
            .await
            .map_err(|_| command_loop_unavailable())?;
        result.await.map_err(|_| command_loop_unavailable())?
    }

    pub async fn drain(&self, session_id: SessionKey) -> Result<QueuedPromptsDrain> {
        let (reply, result) = oneshot::channel();
        self.commands
            .send(Command::Drain { session_id, reply })
            .await
            .map_err(|_| command_loop_unavailable())?;
        result.await.map_err(|_| command_loop_unavailable())?
    }

    pub async fn clear(&self, session_id: SessionKey) -> Result<()> {
        let (reply, result) = oneshot::channel();
        self.commands
            .send(Command::Clear { session_id, reply })
            .await
            .map_err(|_| command_loop_unavailable())?;
        result.await.map_err(|_| command_loop_unavailable())?
    }

    pub async fn status(&self, session_id: SessionKey) -> Result<QueuedPromptsStatus> {
        let (reply, result) = oneshot::channel();
        self.commands
            .send(Command::Status { session_id, reply })
            .await
            .map_err(|_| command_loop_unavailable())?;
        result.await.map_err(|_| command_loop_unavailable())?
    }
}

async fn run_command_loop(store: QueuedPromptsStore, mut commands: mpsc::Receiver<Command>) {
    while let Some(command) = commands.recv().await {
        match command {
            Command::Enqueue {
                session_id,
                content,
                reply,
            } => {
                let _ = reply.send(store.enqueue(&session_id, &content).await);
            },
            Command::Remove { id, reply } => {
                let _ = reply.send(store.remove(id).await);
            },
            Command::Drain { session_id, reply } => {
                let _ = reply.send(store.drain(&session_id).await);
            },
            Command::Clear { session_id, reply } => {
                let _ = reply.send(store.clear(&session_id).await);
            },
            Command::Status { session_id, reply } => {
                let _ = reply.send(store.status(&session_id).await);
            },
        }
    }
}

fn command_loop_unavailable() -> Error {
    Error::message("queuedPrompts command loop is unavailable")
}

#[allow(clippy::unwrap_used, clippy::expect_used)]
#[cfg(test)]
mod tests {
    use sqlx::sqlite::SqlitePoolOptions;

    use super::*;

    async fn sqlite_pool() -> sqlx::SqlitePool {
        SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap()
    }

    async fn test_service() -> QueuedPrompts {
        let pool = sqlite_pool().await;
        sqlx::query(
            r#"CREATE TABLE session_prompt_queue (
                id          INTEGER PRIMARY KEY AUTOINCREMENT,
                session_key TEXT    NOT NULL,
                content     TEXT    NOT NULL
            )"#,
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "CREATE INDEX idx_session_prompt_queue_session \
             ON session_prompt_queue(session_key, id)",
        )
        .execute(&pool)
        .await
        .unwrap();
        QueuedPrompts::new(pool)
    }

    async fn legacy_queue_pool() -> sqlx::SqlitePool {
        let pool = sqlite_pool().await;
        sqlx::raw_sql(include_str!(
            "../migrations/20260815090000_session_prompt_queue.sql"
        ))
        .execute(&pool)
        .await
        .unwrap();
        pool
    }

    fn content(text: &str) -> QueuedPromptContent {
        QueuedPromptContent {
            content: QueuedPromptMessageContent::Text(text.to_string()),
            documents: Vec::new(),
            audio: None,
            client_sequence: None,
            input_medium: MessageMedium::Text,
            reply_medium: MessageMedium::Text,
            channel: None,
            channel_reply_target: None,
        }
    }

    #[tokio::test]
    async fn migration_rebuilds_only_an_empty_incompatible_queue() {
        let empty_pool = legacy_queue_pool().await;
        sqlx::raw_sql(include_str!(
            "../migrations/20260902190000_primitive_queued_prompts.sql"
        ))
        .execute(&empty_pool)
        .await
        .unwrap();
        let columns = sqlx::query_scalar::<_, String>(
            "SELECT name FROM pragma_table_info('session_prompt_queue') ORDER BY cid",
        )
        .fetch_all(&empty_pool)
        .await
        .unwrap();
        assert_eq!(columns, ["id", "session_key", "content"]);

        let incompatible_pool = legacy_queue_pool().await;
        sqlx::query(
            "INSERT INTO session_prompt_queue VALUES ('legacy-id', 's1', 0, '{}', 'text', 1)",
        )
        .execute(&incompatible_pool)
        .await
        .unwrap();
        let migration = sqlx::raw_sql(include_str!(
            "../migrations/20260902190000_primitive_queued_prompts.sql"
        ))
        .execute(&incompatible_pool)
        .await;
        assert!(migration.is_err());
        let unchanged: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM session_prompt_queue")
            .fetch_one(&incompatible_pool)
            .await
            .unwrap();
        assert_eq!(unchanged, 1);
    }

    #[tokio::test]
    async fn enqueue_assigns_increasing_ids_and_status_is_ordered() {
        let service = test_service().await;
        let session = SessionKey::new("s1");

        let first = service
            .enqueue(session.clone(), content("one"))
            .await
            .unwrap();
        let second = service
            .enqueue(session.clone(), content("two"))
            .await
            .unwrap();
        let status = service.status(session).await.unwrap();

        assert_eq!(first.prompts.len(), 1);
        assert_eq!(second.prompts.len(), 2);
        assert_eq!(status.prompts, second.prompts);
        assert!(status.prompts[0].id < status.prompts[1].id);

        let prompt = serde_json::to_value(&status.prompts[0]).unwrap();
        let mut fields = prompt
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect::<Vec<_>>();
        fields.sort_unstable();
        assert_eq!(fields, ["content", "id", "sessionKey"]);
    }

    #[test]
    fn content_parser_accepts_the_canonical_payload() {
        let parsed: QueuedPromptContent = serde_json::from_value(serde_json::json!({
            "content": "hello",
            "inputMedium": "text",
            "replyMedium": "text",
        }))
        .unwrap();

        assert_eq!(parsed, content("hello"));
    }

    #[test]
    fn content_parser_rejects_an_additional_field() {
        let json = serde_json::json!({
            "content": "hello",
            "inputMedium": "text",
            "replyMedium": "text",
            "channelReplyTarget": {
                "channel_type": "telegram",
                "account_id": "account",
                "chat_id": "chat",
                "unexpected": true,
            },
        });

        assert!(serde_json::from_value::<QueuedPromptContent>(json).is_err());
    }

    #[tokio::test]
    async fn remove_uses_only_id_and_returns_the_owner_status() {
        let service = test_service().await;
        let first_status = service
            .enqueue(SessionKey::new("s1"), content("one"))
            .await
            .unwrap();
        service
            .enqueue(SessionKey::new("s1"), content("two"))
            .await
            .unwrap();
        service
            .enqueue(SessionKey::new("s2"), content("other"))
            .await
            .unwrap();

        let status = service.remove(first_status.prompts[0].id).await.unwrap();

        assert_eq!(status.session_id, SessionKey::new("s1"));
        assert_eq!(status.prompts.len(), 1);
        assert_eq!(
            status.prompts[0].content.content,
            QueuedPromptMessageContent::Text("two".to_string())
        );
        assert!(service.remove(i64::MAX).await.is_err());
    }

    #[tokio::test]
    async fn drain_removes_one_ordered_session_batch() {
        let service = test_service().await;
        let session = SessionKey::new("s1");
        service
            .enqueue(session.clone(), content("one"))
            .await
            .unwrap();
        service
            .enqueue(SessionKey::new("s2"), content("other"))
            .await
            .unwrap();
        service
            .enqueue(session.clone(), content("two"))
            .await
            .unwrap();

        let drain = service.drain(session.clone()).await.unwrap();

        assert_eq!(drain.prompts.len(), 2);
        assert!(drain.prompts[0].id < drain.prompts[1].id);
        assert!(drain.status.prompts.is_empty());
        assert!(service.status(session).await.unwrap().prompts.is_empty());
        assert_eq!(
            service
                .status(SessionKey::new("s2"))
                .await
                .unwrap()
                .prompts
                .len(),
            1
        );
    }

    #[tokio::test]
    async fn clear_removes_only_the_selected_session() {
        let service = test_service().await;
        service
            .enqueue(SessionKey::new("s1"), content("one"))
            .await
            .unwrap();
        service
            .enqueue(SessionKey::new("s2"), content("other"))
            .await
            .unwrap();

        service.clear(SessionKey::new("s1")).await.unwrap();

        assert!(
            service
                .status(SessionKey::new("s1"))
                .await
                .unwrap()
                .prompts
                .is_empty()
        );
        assert_eq!(
            service
                .status(SessionKey::new("s2"))
                .await
                .unwrap()
                .prompts
                .len(),
            1
        );
    }
}
