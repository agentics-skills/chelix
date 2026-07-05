//! Session management tools for creating and deleting chat sessions.
//!
//! These tools expose explicit session lifecycle operations to the agent:
//! - `sessions_explore`: list available agents for session creation
//! - `sessions_create`: create a generated session key
//! - `sessions_delete`: delete a session and its history

use std::sync::Arc;

use {async_trait::async_trait, futures::future::BoxFuture, serde_json::Value};

use {moltis_agents::tool_registry::AgentTool, moltis_sessions::metadata::SqliteSessionMetadata};

use crate::{
    Error,
    params::{bool_param, owned_str_param, require_str, str_param, without_null_params},
    session_model_override::{ModelOverride, model_override_schema, parse_model_override},
};

/// Request payload for session creation.
#[derive(Debug, Clone)]
pub struct CreateSessionRequest {
    pub key: String,
    pub agent_id: String,
    pub label: Option<String>,
    pub model_override: Option<ModelOverride>,
    pub project_id: Option<String>,
    /// Session that spawned this one. Drives the parent/child tree in the
    /// UI (same mechanism as session forks).
    pub parent_session_key: Option<String>,
}

/// Callback used by `sessions_create`.
pub type CreateSessionFn =
    Arc<dyn Fn(CreateSessionRequest) -> BoxFuture<'static, crate::Result<Value>> + Send + Sync>;

/// Callback used by `sessions_explore`.
pub type ExploreSessionsFn =
    Arc<dyn Fn() -> BoxFuture<'static, crate::Result<Value>> + Send + Sync>;

/// Request payload for session deletion.
#[derive(Debug, Clone)]
pub struct DeleteSessionRequest {
    pub key: String,
    pub force: bool,
}

/// Callback used by `sessions_delete`.
pub type DeleteSessionFn =
    Arc<dyn Fn(DeleteSessionRequest) -> BoxFuture<'static, crate::Result<Value>> + Send + Sync>;

/// Tool for discovering available session agents.
pub struct SessionsExploreTool {
    explore_fn: ExploreSessionsFn,
}

impl SessionsExploreTool {
    pub fn new(explore_fn: ExploreSessionsFn) -> Self {
        Self { explore_fn }
    }
}

/// Tool for creating sessions.
pub struct SessionsCreateTool {
    create_fn: CreateSessionFn,
}

impl SessionsCreateTool {
    pub fn new(create_fn: CreateSessionFn) -> Self {
        Self { create_fn }
    }
}

/// Tool for deleting sessions.
pub struct SessionsDeleteTool {
    metadata: Arc<SqliteSessionMetadata>,
    delete_fn: DeleteSessionFn,
}

impl SessionsDeleteTool {
    pub fn new(metadata: Arc<SqliteSessionMetadata>, delete_fn: DeleteSessionFn) -> Self {
        Self {
            metadata,
            delete_fn,
        }
    }
}

#[async_trait]
impl AgentTool for SessionsExploreTool {
    fn name(&self) -> &str {
        "sessions_explore"
    }

    fn description(&self) -> &str {
        "List every available agent that can be used with sessions_create. \
         Returns each agent's id, name, description, and preset model configuration. \
         Call this before sessions_create to choose an explicit agent_id."
    }

    fn parameters_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "additionalProperties": false
        })
    }

    async fn execute(&self, _params: Value) -> anyhow::Result<Value> {
        (self.explore_fn)().await.map_err(Into::into)
    }
}

#[async_trait]
impl AgentTool for SessionsCreateTool {
    fn name(&self) -> &str {
        "sessions_create"
    }

    fn description(&self) -> &str {
        "Create a new chat session with a generated session:<uuid> key for an explicit agent. \
         The agent_id parameter is required; call sessions_explore first to discover valid agents. \
            The generated key is returned in the result and should be used for later session tools. \
         Omit model_override to use the selected agent's preset model; provide model_override only for intentional advanced overrides."
    }

