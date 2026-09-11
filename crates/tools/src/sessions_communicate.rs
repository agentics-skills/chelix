//! Session communication tools for listing, inspecting, and messaging sessions.
//!
//! These tools expose cross-session coordination primitives:
//! - `sessions_list`: list sessions with optional filtering
//! - `sessions_history`: read paginated history from a session
//! - `sessions_search`: search past session history for relevant snippets
//! - `sessions_send`: send a message to another session (async or sync)

use std::{collections::HashMap, sync::Arc};

use {
    async_trait::async_trait,
    futures::future::BoxFuture,
    serde::{Deserialize, Deserializer, de::Error as _},
    serde_json::Value,
};

use {
    chelix_agents::{tool_context::ToolExecutionContext, tool_registry::AgentTool},
    chelix_sessions::{metadata::SqliteSessionMetadata, store::SessionStore},
};

use crate::{
    Error,
    params::{require_str, str_param, u64_param},
    session_model_override::{ModelOverride, deserialize_model_override},
    session_tool_params::{nonempty_string, optional_nonempty_string},
};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SessionsSendParams {
    #[serde(deserialize_with = "nonempty_string")]
    key: String,
    #[serde(deserialize_with = "nonempty_string")]
    message: String,
    #[serde(default)]
    wait_for_reply: bool,
    #[serde(default, deserialize_with = "optional_nonempty_string")]
    context: Option<String>,
    model: SessionsSendModel,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum SessionsSendModel {
    Session(SessionsSendSessionForm),
    Override(SessionsSendOverrideForm),
}

impl SessionsSendModel {
    fn into_override(self) -> Option<ModelOverride> {
        match self {
            Self::Session(SessionsSendSessionForm {
                session: SessionsSendSessionBody {},
            }) => None,
            Self::Override(form) => Some(form.model_override),
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SessionsSendSessionForm {
    session: SessionsSendSessionBody,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SessionsSendSessionBody {}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SessionsSendOverrideForm {
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
    if let Some(session) = model.get("session") {
        require_json_object(session, "model.session")?;
    }
    if let Some(override_value) = model.get("override") {
        require_json_object(override_value, "model.override")?;
    }
    Ok(())
}

fn parse_sessions_send_params(params: Value) -> Result<SessionsSendParams, serde_json::Error> {
    reject_non_object_model_payloads(&params)?;
    serde_json::from_value(params)
}

/// Format the sender identity badge placed at the top of cross-session text.
///
/// The badge uses the sender agent display name and is followed by a blank line.
#[must_use]
pub fn sender_badge(name: &str) -> String {
    format!("[From the \"{name}\" agent]\n\n")
}

/// Request payload for cross-session message delivery.
#[derive(Debug, Clone)]
pub struct SendToSessionRequest {
    pub key: String,
    pub message: String,
    pub wait_for_reply: bool,
    pub model_override: Option<ModelOverride>,
}

/// Callback used by `sessions_send`.
pub type SendToSessionFn =
    Arc<dyn Fn(SendToSessionRequest) -> BoxFuture<'static, crate::Result<Value>> + Send + Sync>;

/// Policy controlling which sessions an agent can access.
#[derive(Debug, Clone, Default)]
pub struct SessionAccessPolicy {
    /// If set, only sessions with keys matching this prefix are visible.
    pub key_prefix: Option<String>,
    /// Explicit list of session keys this agent can access (in addition to prefix).
    pub allowed_keys: Vec<String>,
    /// If true, agent can send messages to other sessions.
    pub can_send: bool,
    /// If true, agent can access sessions from other agents.
    pub cross_agent: bool,
}

impl SessionAccessPolicy {
    /// Check if a session key is accessible under this policy.
    pub fn can_access(&self, key: &str) -> bool {
        // Check explicit allowed keys first.
        if self.allowed_keys.iter().any(|k| k == key) {
            return true;
        }

        // Check prefix match.
        if let Some(ref prefix) = self.key_prefix {
            return key.starts_with(prefix);
        }

        // Default: allow all if no restrictions.
        true
    }
}

impl From<&chelix_config::SessionAccessPolicyConfig> for SessionAccessPolicy {
    fn from(config: &chelix_config::SessionAccessPolicyConfig) -> Self {
        Self {
            key_prefix: config.key_prefix.clone(),
            allowed_keys: config.allowed_keys.clone(),
            can_send: config.can_send,
            cross_agent: config.cross_agent,
        }
    }
}

/// Tool for listing known sessions.
pub struct SessionsListTool {
    metadata: Arc<SqliteSessionMetadata>,
    policy: Option<SessionAccessPolicy>,
}

impl SessionsListTool {
    pub fn new(metadata: Arc<SqliteSessionMetadata>) -> Self {
        Self {
            metadata,
            policy: None,
        }
    }

    /// Attach a session access policy for filtering.
    pub fn with_policy(mut self, policy: SessionAccessPolicy) -> Self {
        self.policy = Some(policy);
        self
    }
}

/// Tool for reading history from a target session.
pub struct SessionsHistoryTool {
    store: Arc<SessionStore>,
    metadata: Arc<SqliteSessionMetadata>,
    policy: Option<SessionAccessPolicy>,
}

impl SessionsHistoryTool {
    pub fn new(store: Arc<SessionStore>, metadata: Arc<SqliteSessionMetadata>) -> Self {
        Self {
            store,
            metadata,
            policy: None,
        }
    }

    /// Attach a session access policy for filtering.
    pub fn with_policy(mut self, policy: SessionAccessPolicy) -> Self {
        self.policy = Some(policy);
        self
    }
}

/// Tool for searching across session history.
pub struct SessionsSearchTool {
    store: Arc<SessionStore>,
    metadata: Arc<SqliteSessionMetadata>,
    policy: Option<SessionAccessPolicy>,
}

impl SessionsSearchTool {
    pub fn new(store: Arc<SessionStore>, metadata: Arc<SqliteSessionMetadata>) -> Self {
        Self {
            store,
            metadata,
            policy: None,
        }
    }

    /// Attach a session access policy for filtering.
    pub fn with_policy(mut self, policy: SessionAccessPolicy) -> Self {
        self.policy = Some(policy);
        self
    }
}

/// Tool for sending a message to another session.
pub struct SessionsSendTool {
    metadata: Arc<SqliteSessionMetadata>,
    send_fn: SendToSessionFn,
    policy: Option<SessionAccessPolicy>,
    agents_config: Option<Arc<tokio::sync::RwLock<chelix_config::AgentsConfig>>>,
}

impl SessionsSendTool {
    pub fn new(metadata: Arc<SqliteSessionMetadata>, send_fn: SendToSessionFn) -> Self {
        Self {
            metadata,
            send_fn,
            policy: None,
            agents_config: None,
        }
    }

    /// Attach a session access policy for filtering.
    pub fn with_policy(mut self, policy: SessionAccessPolicy) -> Self {
        self.policy = Some(policy);
        self
    }

    /// Attach the agent registry used to resolve the trusted sender badge.
    pub fn with_agents_config(
        mut self,
        agents_config: Arc<tokio::sync::RwLock<chelix_config::AgentsConfig>>,
    ) -> Self {
        self.agents_config = Some(agents_config);
        self
    }
}

#[async_trait]
impl AgentTool for SessionsListTool {
    fn name(&self) -> &str {
        "sessions_list"
    }

    fn description(&self) -> &str {
        "List available sessions with metadata. Supports optional text filtering and limit."
    }

    fn parameters_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "filter": {
                    "type": "string",
                    "description": "Optional substring to match against session key or label."
                },
                "limit": {
                    "type": "integer",
                    "description": "Maximum sessions returned (default: 20, max: 100)."
                }
            }
        })
    }

    async fn execute(&self, params: Value) -> anyhow::Result<Value> {
        let filter = str_param(&params, "filter").map(|v| v.to_lowercase());
        let limit = u64_param(&params, "limit", 20).min(100) as usize;

        let mut sessions: Vec<Value> = self
            .metadata
            .list()
            .await?
            .into_iter()
            .filter(|entry| {
                // Apply session access policy.
                if let Some(ref policy) = self.policy
                    && !policy.can_access(&entry.key)
                {
                    return false;
                }
                filter.as_ref().is_none_or(|needle| {
                    let key_match = entry.key.to_lowercase().contains(needle);
                    let label_match = entry
                        .label
                        .as_ref()
                        .map(|label| label.to_lowercase().contains(needle))
                        .unwrap_or(false);
                    key_match || label_match
                })
            })
            .take(limit)
            .map(|entry| {
                serde_json::json!({
                    "id": entry.id,
                    "key": entry.key,
                    "label": entry.label,
                    "model": entry.model(),
                    "reasoningEffort": entry.reasoning_effort().map(|effort| effort.as_str()),
                    "messageCount": entry.message_count,
                    "createdAt": entry.created_at,
                    "updatedAt": entry.updated_at,
                    "projectId": entry.project_id,
                    "agentId": entry.agent_id,
                    "version": entry.version,
                })
            })
            .collect();
        let count = sessions.len();
        sessions.shrink_to_fit();

        Ok(serde_json::json!({
            "sessions": sessions,
            "count": count,
        }))
    }
}

#[async_trait]
impl AgentTool for SessionsSearchTool {
    fn name(&self) -> &str {
        "sessions_search"
    }

    fn description(&self) -> &str {
        "Search past session history for relevant snippets across sessions."
    }

    fn parameters_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "Search query to match against prior session messages."
                },
                "limit": {
                    "type": "integer",
                    "description": "Maximum results returned (default: 5, max: 20)."
                },
                "exclude_current": {
                    "type": "boolean",
                    "description": "Exclude the current session from results when `_session_key` is available. Defaults to true."
                }
            },
            "required": ["query"]
        })
    }

    async fn execute(&self, params: Value) -> anyhow::Result<Value> {
        let query = require_str(&params, "query")?;
        let limit = u64_param(&params, "limit", 5).min(20) as usize;
        let exclude_current = params
            .get("exclude_current")
            .or_else(|| params.get("excludeCurrent"))
            .and_then(Value::as_bool)
            .unwrap_or(true);
        let current_session_key = if exclude_current {
            str_param(&params, "_session_key")
        } else {
            None
        };

        let entries: HashMap<String, chelix_sessions::metadata::SessionEntry> = self
            .metadata
            .list()
            .await?
            .into_iter()
            .map(|entry| (entry.key.clone(), entry))
            .collect();

        let keys = entries
            .values()
            .filter(|entry| {
                current_session_key != Some(entry.key.as_str())
                    && self
                        .policy
                        .as_ref()
                        .is_none_or(|policy| policy.can_access(&entry.key))
            })
            .map(|entry| entry.key.clone())
            .collect::<Vec<_>>();
        let hits = self
            .store
            .search(&keys, query, limit)
            .await
            .map_err(|error| {
                Error::message(format!("failed to search sessions for '{query}': {error}"))
            })?;
        let mut results = Vec::with_capacity(limit);
        for hit in hits {
            if results.len() >= limit {
                break;
            }

            if current_session_key == Some(hit.session_key.as_str()) {
                continue;
            }

            if let Some(ref policy) = self.policy
                && !policy.can_access(&hit.session_key)
            {
                continue;
            }

            let entry = entries.get(&hit.session_key);
            results.push(serde_json::json!({
                "key": hit.session_key,
                "label": entry.and_then(|value| value.label.clone()),
                "model": entry.and_then(|value| value.model().map(str::to_string)),
                "projectId": entry.and_then(|value| value.project_id.clone()),
                "agentId": entry.and_then(|value| value.agent_id.clone()),
                "createdAt": entry.map(|value| value.created_at),
                "updatedAt": entry.map(|value| value.updated_at),
                "messageCount": entry.map(|value| value.message_count),
                "snippet": hit.snippet,
                "role": hit.role,
                "messageId": hit.message_id,
                "generation": hit.generation,
                "position": hit.position,
            }));
        }

        Ok(serde_json::json!({
            "query": query,
            "count": results.len(),
            "results": results,
        }))
    }
}

