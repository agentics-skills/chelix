//! Agent tool for forking the current session into a new branch.

use std::sync::Arc;

use {
    async_trait::async_trait,
    chelix_agents::tool_registry::AgentTool,
    chelix_sessions::{metadata::SqliteSessionMetadata, store::SessionStore},
    serde_json::{Value, json},
};

use crate::error::Error;

/// Agent tool that forks the current session at a given message index.
pub struct BranchSessionTool {
    store: Arc<SessionStore>,
    metadata: Arc<SqliteSessionMetadata>,
}

impl BranchSessionTool {
    pub fn new(store: Arc<SessionStore>, metadata: Arc<SqliteSessionMetadata>) -> Self {
        Self { store, metadata }
    }
}

#[async_trait]
impl AgentTool for BranchSessionTool {
    fn name(&self) -> &str {
        "branch_session"
    }

    fn description(&self) -> &str {
        "Fork the current session into a new branch at a given message index. \
         Messages up to fork_point are copied to the new session. \
         The new session inherits the parent's model, reasoning effort, agent, and project."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["label"],
            "properties": {
                "label": {
                    "type": "string",
                    "description": "Label for the new branched session"
                },
                "fork_point": {
                    "type": "integer",
                    "description": "Message index to fork at (0-based, exclusive). Defaults to all messages."
                }
            }
        })
    }

    async fn execute(&self, params: Value) -> anyhow::Result<Value> {
        let parent_key = params
            .get("_session_key")
            .and_then(Value::as_str)
            .ok_or_else(|| Error::message("missing session context"))?;
        let label = params
            .get("label")
            .and_then(Value::as_str)
            .ok_or_else(|| Error::message("missing 'label'"))?;

        let parent = self
            .metadata
            .get(parent_key)
            .await?
            .ok_or_else(|| Error::message(format!("session '{parent_key}' not found")))?;
        let model_reasoning = parent.model_reasoning().cloned().ok_or_else(|| {
            Error::message(format!(
                "session '{parent_key}' has no LLM model/reasoning pair"
            ))
        })?;
        let messages = self.store.read(parent_key).await?;
        let message_count = messages.len();
        let fork_point = params
            .get("fork_point")
            .and_then(Value::as_u64)
            .map(|value| value as usize)
            .unwrap_or(message_count);
        if fork_point > message_count {
            return Err(Error::message(format!(
                "fork_point {fork_point} exceeds message count {message_count}"
            ))
            .into());
        }

        let new_key = format!("session:{}", uuid::Uuid::new_v4());
        self.store
            .replace_history(&new_key, messages[..fork_point].to_vec())
            .await?;
        self.metadata
            .create_llm_session(
                &new_key,
                Some(label),
                &model_reasoning,
                parent.agent_id.as_deref(),
            )
            .await?;
        let ui_message_count = self.store.ui_message_count(&new_key).await?;
        self.metadata.touch(&new_key, ui_message_count).await?;
        if let Some(project_id) = parent.project_id.as_deref() {
            self.metadata
                .set_project_id(&new_key, Some(project_id))
                .await?;
        }
        self.metadata
            .set_parent(&new_key, Some(parent_key), Some(fork_point as u32))
            .await?;
        let entry = self
            .metadata
            .get(&new_key)
            .await?
            .ok_or_else(|| Error::message(format!("session '{new_key}' disappeared")))?;

        Ok(json!({
            "sessionKey": new_key,
            "id": entry.id,
            "label": label,
            "forkPoint": fork_point,
            "messageCount": fork_point,
        }))
    }
}

#[allow(clippy::unwrap_used, clippy::expect_used)]
#[cfg(test)]
mod tests {
    use {
        super::*,
        chelix_common::{ReasoningEffort, ResolvedModelReasoning},
        chelix_sessions::{MessageContent, PersistedMessage},
    };

    fn user_message(text: impl Into<String>) -> Value {
        PersistedMessage::User {
            content: MessageContent::Text(text.into()),
            created_at: None,
            audio: None,
            documents: None,
            channel: None,
            seq: None,
            run_id: None,
        }
        .to_value()
    }

    async fn setup() -> (
        Arc<SessionStore>,
        Arc<SqliteSessionMetadata>,
        tempfile::TempDir,
    ) {
        let temp_dir = tempfile::tempdir().unwrap();
        let store = Arc::new(SessionStore::new(temp_dir.path().to_path_buf()));
        let pool = sqlx::SqlitePool::connect("sqlite::memory:").await.unwrap();
        sqlx::query("CREATE TABLE projects (id TEXT PRIMARY KEY)")
            .execute(&pool)
            .await
            .unwrap();
        SqliteSessionMetadata::init(&pool).await.unwrap();
        (store, Arc::new(SqliteSessionMetadata::new(pool)), temp_dir)
    }

    #[tokio::test]
    async fn branch_inherits_the_complete_pair() {
        let (store, metadata, _temp_dir) = setup().await;
        let tool = BranchSessionTool::new(Arc::clone(&store), Arc::clone(&metadata));
        let parent_key = "session:parent";
        let pair = ResolvedModelReasoning::try_new(
            "test::model".to_string(),
            ReasoningEffort::from("high"),
        )
        .unwrap();
        metadata
            .create_llm_session(parent_key, Some("Parent"), &pair, Some("main"))
            .await
            .unwrap();
        for index in 0..4 {
            store
                .append(parent_key, &user_message(format!("message {index}")))
                .await
                .unwrap();
        }
        metadata.touch(parent_key, 4).await.unwrap();

        let result = tool
            .execute(json!({
                "label": "Branch",
                "fork_point": 2,
                "_session_key": parent_key,
            }))
            .await
            .unwrap();
        let new_key = result["sessionKey"].as_str().unwrap();
        let child = metadata.get(new_key).await.unwrap().unwrap();
        assert_eq!(child.model(), Some("test::model"));
        assert_eq!(
            child.reasoning_effort().map(ReasoningEffort::as_str),
            Some("high")
        );
        assert_eq!(child.agent_id.as_deref(), Some("main"));
        assert_eq!(store.read(new_key).await.unwrap().len(), 2);
    }

    #[tokio::test]
    async fn missing_parent_metadata_is_rejected_before_history_changes() {
        let (store, metadata, _temp_dir) = setup().await;
        let tool = BranchSessionTool::new(Arc::clone(&store), Arc::clone(&metadata));
        let parent_key = "session:missing";
        store
            .append(parent_key, &user_message("unchanged"))
            .await
            .unwrap();

        let result = tool
            .execute(json!({
                "label": "Rejected",
                "_session_key": parent_key,
            }))
            .await;
        assert!(result.is_err());
        assert_eq!(store.read(parent_key).await.unwrap().len(), 1);
        assert!(metadata.list().await.unwrap().is_empty());
    }
}