    fn parameters_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "additionalProperties": false,
            "properties": {
                "agent_id": {
                    "description": "Required agent id from sessions_explore. No default or fallback is applied.",
                    "minLength": 1,
                    "type": "string"
                },
                "label": {
                    "description": "Optional session label.",
                    "minLength": 1,
                    "type": "string"
                },
                "project_id": {
                    "description": "Optional project ID to associate with the session. Do not pass null or empty strings; omit the field instead.",
                    "minLength": 1,
                    "type": "string"
                },
                "model_override": model_override_schema()
            },
            "required": ["agent_id"]
        })
    }

    async fn execute(&self, params: Value) -> anyhow::Result<Value> {
        let params = without_null_params(params);
        if params.get("inherit_agent_from").is_some() || params.get("inheritAgentFrom").is_some() {
            return Err(Error::message(
                "inherit_agent_from is not supported by sessions_create; pass an explicit agent_id",
            )
            .into());
        }
        if params.get("key").is_some() {
            return Err(Error::message(
                "key is not supported by sessions_create; use the generated key returned by the tool",
            )
            .into());
        }
        let agent_id = require_str(&params, "agent_id")?.to_string();
        let key = format!("session:{}", uuid::Uuid::new_v4());

        let label = owned_str_param(&params, &["label"]);
        let model_override = parse_model_override(&params)?;
        let project_id = owned_str_param(&params, &["project_id", "projectId"]);

        // Link the new session to its creator so the UI renders it as a
        // child (same tree mechanism as forks). `_session_key` is injected
        // into tool params by the agent runner.
        let parent_session_key = str_param(&params, "_session_key").map(String::from);

        let req = CreateSessionRequest {
            key: key.clone(),
            agent_id: agent_id.clone(),
            label,
            model_override,
            project_id,
            parent_session_key,
        };
        let result = (self.create_fn)(req).await?;
        let agent_id = agent_id.as_str();

        Ok(serde_json::json!({
            "key": key,
            "agent_id": agent_id,
            "agentId": agent_id,
            "result": result,
        }))
    }
}

#[async_trait]
impl AgentTool for SessionsDeleteTool {
    fn name(&self) -> &str {
        "sessions_delete"
    }

    fn description(&self) -> &str {
        "Delete a chat session and its history by key. \
         Deleting the main session is not allowed."
    }