#[async_trait]
impl AgentTool for SessionsHistoryTool {
    fn name(&self) -> &str {
        "sessions_history"
    }

    fn description(&self) -> &str {
        "Read paginated message history from another session."
    }

    fn parameters_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "key": {
                    "type": "string",
                    "description": "Session key to read."
                },
                "limit": {
                    "type": "integer",
                    "description": "Maximum messages to return (default: 20, max: 100)."
                },
                "offset": {
                    "type": "integer",
                    "description": "Skip this many newest messages (default: 0)."
                }
            },
            "required": ["key"]
        })
    }

    async fn execute(&self, params: Value) -> anyhow::Result<Value> {
        let key = require_str(&params, "key")?;

        // Enforce session access policy.
        if let Some(ref policy) = self.policy
            && !policy.can_access(key)
        {
            return Err(Error::message(format!("session access denied: {key}")).into());
        }

        let limit = u64_param(&params, "limit", 20).min(100) as usize;
        let offset = u64_param(&params, "offset", 0) as usize;

        let entry = self
            .metadata
            .get(key)
            .await?
            .ok_or_else(|| Error::message(format!("session not found: {key}")))?;
        let page = self
            .store
            .ui_history
            .page(
                key,
                chelix_sessions::ui_history_types::UiHistoryRange::Latest,
                limit.saturating_add(offset).max(1),
            )
            .await
            .map_err(|error| Error::message(format!("failed to read session '{key}': {error}")))?;
        let total = page.total_messages as usize;
        let end = page.history.len().saturating_sub(offset);
        let start = end.saturating_sub(limit);
        let messages = page.history[start..end]
            .iter()
            .map(|snapshot| snapshot.public_value())
            .collect::<chelix_sessions::Result<Vec<_>>>()?;

        Ok(serde_json::json!({
            "key": key,
            "label": entry.label,
            "messages": messages,
            "totalMessages": total,
            "offset": offset,
            "count": end.saturating_sub(start),
            "hasMore": total > offset.saturating_add(messages.len()),
            "generation": page.generation,
            "revision": page.revision,
        }))
    }
}

