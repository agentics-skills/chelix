//! Session management tools for creating and deleting chat sessions.
//!
//! These tools expose explicit session lifecycle operations to the agent:
//! - `sessions_explore`: list available agents for session creation
//! - `sessions_create`: create a generated session key
//! - `sessions_delete`: delete a session and its history

use std::sync::Arc;

use {
    async_trait::async_trait,
    futures::future::BoxFuture,
    serde::{Deserialize, Deserializer, de::Error as _},
    serde_json::Value,
};

use {
    chelix_agents::{tool_context::ToolExecutionContext, tool_registry::AgentTool},
    chelix_sessions::metadata::SqliteSessionMetadata,
};

use crate::{
    Error,
    params::{bool_param, require_str},
    session_model_override::{ModelOverride, deserialize_model_override},
    session_tool_params::{nonempty_string, optional_nonempty_string},
};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SessionsCreateParams {
    #[serde(deserialize_with = "nonempty_string")]
    agent_id: String,
    #[serde(default, deserialize_with = "optional_nonempty_string")]
    label: Option<String>,
    #[serde(default, deserialize_with = "optional_nonempty_string")]
    project_id: Option<String>,
    model: SessionsCreateModel,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum SessionsCreateModel {
    Agent(SessionsCreateAgentForm),
    Override(SessionsCreateOverrideForm),
}

impl SessionsCreateModel {
    fn into_override(self) -> Option<ModelOverride> {
        match self {
            Self::Agent(SessionsCreateAgentForm {
                agent: SessionsCreateAgentBody {},
            }) => None,
            Self::Override(form) => Some(form.model_override),
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SessionsCreateAgentForm {
    agent: SessionsCreateAgentBody,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SessionsCreateAgentBody {}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SessionsCreateOverrideForm {
    #[serde(
        rename = "override",
        deserialize_with = "deserialize_present_model_override"
    )]
    model_override: ModelOverride,
}

fn deserialize_present_model_override<'de, D>(deserializer: D) -> Result<ModelOverride, D::Error>
where
    D: Deserializer<'de>,
{
    deserialize_model_override(deserializer)?
        .ok_or_else(|| D::Error::custom("model.override requires model and reasoning_effort"))
}

fn require_json_object(value: &Value, path: &str) -> Result<(), serde_json::Error> {
    if value.is_object() {
        Ok(())
    } else {
        Err(serde_json::Error::custom(format!(
            "{path} must be an object"
        )))
    }
}

fn reject_non_object_model_payloads(params: &Value) -> Result<(), serde_json::Error> {
    let Some(model) = params.get("model") else {
        return Ok(());
    };
    require_json_object(model, "model")?;
    if let Some(agent) = model.get("agent") {
        require_json_object(agent, "model.agent")?;
    }
    if let Some(override_value) = model.get("override") {
        require_json_object(override_value, "model.override")?;
    }
    Ok(())
}

fn parse_sessions_create_params(params: Value) -> Result<SessionsCreateParams, serde_json::Error> {
    reject_non_object_model_payloads(&params)?;
    serde_json::from_value(params)
}

/// Request payload for session creation.
#[derive(Debug, Clone)]
pub struct CreateSessionRequest {
    pub key: String,
    pub agent_id: String,
    pub label: Option<String>,
    pub model_override: Option<ModelOverride>,
    pub project_id: Option<String>,
    /// Direct parent session. Drives the parent/child tree in the
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

    async fn create(
        &self,
        params: SessionsCreateParams,
        parent_session_key: Option<String>,
    ) -> anyhow::Result<Value> {
        let key = format!("session:{}", uuid::Uuid::new_v4());
        let agent_id = params.agent_id.clone();
        let request = CreateSessionRequest {
            key: key.clone(),
            agent_id: params.agent_id,
            label: params.label,
            model_override: params.model.into_override(),
            project_id: params.project_id,
            parent_session_key,
        };
        let result = (self.create_fn)(request).await?;
        Ok(serde_json::json!({
            "key": key,
            "agent_id": agent_id,
            "agentId": agent_id,
            "result": result,
        }))
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
            "properties": {},
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
         The model parameter is required; use agent to keep the selected agent's preset model, and provide override only for an intentional advanced override."
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
                "model": {
                    "description": "Required model form. Use agent to keep the selected agent's preset model/reasoning pair. Use override only for an intentional complete model/reasoning pair. Do not copy preset model values returned by sessions_explore.",
                    "oneOf": [
                        {
                            "type": "object",
                            "additionalProperties": false,
                            "required": ["agent"],
                            "properties": {
                                "agent": {
                                    "type": "object",
                                    "description": "Empty object. Uses the selected agent's preset model/reasoning pair.",
                                    "additionalProperties": false,
                                    "properties": {},
                                    "required": []
                                }
                            }
                        },
                        {
                            "type": "object",
                            "additionalProperties": false,
                            "required": ["override"],
                            "properties": {
                                "override": {
                                    "description": "Complete model/reasoning override for this session. Provide this only when intentionally overriding the agent's preset with a different model configuration. Do not copy preset model values returned by sessions_explore.",
                                    "type": "object",
                                    "additionalProperties": false,
                                    "required": ["model", "reasoning_effort"],
                                    "properties": {
                                        "model": {
                                            "description": "Base model id override from the chat model registry. Must be different from the selected agent's preset model. Do not pass null or empty strings.",
                                            "minLength": 1,
                                            "type": "string"
                                        },
                                        "reasoning_effort": {
                                            "description": "Exact reasoning effort advertised by the selected model's reasoning_supported_efforts metadata. Do not pass null or empty strings.",
                                            "minLength": 1,
                                            "type": "string"
                                        }
                                    }
                                }
                            }
                        }
                    ]
                }
            },
            "required": ["agent_id", "model"]
        })
    }

    fn validate(&self, params: &Value) -> anyhow::Result<()> {
        parse_sessions_create_params(params.clone())?;
        Ok(())
    }

    async fn execute(&self, params: Value) -> anyhow::Result<Value> {
        self.create(parse_sessions_create_params(params)?, None)
            .await
    }

    async fn execute_with_context(
        &self,
        params: Value,
        context: &ToolExecutionContext,
    ) -> anyhow::Result<Value> {
        let params = parse_sessions_create_params(params)?;
        let parent = context.session_key().map(|key| key.as_str().to_owned());
        self.create(params, parent).await
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

        if self.metadata.get(key).await?.is_none() {
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

    async fn create_test_session(
        metadata: &SqliteSessionMetadata,
        key: &str,
        label: &str,
    ) -> TestResult<()> {
        let model_reasoning = chelix_common::ResolvedModelReasoning::try_new(
            "test::model".to_string(),
            chelix_common::ReasoningEffort::from("off"),
        )?;
        metadata
            .create_llm_session(key, Some(label), &model_reasoning, Some("main"))
            .await?;
        Ok(())
    }

    #[tokio::test]
    async fn sessions_create_generates_standard_key() -> TestResult<()> {
        let called = Arc::new(AtomicBool::new(false));
        let called_ref = Arc::clone(&called);

        let create_fn: CreateSessionFn = Arc::new(move |req| {
            let called_ref = Arc::clone(&called_ref);
            Box::pin(async move {
                called_ref.store(true, Ordering::SeqCst);
                assert!(req.model_override.is_none());
                Ok(serde_json::json!({
                    "entry": { "key": req.key }
                }))
            })
        });

        let tool = SessionsCreateTool::new(create_fn);

        let result = tool
            .execute(serde_json::json!({
                "agent_id": "main",
                "label": "Worker session",
                "model": { "agent": {} }
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
        create_test_session(&metadata, "session:parent", "Parent").await?;

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
            .execute_with_context(
                serde_json::json!({
                    "agent_id": "main",
                    "label": "Child session",
                    "model": { "agent": {} },
                }),
                &ToolExecutionContext::for_session(chelix_sessions::SessionKey::new(
                    "session:parent",
                )),
            )
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
    async fn sessions_create_uses_generated_key_even_when_other_sessions_exist() -> TestResult<()> {
        let metadata = Arc::new(SqliteSessionMetadata::new(test_pool().await?));
        create_test_session(&metadata, "session:existing", "Existing").await?;

        let create_fn: CreateSessionFn = Arc::new(move |req| {
            Box::pin(async move {
                Ok(serde_json::json!({
                    "entry": { "key": req.key }
                }))
            })
        });

        let tool = SessionsCreateTool::new(create_fn);
        let result = tool
            .execute_with_context(
                serde_json::json!({ "agent_id": "main", "model": { "agent": {} } }),
                &ToolExecutionContext::for_session(chelix_sessions::SessionKey::new(
                    "session:caller",
                )),
            )
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
    async fn sessions_create_rejects_invalid_parameters_before_callback() {
        let tool = SessionsCreateTool::new(Arc::new(|_| {
            Box::pin(async { panic!("invalid parameters must not reach the callback") })
        }));
        for params in [
            serde_json::json!({}),
            serde_json::json!({"agent_id": "", "model": {"agent": {}}}),
            serde_json::json!({"agent_id": 1, "model": {"agent": {}}}),
            serde_json::json!({"agent_id": "main"}),
            serde_json::json!({"agent_id": "main", "label": "", "model": {"agent": {}}}),
            serde_json::json!({"agent_id": "main", "label": null, "model": {"agent": {}}}),
            serde_json::json!({"agent_id": "main", "project_id": null, "model": {"agent": {}}}),
            serde_json::json!({"agent_id": "main", "model_override": {"model": "test::model", "reasoning_effort": "low"}}),
            serde_json::json!({"agent_id": "main", "model": null}),
            serde_json::json!({"agent_id": "main", "model": []}),
            serde_json::json!({"agent_id": "main", "model": {}}),
            serde_json::json!({"agent_id": "main", "model": {"agent": {}, "override": {"model": "test::model", "reasoning_effort": "low"}}}),
            serde_json::json!({"agent_id": "main", "model": {"agent": []}}),
            serde_json::json!({"agent_id": "main", "model": {"agent": null}}),
            serde_json::json!({"agent_id": "main", "model": {"agent": {"extra_field": true}}}),
            serde_json::json!({"agent_id": "main", "model": {"override": null}}),
            serde_json::json!({"agent_id": "main", "model": {"override": []}}),
            serde_json::json!({"agent_id": "main", "model": {"override": {"reasoning_effort": "low"}}}),
            serde_json::json!({"agent_id": "main", "model": {"override": {"model": "", "reasoning_effort": "low"}}}),
            serde_json::json!({"agent_id": "main", "model": {"override": {"model": "test::model", "reasoning_effort": ""}}}),
            serde_json::json!({"agent_id": "main", "model": {"override": {"model": "test::model", "reasoning_effort": "low", "extra_field": true}}}),
            serde_json::json!({"agent_id": "main", "model": {"extra_field": true}}),
        ] {
            assert!(tool.validate(&params).is_err(), "{params}");
            assert!(tool.execute(params).await.is_err());
        }
    }

    #[tokio::test]
    async fn sessions_create_rejects_additional_field() -> TestResult<()> {
        let tool = SessionsCreateTool::new(Arc::new(|_| {
            Box::pin(async { panic!("additional fields must not reach the callback") })
        }));
        let schema = tool.parameters_schema();
        assert_eq!(schema["additionalProperties"], false);
        assert!(schema.get("oneOf").is_none());
        assert_eq!(schema["required"], serde_json::json!(["agent_id", "model"]));
        assert!(schema["properties"].get("model_override").is_none());
        let variants = schema["properties"]["model"]["oneOf"]
            .as_array()
            .ok_or_else(|| std::io::Error::other("model.oneOf must be an array"))?;
        assert_eq!(variants.len(), 2);
        assert_eq!(variants[0]["required"], serde_json::json!(["agent"]));
        assert!(
            variants[0]["properties"]["agent"]["properties"]
                .get("model")
                .is_none()
        );
        assert!(
            variants[0]["properties"]["agent"]["properties"]
                .get("reasoning_effort")
                .is_none()
        );
        assert_eq!(variants[1]["required"], serde_json::json!(["override"]));
        assert_eq!(
            variants[1]["properties"]["override"]["required"],
            serde_json::json!(["model", "reasoning_effort"])
        );
        for params in [
            serde_json::json!({"agent_id": "main", "model": { "agent": {} }, "extra_field": true}),
            serde_json::json!({"agent_id": "main", "model": {
                "override": {
                    "model": "test::model", "reasoning_effort": "low", "extra_field": true,
                }
            }}),
        ] {
            assert!(tool.validate(&params).is_err());
            assert!(tool.execute(params).await.is_err());
        }
        Ok(())
    }

    #[tokio::test]
    async fn sessions_create_preserves_provider_defined_reasoning_effort() -> TestResult<()> {
        let captured_override = Arc::new(std::sync::Mutex::new(None::<ModelOverride>));
        let captured_ref = Arc::clone(&captured_override);
        let create_fn: CreateSessionFn = Arc::new(move |req| {
            let captured_ref = Arc::clone(&captured_ref);
            Box::pin(async move {
                *captured_ref.lock().unwrap_or_else(|e| e.into_inner()) = req.model_override;
                Ok(serde_json::json!({ "ok": true }))
            })
        });
        let tool = SessionsCreateTool::new(create_fn);

        tool.execute(serde_json::json!({
            "agent_id": "main",
            "model": {
                "override": {
                    "model": "openai::gpt-5.2",
                    "reasoning_effort": "ultra"
                }
            }
        }))
        .await?;

        let model_override = captured_override
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
            .ok_or_else(|| std::io::Error::other("callback did not receive model_override"))?;
        assert_eq!(model_override.model, "openai::gpt-5.2");
        assert_eq!(model_override.reasoning_effort.as_str(), "ultra");
        Ok(())
    }

    #[tokio::test]
    async fn sessions_delete_deletes_existing_session() -> TestResult<()> {
        let metadata = Arc::new(SqliteSessionMetadata::new(test_pool().await?));
        create_test_session(&metadata, "session:to-delete", "Delete me").await?;

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
        create_test_session(&metadata, "main", "Main").await?;

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