    fn parameters_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "key": {
                    "type": "string",
                    "description": "Session key to delete."
                },
                "force": {
                    "type": "boolean",
                    "description": "Force deletion for sessions with worktree checks (default: false)."
                }
            },
            "required": ["key"]
        })
    }

    async fn execute(&self, params: Value) -> anyhow::Result<Value> {
        let key = require_str(&params, "key")?;
        let force = bool_param(&params, "force", false);

        if key == "main" {
            return Err(Error::message("cannot delete the main session").into());
        }

        if self.metadata.get(key).await.is_none() {
            return Err(Error::message(format!("session not found: {key}")).into());
        }

        let req = DeleteSessionRequest {
            key: key.to_string(),
            force,
        };
        let result = (self.delete_fn)(req).await?;

        Ok(serde_json::json!({
            "key": key,
            "deleted": true,
            "result": result,
        }))
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };

    use super::*;

    type TestResult<T> = Result<T, Box<dyn std::error::Error + Send + Sync>>;

    async fn test_pool() -> TestResult<sqlx::SqlitePool> {
        let pool = sqlx::SqlitePool::connect(":memory:").await?;
        sqlx::query("CREATE TABLE IF NOT EXISTS projects (id TEXT PRIMARY KEY)")
            .execute(&pool)
            .await?;
        SqliteSessionMetadata::init(&pool).await?;
        Ok(pool)
    }

    #[tokio::test]
    async fn sessions_create_generates_standard_key() -> TestResult<()> {
        let called = Arc::new(AtomicBool::new(false));
        let called_ref = Arc::clone(&called);

        let create_fn: CreateSessionFn = Arc::new(move |req| {
            let called_ref = Arc::clone(&called_ref);
            Box::pin(async move {
                called_ref.store(true, Ordering::SeqCst);
                Ok(serde_json::json!({
                    "entry": { "key": req.key }
                }))
            })
        });

        let tool = SessionsCreateTool::new(create_fn);

        let result = tool
            .execute(serde_json::json!({
                "agent_id": "main",
                "label": "Worker session"
            }))
            .await?;

        let key = result
            .get("key")
            .and_then(Value::as_str)
            .ok_or_else(|| std::io::Error::other("missing key in create response"))?;
        assert!(key.starts_with("session:"));
        assert!(result.get("created").is_none());
        assert!(called.load(Ordering::SeqCst));
        Ok(())
    }

    #[tokio::test]
    async fn sessions_create_links_parent_from_session_context() -> TestResult<()> {
        let metadata = Arc::new(SqliteSessionMetadata::new(test_pool().await?));
        metadata
            .upsert("session:parent", Some("Parent".to_string()))
            .await?;

        let captured_parent = Arc::new(std::sync::Mutex::new(None::<Option<String>>));
        let captured_ref = Arc::clone(&captured_parent);
        let create_fn: CreateSessionFn = Arc::new(move |req| {
            let captured_ref = Arc::clone(&captured_ref);
            Box::pin(async move {
                *captured_ref.lock().unwrap_or_else(|e| e.into_inner()) =
                    Some(req.parent_session_key.clone());
                Ok(serde_json::json!({
                    "entry": { "key": req.key }
                }))
            })
        });

        let tool = SessionsCreateTool::new(create_fn);
        let result = tool
            .execute(serde_json::json!({
                "agent_id": "main",
                "label": "Child session",
                "_session_key": "session:parent"
            }))
            .await?;

        assert!(result.get("created").is_none());
        let parent = captured_parent
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
            .ok_or_else(|| std::io::Error::other("callback was not invoked"))?;
        assert_eq!(parent.as_deref(), Some("session:parent"));
        Ok(())
    }

    #[tokio::test]
    async fn sessions_create_rejects_input_key() -> TestResult<()> {
        let called = Arc::new(AtomicBool::new(false));
        let called_ref = Arc::clone(&called);

        let create_fn: CreateSessionFn = Arc::new(move |req| {
            let called_ref = Arc::clone(&called_ref);
            Box::pin(async move {
                called_ref.store(true, Ordering::SeqCst);
                Ok(serde_json::json!({
                    "entry": { "key": req.key }
                }))
            })
        });

        let tool = SessionsCreateTool::new(create_fn);
        let result = tool
            .execute(serde_json::json!({
                "agent_id": "main",
                "key": "session:custom"
            }))
            .await;

        let err = result
            .err()
            .ok_or_else(|| std::io::Error::other("expected key to fail"))?;
        assert!(err.to_string().contains("key is not supported"));
        assert!(!called.load(Ordering::SeqCst));
        Ok(())
    }

    #[tokio::test]
    async fn sessions_create_uses_generated_key_even_when_other_sessions_exist() -> TestResult<()> {
        let metadata = Arc::new(SqliteSessionMetadata::new(test_pool().await?));
        metadata
            .upsert("session:existing", Some("Existing".to_string()))
            .await?;

        let create_fn: CreateSessionFn = Arc::new(move |req| {
            Box::pin(async move {
                Ok(serde_json::json!({
                    "entry": { "key": req.key }
                }))
            })
        });

        let tool = SessionsCreateTool::new(create_fn);
        let result = tool
            .execute(serde_json::json!({
                "agent_id": "main",
                "_session_key": "session:caller"
            }))
            .await?;

        assert!(result.get("created").is_none());
        let key = result
            .get("key")
            .and_then(Value::as_str)
            .ok_or_else(|| std::io::Error::other("missing key in create response"))?;
        assert!(key.starts_with("session:"));
        assert_ne!(key, "session:existing");
        Ok(())
    }

    #[tokio::test]
    async fn sessions_explore_delegates_to_callback() -> TestResult<()> {
        let tool = SessionsExploreTool::new(Arc::new(|| {
            Box::pin(async move {
                Ok(serde_json::json!({
                    "agents": [{ "id": "main", "name": "Main" }]
                }))
            })
        }));

        let result = tool.execute(serde_json::json!({})).await?;

        assert_eq!(result["agents"][0]["id"], "main");
        Ok(())
    }

    #[tokio::test]
    async fn sessions_create_rejects_missing_agent_id() -> TestResult<()> {
        let create_fn: CreateSessionFn =
            Arc::new(move |_req| Box::pin(async move { Ok(serde_json::json!({ "ok": true })) }));
        let tool = SessionsCreateTool::new(create_fn);

        let result = tool.execute(serde_json::json!({})).await;

        let err = result
            .err()
            .ok_or_else(|| std::io::Error::other("expected missing agent_id to fail"))?;
        assert!(
            err.to_string()
                .contains("missing required parameter: agent_id")
        );
        Ok(())
    }

    #[tokio::test]
    async fn sessions_create_rejects_invalid_reasoning_effort() -> TestResult<()> {
        let create_fn: CreateSessionFn =
            Arc::new(move |_req| Box::pin(async move { Ok(serde_json::json!({ "ok": true })) }));
        let tool = SessionsCreateTool::new(create_fn);

        let result = tool
            .execute(serde_json::json!({
                "agent_id": "main",
                "model_override": {
                    "model": "anthropic::claude-opus-4-5-20251101",
                    "reasoning_effort": "ultra"
                }
            }))
            .await;

        let err = result
            .err()
            .ok_or_else(|| std::io::Error::other("expected invalid reasoning effort to fail"))?;
        assert!(err.to_string().contains("invalid reasoning_effort"));
        Ok(())
    }

    #[tokio::test]
    async fn sessions_create_ignores_null_parameters() -> TestResult<()> {
        let captured_override = Arc::new(std::sync::Mutex::new(None::<Option<ModelOverride>>));
        let captured_ref = Arc::clone(&captured_override);
        let create_fn: CreateSessionFn = Arc::new(move |req| {
            let captured_ref = Arc::clone(&captured_ref);
            Box::pin(async move {
                *captured_ref.lock().unwrap_or_else(|e| e.into_inner()) =
                    Some(req.model_override.clone());
                Ok(serde_json::json!({ "ok": true }))
            })
        });
        let tool = SessionsCreateTool::new(create_fn);

        let result = tool
            .execute(serde_json::json!({
                "agent_id": "main",
                "label": null,
                "project_id": null,
                "model_override": null
            }))
            .await?;

        assert!(result.get("created").is_none());
        let model_override = captured_override
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
            .ok_or_else(|| std::io::Error::other("callback was not invoked"))?;
        assert!(model_override.is_none());
        Ok(())
    }

    #[tokio::test]
    async fn sessions_create_rejects_inherit_agent_from() -> TestResult<()> {
        let create_fn: CreateSessionFn =
            Arc::new(move |_req| Box::pin(async move { Ok(serde_json::json!({ "ok": true })) }));
        let tool = SessionsCreateTool::new(create_fn);

        let result = tool
            .execute(serde_json::json!({
                "agent_id": "main",
                "inherit_agent_from": "main"
            }))
            .await;

        let err = result
            .err()
            .ok_or_else(|| std::io::Error::other("expected inherit_agent_from to fail"))?;
        assert!(
            err.to_string()
                .contains("inherit_agent_from is not supported")
        );
        Ok(())
    }

    #[tokio::test]
    async fn sessions_delete_deletes_existing_session() -> TestResult<()> {
        let metadata = Arc::new(SqliteSessionMetadata::new(test_pool().await?));
        metadata
            .upsert("session:to-delete", Some("Delete me".to_string()))
            .await?;

        let called = Arc::new(AtomicBool::new(false));
        let called_ref = Arc::clone(&called);
        let delete_fn: DeleteSessionFn = Arc::new(move |req| {
            let called_ref = Arc::clone(&called_ref);
            Box::pin(async move {
                assert_eq!(req.key, "session:to-delete");
                assert!(req.force);
                called_ref.store(true, Ordering::SeqCst);
                Ok(serde_json::json!({ "ok": true }))
            })
        });

        let tool = SessionsDeleteTool::new(metadata, delete_fn);
        let result = tool
            .execute(serde_json::json!({
                "key": "session:to-delete",
                "force": true
            }))
            .await?;

        assert_eq!(result["deleted"], true);
        assert!(called.load(Ordering::SeqCst));
        Ok(())
    }

    #[tokio::test]
    async fn sessions_delete_rejects_missing_session() -> TestResult<()> {
        let metadata = Arc::new(SqliteSessionMetadata::new(test_pool().await?));
        let delete_fn: DeleteSessionFn =
            Arc::new(move |_req| Box::pin(async move { Ok(serde_json::json!({ "ok": true })) }));

        let tool = SessionsDeleteTool::new(metadata, delete_fn);
        let result = tool
            .execute(serde_json::json!({
                "key": "session:missing"
            }))
            .await;

        let err = result
            .err()
            .ok_or_else(|| std::io::Error::other("expected missing-session delete to fail"))?;
        assert!(err.to_string().contains("session not found"));
        Ok(())
    }

    #[tokio::test]
    async fn sessions_delete_rejects_main_session() -> TestResult<()> {
        let metadata = Arc::new(SqliteSessionMetadata::new(test_pool().await?));
        metadata.upsert("main", Some("Main".to_string())).await?;

        let delete_fn: DeleteSessionFn =
            Arc::new(move |_req| Box::pin(async move { Ok(serde_json::json!({ "ok": true })) }));

        let tool = SessionsDeleteTool::new(metadata, delete_fn);
        let result = tool
            .execute(serde_json::json!({
                "key": "main"
            }))
            .await;

        let err = result
            .err()
            .ok_or_else(|| std::io::Error::other("expected main-session delete to fail"))?;
        assert!(err.to_string().contains("cannot delete the main session"));
        Ok(())
    }
}