#[async_trait]
impl AgentTool for SessionsSendTool {
    fn name(&self) -> &str {
        "sessions_send"
    }

    fn description(&self) -> &str {
        "Send a message to another session. Optionally wait for the target session's reply."
    }

    fn parameters_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "additionalProperties": false,
            "properties": {
                "key": {
                    "type": "string",
                    "minLength": 1,
                    "description": "Session key to send to."
                },
                "message": {
                    "type": "string",
                    "minLength": 1,
                    "description": "Message text to send."
                },
                "wait_for_reply": {
                    "type": "boolean",
                    "description": "Wait for a synchronous response from the target session."
                },
                "context": {
                    "type": "string",
                    "minLength": 1,
                    "description": "Optional sender context prepended to the message."
                },
                "model": {
                    "description": "Required model form. Use session to keep the target session's persisted model/reasoning pair. Use override only for an intentional complete model/reasoning pair. Do not copy preset model values returned by sessions_explore.",
                    "oneOf": [
                        {
                            "type": "object",
                            "additionalProperties": false,
                            "required": ["session"],
                            "properties": {
                                "session": {
                                    "type": "object",
                                    "description": "Empty object. Uses the target session's persisted model/reasoning pair.",
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
                                    "description": "Complete model/reasoning override for this send. Provide this only when intentionally overriding the target session's persisted pair. Do not copy preset model values returned by sessions_explore.",
                                    "type": "object",
                                    "additionalProperties": false,
                                    "required": ["model", "reasoning_effort"],
                                    "properties": {
                                        "model": {
                                            "description": "Base model id override from the chat model registry. Must be different from the target session's persisted model. Do not pass null or empty strings.",
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
            "required": ["key", "message", "model"]
        })
    }

    fn validate(&self, params: &Value) -> anyhow::Result<()> {
        parse_sessions_send_params(params.clone())?;
        Ok(())
    }

    async fn execute_with_context(
        &self,
        params: Value,
        context: &ToolExecutionContext,
    ) -> anyhow::Result<Value> {
        let sender = context.sender_agent_id().map(str::to_string);
        self.execute_inner(params, sender).await
    }

    async fn execute(&self, params: Value) -> anyhow::Result<Value> {
        self.execute_inner(params, None).await
    }
}

impl SessionsSendTool {
    async fn sender_badge_prefix(&self, sender_agent_id: Option<String>) -> anyhow::Result<String> {
        let Some(sender_agent_id) = sender_agent_id else {
            return Ok(String::new());
        };
        let Some(ref agents_config) = self.agents_config else {
            return Ok(String::new());
        };
        let guard = agents_config.read().await;
        let agent = guard
            .get(&sender_agent_id)
            .ok_or_else(|| Error::message(format!("unknown sender agent '{sender_agent_id}'")))?;
        if agent.prepend_sender_badge {
            Ok(sender_badge(&agent.name))
        } else {
            Ok(String::new())
        }
    }

    async fn execute_inner(
        &self,
        params: Value,
        sender_agent_id: Option<String>,
    ) -> anyhow::Result<Value> {
        let SessionsSendParams {
            key,
            message,
            wait_for_reply,
            context,
            model,
        } = parse_sessions_send_params(params)?;
        let model_override = model.into_override();

        // Enforce session access policy.
        if let Some(ref policy) = self.policy {
            if !policy.can_access(&key) {
                return Err(Error::message(format!("session access denied: {key}")).into());
            }
            if !policy.can_send {
                return Err(
                    Error::message("session policy denies sending messages".to_string()).into(),
                );
            }
        }

        let entry = self
            .metadata
            .get(&key)
            .await?
            .ok_or_else(|| Error::message(format!("session not found: {key}")))?;

        let badge = self.sender_badge_prefix(sender_agent_id).await?;
        let message = if let Some(ctx) = context {
            format!("{badge}[From: {ctx}]\n\n{message}")
        } else if badge.is_empty() {
            message
        } else {
            format!("{badge}{message}")
        };

        let result = (self.send_fn)(SendToSessionRequest {
            key: key.clone(),
            message,
            wait_for_reply,
            model_override,
        })
        .await?;

        Ok(serde_json::json!({
            "key": key,
            "label": entry.label,
            "sent": true,
            "waitForReply": wait_for_reply,
            "result": result,
        }))
    }
}

#[allow(clippy::unwrap_used, clippy::expect_used)]
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
    async fn sessions_list_filters_and_limits() -> TestResult<()> {
        let metadata = Arc::new(SqliteSessionMetadata::new(test_pool().await?));
        create_test_session(&metadata, "main", "Main").await?;
        create_test_session(&metadata, "session:alpha", "Alpha").await?;
        create_test_session(&metadata, "session:beta", "Beta").await?;

        let tool = SessionsListTool::new(metadata);
        let result = tool
            .execute(serde_json::json!({
                "filter": "alp",
                "limit": 5
            }))
            .await?;

        assert_eq!(result["count"], 1);
        let sessions = result
            .get("sessions")
            .and_then(Value::as_array)
            .ok_or_else(|| std::io::Error::other("missing sessions array"))?;
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0]["key"], "session:alpha");
        Ok(())
    }

    #[tokio::test]
    async fn sessions_history_reads_paginated_messages() -> TestResult<()> {
        let metadata = Arc::new(SqliteSessionMetadata::new(test_pool().await?));
        create_test_session(&metadata, "session:history", "History").await?;

        let tmp = tempfile::tempdir()?;
        let store = Arc::new(SessionStore::new(tmp.path().to_path_buf()));
        store
            .append(
                "session:history",
                &serde_json::json!({
                    "role": "user",
                    "content": "one"
                }),
            )
            .await?;
        store
            .append(
                "session:history",
                &serde_json::json!({
                    "role": "assistant",
                    "content": "two",
                    "model": "provider::model",
                    "reasoning": "Visible reasoning",
                    "inputTokens": 100,
                    "llmApiResponse": [{"type": "response.output_text.delta", "delta": "tw"}]
                }),
            )
            .await?;
        store
            .append(
                "session:history",
                &serde_json::json!({
                    "role": "user",
                    "content": "three"
                }),
            )
            .await?;

        let tool = SessionsHistoryTool::new(store, metadata);
        let result = tool
            .execute(serde_json::json!({
                "key": "session:history",
                "limit": 2
            }))
            .await?;

        assert_eq!(result["totalMessages"], 3);
        assert_eq!(result["count"], 2);
        let messages = result
            .get("messages")
            .and_then(Value::as_array)
            .ok_or_else(|| std::io::Error::other("missing messages array"))?;
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0]["content"], "two");
        assert_eq!(messages[0]["model"], "provider::model");
        assert_eq!(messages[0]["reasoning"], "Visible reasoning");
        assert_eq!(messages[0]["inputTokens"], 100);
        assert!(messages[0].get("llmApiResponse").is_none());
        assert_eq!(messages[1]["content"], "three");
        let older = tool
            .execute(serde_json::json!({
                "key": "session:history", "limit": 1, "offset": 1
            }))
            .await?;
        assert_eq!(older["messages"][0], messages[0]);
        Ok(())
    }

    #[tokio::test]
    async fn sessions_history_rejects_missing_session() -> TestResult<()> {
        let metadata = Arc::new(SqliteSessionMetadata::new(test_pool().await?));
        let tmp = tempfile::tempdir()?;
        let store = Arc::new(SessionStore::new(tmp.path().to_path_buf()));
        let tool = SessionsHistoryTool::new(store, metadata);

        let result = tool
            .execute(serde_json::json!({
                "key": "session:missing"
            }))
            .await;
        let err = result
            .err()
            .ok_or_else(|| std::io::Error::other("expected missing session error"))?;
        assert!(err.to_string().contains("session not found"));
        Ok(())
    }

    #[tokio::test]
    async fn sessions_search_finds_matches_and_excludes_current_by_default() -> TestResult<()> {
        let metadata = Arc::new(SqliteSessionMetadata::new(test_pool().await?));
        create_test_session(&metadata, "session:current", "Current").await?;
        create_test_session(&metadata, "session:other", "Other").await?;

        let tmp = tempfile::tempdir()?;
        let store = Arc::new(SessionStore::new(tmp.path().to_path_buf()));
        store
            .append(
                "session:current",
                &serde_json::json!({
                    "role": "user",
                    "content": "rust checkpoint design"
                }),
            )
            .await?;
        store
            .append(
                "session:other",
                &serde_json::json!({
                    "role": "assistant",
                    "content": "rust checkpoint design with rollback"
                }),
            )
            .await?;

        let tool = SessionsSearchTool::new(store, metadata);
        let result = tool
            .execute(serde_json::json!({
                "query": "checkpoint",
                "_session_key": "session:current"
            }))
            .await?;

        assert_eq!(result["count"], 1);
        let results = result
            .get("results")
            .and_then(Value::as_array)
            .ok_or_else(|| std::io::Error::other("missing results array"))?;
        assert_eq!(results[0]["key"], "session:other");
        Ok(())
    }

    #[tokio::test]
    async fn sessions_search_can_include_current_session() -> TestResult<()> {
        let metadata = Arc::new(SqliteSessionMetadata::new(test_pool().await?));
        create_test_session(&metadata, "session:current", "Current").await?;

        let tmp = tempfile::tempdir()?;
        let store = Arc::new(SessionStore::new(tmp.path().to_path_buf()));
        store
            .append(
                "session:current",
                &serde_json::json!({
                    "role": "user",
                    "content": "needle in current session"
                }),
            )
            .await?;

        let tool = SessionsSearchTool::new(store, metadata);
        let result = tool
            .execute(serde_json::json!({
                "query": "needle",
                "_session_key": "session:current",
                "exclude_current": false
            }))
            .await?;

        assert_eq!(result["count"], 1);
        assert_eq!(result["results"][0]["key"], "session:current");
        Ok(())
    }

    #[tokio::test]
    async fn sessions_send_calls_callback_and_wraps_context() -> TestResult<()> {
        let metadata = Arc::new(SqliteSessionMetadata::new(test_pool().await?));
        create_test_session(&metadata, "session:target", "Target").await?;

        let called = Arc::new(AtomicBool::new(false));
        let called_ref = Arc::clone(&called);
        let send_fn: SendToSessionFn = Arc::new(move |req| {
            let called_ref = Arc::clone(&called_ref);
            Box::pin(async move {
                called_ref.store(true, Ordering::SeqCst);
                assert_eq!(req.key, "session:target");
                assert!(req.message.starts_with("[From: coordinator]"));
                assert!(req.wait_for_reply);
                assert!(req.model_override.is_none());
                Ok(serde_json::json!({
                    "text": "ok",
                    "inputTokens": 1,
                    "outputTokens": 1
                }))
            })
        });
        let tool = SessionsSendTool::new(metadata, send_fn);

        let result = tool
            .execute(serde_json::json!({
                "key": "session:target",
                "message": "Do work",
                "context": "coordinator",
                "wait_for_reply": true,
                "model": { "session": {} }
            }))
            .await?;

        assert_eq!(result["sent"], true);
        assert_eq!(result["waitForReply"], true);
        assert_eq!(result["result"]["text"], "ok");
        assert!(called.load(Ordering::SeqCst));
        Ok(())
    }

    #[tokio::test]
    async fn sessions_send_passes_model_override_to_callback() -> TestResult<()> {
        let metadata = Arc::new(SqliteSessionMetadata::new(test_pool().await?));
        create_test_session(&metadata, "session:target", "Target").await?;

        let send_fn: SendToSessionFn = Arc::new(move |req| {
            Box::pin(async move {
                let override_config = req
                    .model_override
                    .ok_or_else(|| std::io::Error::other("missing model override"))?;
                assert_eq!(override_config.model, "openai::gpt-5.2");
                assert_eq!(override_config.reasoning_effort.as_str(), "high");
                Ok(serde_json::json!({ "ok": true }))
            })
        });
        let tool = SessionsSendTool::new(metadata, send_fn);

        tool.execute(serde_json::json!({
            "key": "session:target",
            "message": "Do work",
            "model": {
                "override": {
                    "model": "openai::gpt-5.2",
                    "reasoning_effort": "high"
                }
            }
        }))
        .await?;
        Ok(())
    }

    #[tokio::test]
    async fn sessions_send_rejects_invalid_parameters_before_state() -> TestResult<()> {
        let pool = sqlx::SqlitePool::connect_lazy("sqlite::memory:")?;
        pool.close().await;
        let tool = SessionsSendTool::new(
            Arc::new(SqliteSessionMetadata::new(pool)),
            Arc::new(|_| {
                Box::pin(async { panic!("invalid parameters must not reach the callback") })
            }),
        );
        for params in [
            serde_json::json!({"message": "hello", "model": {"session": {}}}),
            serde_json::json!({"key": "session:target", "model": {"session": {}}}),
            serde_json::json!({"key": "", "message": "hello", "model": {"session": {}}}),
            serde_json::json!({"key": "session:target", "message": "", "model": {"session": {}}}),
            serde_json::json!({"key": "session:target", "message": "hello", "model": {"session": {}}, "context": null}),
            serde_json::json!({"key": "session:target", "message": "hello", "model": {"session": {}}, "wait_for_reply": null}),
            serde_json::json!({"key": "session:target", "message": "hello", "model": {"session": {}}, "wait_for_reply": "true"}),
            serde_json::json!({"key": "session:target", "message": "hello"}),
            serde_json::json!({"key": "session:target", "message": "hello", "model": {}}),
            serde_json::json!({"key": "session:target", "message": "hello", "model": null}),
            serde_json::json!({"key": "session:target", "message": "hello", "model": []}),
            serde_json::json!({"key": "session:target", "message": "hello", "model": {"session": {}, "override": {"model": "openai::gpt-5.2", "reasoning_effort": "high"}}}),
            serde_json::json!({"key": "session:target", "message": "hello", "model": {"session": []}}),
            serde_json::json!({"key": "session:target", "message": "hello", "model": {"session": null}}),
            serde_json::json!({"key": "session:target", "message": "hello", "model": {"session": {"extra_field": true}}}),
            serde_json::json!({"key": "session:target", "message": "hello", "model": {"override": null}}),
            serde_json::json!({"key": "session:target", "message": "hello", "model": {"override": []}}),
            serde_json::json!({"key": "session:target", "message": "hello", "model": {"override": {"reasoning_effort": "low"}}}),
            serde_json::json!({"key": "session:target", "message": "hello", "model": {"override": {"model": "", "reasoning_effort": "low"}}}),
            serde_json::json!({"key": "session:target", "message": "hello", "model": {"override": {"model": "openai::gpt-5.2", "reasoning_effort": "high", "extra_field": true}}}),
            serde_json::json!({"key": "session:target", "message": "hello", "model": {"extra_field": true}}),
        ] {
            assert!(tool.validate(&params).is_err(), "{params}");
            let result = tool.execute(params).await;
            assert!(matches!(result, Err(ref error) if error.is::<serde_json::Error>()));
        }
        Ok(())
    }

    #[tokio::test]
    async fn sessions_send_rejects_additional_field() -> TestResult<()> {
        let pool = sqlx::SqlitePool::connect_lazy("sqlite::memory:")?;
        pool.close().await;
        let tool = SessionsSendTool::new(
            Arc::new(SqliteSessionMetadata::new(pool)),
            Arc::new(|_| {
                Box::pin(async { panic!("additional fields must not reach the callback") })
            }),
        );
        let schema = tool.parameters_schema();
        assert_eq!(schema["additionalProperties"], false);
        assert!(schema.get("oneOf").is_none());
        assert_eq!(
            schema["required"],
            serde_json::json!(["key", "message", "model"])
        );
        assert!(schema["properties"].get("model_override").is_none());
        let variants = schema["properties"]["model"]["oneOf"]
            .as_array()
            .ok_or_else(|| std::io::Error::other("model.oneOf must be an array"))?;
        assert_eq!(variants.len(), 2);
        assert_eq!(variants[0]["required"], serde_json::json!(["session"]));
        assert!(
            variants[0]["properties"]["session"]["properties"]
                .get("model")
                .is_none()
        );
        assert!(
            variants[0]["properties"]["session"]["properties"]
                .get("reasoning_effort")
                .is_none()
        );
        assert_eq!(variants[1]["required"], serde_json::json!(["override"]));
        assert_eq!(
            variants[1]["properties"]["override"]["required"],
            serde_json::json!(["model", "reasoning_effort"])
        );
        let params = serde_json::json!({
            "key": "session:target",
            "message": "hello",
            "model": { "session": {} },
            "extra_field": true,
        });
        assert!(tool.validate(&params).is_err());
        let result = tool.execute(params).await;
        assert!(matches!(result, Err(ref error) if error.is::<serde_json::Error>()));
        Ok(())
    }

    #[tokio::test]
    async fn sessions_send_rejects_missing_target() -> TestResult<()> {
        let metadata = Arc::new(SqliteSessionMetadata::new(test_pool().await?));
        let send_fn: SendToSessionFn = Arc::new(move |_req| {
            Box::pin(async move {
                Ok(serde_json::json!({
                    "ok": true
                }))
            })
        });
        let tool = SessionsSendTool::new(metadata, send_fn);

        let result = tool
            .execute(serde_json::json!({
                "key": "session:missing",
                "message": "hello",
                "model": { "session": {} }
            }))
            .await;
        let err = result
            .err()
            .ok_or_else(|| std::io::Error::other("expected missing target error"))?;
        assert!(err.to_string().contains("session not found"));
        Ok(())
    }

    #[tokio::test]
    async fn test_list_filtered_by_key_prefix() -> TestResult<()> {
        let metadata = Arc::new(SqliteSessionMetadata::new(test_pool().await?));
        create_test_session(&metadata, "agent:scout:1", "Scout 1").await?;
        create_test_session(&metadata, "agent:scout:2", "Scout 2").await?;
        create_test_session(&metadata, "agent:coder:1", "Coder 1").await?;

        let policy = SessionAccessPolicy {
            key_prefix: Some("agent:scout:".into()),
            ..Default::default()
        };
        let tool = SessionsListTool::new(metadata).with_policy(policy);
        let result = tool.execute(serde_json::json!({})).await?;

        assert_eq!(result["count"], 2);
        Ok(())
    }

    #[tokio::test]
    async fn test_search_filtered_by_key_prefix() -> TestResult<()> {
        let metadata = Arc::new(SqliteSessionMetadata::new(test_pool().await?));
        create_test_session(&metadata, "agent:scout:1", "Scout 1").await?;
        create_test_session(&metadata, "agent:coder:1", "Coder 1").await?;

        let tmp = tempfile::tempdir()?;
        let store = Arc::new(SessionStore::new(tmp.path().to_path_buf()));
        store
            .append(
                "agent:scout:1",
                &serde_json::json!({"role": "user", "content": "shared search term"}),
            )
            .await?;
        store
            .append(
                "agent:coder:1",
                &serde_json::json!({"role": "user", "content": "shared search term"}),
            )
            .await?;

        let policy = SessionAccessPolicy {
            key_prefix: Some("agent:scout:".into()),
            ..Default::default()
        };
        let tool = SessionsSearchTool::new(store, metadata).with_policy(policy);
        let result = tool.execute(serde_json::json!({"query": "shared"})).await?;

        assert_eq!(result["count"], 1);
        assert_eq!(result["results"][0]["key"], "agent:scout:1");
        Ok(())
    }

    #[tokio::test]
    async fn test_send_denied_when_can_send_false() -> TestResult<()> {
        let metadata = Arc::new(SqliteSessionMetadata::new(test_pool().await?));
        create_test_session(&metadata, "session:target", "Target").await?;

        let send_fn: SendToSessionFn =
            Arc::new(move |_req| Box::pin(async move { Ok(serde_json::json!({"ok": true})) }));
        let policy = SessionAccessPolicy {
            can_send: false,
            ..Default::default()
        };
        let tool = SessionsSendTool::new(metadata, send_fn).with_policy(policy);

        let result = tool
            .execute(serde_json::json!({
                "key": "session:target",
                "message": "hello",
                "model": { "session": {} }
            }))
            .await;

        let err = result.expect_err("should deny sending");
        assert!(err.to_string().contains("denies sending"));
        Ok(())
    }

    #[tokio::test]
    async fn test_no_policy_allows_all() -> TestResult<()> {
        let metadata = Arc::new(SqliteSessionMetadata::new(test_pool().await?));
        create_test_session(&metadata, "agent:scout:1", "Scout 1").await?;
        create_test_session(&metadata, "agent:coder:1", "Coder 1").await?;

        // No policy = all sessions visible.
        let tool = SessionsListTool::new(metadata);
        let result = tool.execute(serde_json::json!({})).await?;

        assert_eq!(result["count"], 2);
        Ok(())
    }

    #[test]
    fn sender_badge_formats_with_blank_line() {
        assert_eq!(sender_badge("Coder"), "[From the \"Coder\" agent]\n\n");
    }

    fn badge_agents_config() -> Arc<tokio::sync::RwLock<chelix_config::AgentsConfig>> {
        let mut agents = chelix_config::AgentsConfig {
            default: "coder".to_string(),
            ..Default::default()
        };
        let mut coder = chelix_config::AgentConfig::new(
            "Coder",
            "test::model",
            chelix_common::ReasoningEffort::from("off"),
        );
        coder.prepend_sender_badge = true;
        let mut quiet = chelix_config::AgentConfig::new(
            "Quiet",
            "test::model",
            chelix_common::ReasoningEffort::from("off"),
        );
        quiet.prepend_sender_badge = false;
        agents.entries.insert("coder".to_string(), coder);
        agents.entries.insert("quiet".to_string(), quiet);
        Arc::new(tokio::sync::RwLock::new(agents))
    }

    #[tokio::test]
    async fn sessions_send_prepends_badge_when_enabled() -> TestResult<()> {
        let metadata = Arc::new(SqliteSessionMetadata::new(test_pool().await?));
        create_test_session(&metadata, "session:target", "Target").await?;
        let send_fn: SendToSessionFn = Arc::new(move |req| {
            Box::pin(async move {
                assert_eq!(req.message, "[From the \"Coder\" agent]\n\nDo work");
                Ok(serde_json::json!({ "ok": true }))
            })
        });
        let tool =
            SessionsSendTool::new(metadata, send_fn).with_agents_config(badge_agents_config());
        let context = ToolExecutionContext::for_session_with_agent(
            chelix_sessions::SessionKey::new("session:sender"),
            "coder",
        );
        tool.execute_with_context(
            serde_json::json!({
                "key": "session:target",
                "message": "Do work",
                "model": { "session": {} }
            }),
            &context,
        )
        .await?;
        Ok(())
    }

    #[tokio::test]
    async fn sessions_send_omits_badge_when_disabled() -> TestResult<()> {
        let metadata = Arc::new(SqliteSessionMetadata::new(test_pool().await?));
        create_test_session(&metadata, "session:target", "Target").await?;
        let send_fn: SendToSessionFn = Arc::new(move |req| {
            Box::pin(async move {
                assert_eq!(req.message, "Do work");
                Ok(serde_json::json!({ "ok": true }))
            })
        });
        let tool =
            SessionsSendTool::new(metadata, send_fn).with_agents_config(badge_agents_config());
        let context = ToolExecutionContext::for_session_with_agent(
            chelix_sessions::SessionKey::new("session:sender"),
            "quiet",
        );
        tool.execute_with_context(
            serde_json::json!({
                "key": "session:target",
                "message": "Do work",
                "model": { "session": {} }
            }),
            &context,
        )
        .await?;
        Ok(())
    }

    #[tokio::test]
    async fn sessions_send_places_badge_above_context() -> TestResult<()> {
        let metadata = Arc::new(SqliteSessionMetadata::new(test_pool().await?));
        create_test_session(&metadata, "session:target", "Target").await?;
        let send_fn: SendToSessionFn = Arc::new(move |req| {
            Box::pin(async move {
                assert_eq!(
                    req.message,
                    "[From the \"Coder\" agent]\n\n[From: coordinator]\n\nDo work"
                );
                Ok(serde_json::json!({ "ok": true }))
            })
        });
        let tool =
            SessionsSendTool::new(metadata, send_fn).with_agents_config(badge_agents_config());
        let context = ToolExecutionContext::for_session_with_agent(
            chelix_sessions::SessionKey::new("session:sender"),
            "coder",
        );
        tool.execute_with_context(
            serde_json::json!({
                "key": "session:target",
                "message": "Do work",
                "context": "coordinator",
                "model": { "session": {} }
            }),
            &context,
        )
        .await?;
        Ok(())
    }

    #[tokio::test]
    async fn sessions_send_rejects_unknown_sender() -> TestResult<()> {
        let metadata = Arc::new(SqliteSessionMetadata::new(test_pool().await?));
        create_test_session(&metadata, "session:target", "Target").await?;
        let send_fn: SendToSessionFn = Arc::new(move |_| {
            Box::pin(async move { panic!("unknown sender must not reach callback") })
        });
        let tool =
            SessionsSendTool::new(metadata, send_fn).with_agents_config(badge_agents_config());
        let context = ToolExecutionContext::for_session_with_agent(
            chelix_sessions::SessionKey::new("session:sender"),
            "missing",
        );
        let result = tool
            .execute_with_context(
                serde_json::json!({
                    "key": "session:target",
                    "message": "hi",
                    "model": { "session": {} }
                }),
                &context,
            )
            .await;
        assert!(
            result
                .err()
                .is_some_and(|error| error.to_string().contains("unknown sender agent"))
        );
        Ok(())
    }

    #[tokio::test]
    async fn sessions_send_without_sender_context_sends_plain() -> TestResult<()> {
        let metadata = Arc::new(SqliteSessionMetadata::new(test_pool().await?));
        create_test_session(&metadata, "session:target", "Target").await?;
        let send_fn: SendToSessionFn = Arc::new(move |req| {
            Box::pin(async move {
                assert_eq!(req.message, "Do work");
                Ok(serde_json::json!({ "ok": true }))
            })
        });
        let tool =
            SessionsSendTool::new(metadata, send_fn).with_agents_config(badge_agents_config());
        tool.execute(serde_json::json!({
            "key": "session:target",
            "message": "Do work",
            "model": { "session": {} }
        }))
        .await?;
        Ok(())
    }
}
