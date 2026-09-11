use std::{collections::HashMap, sync::Arc, time::Instant};

use {
    chelix_agents::tool_registry::ToolRegistry,
    chelix_common::ReasoningEffort,
    chelix_service_traits::{
        ChatExecutionContext, ChatSendRequest, ChatSendSyncRequest, ChatService,
    },
    chelix_sessions::{
        SessionKey,
        message::PersistedMessage,
        metadata::{SessionEntry, SqliteSessionMetadata},
        store::SessionStore,
    },
    chelix_tools::sub_agent::{SubAgentMode, SubAgentRequest, SubAgentTool},
    serde_json::Value,
    tokio::sync::RwLock,
    tracing::info,
};

use crate::state::GatewayState;

#[derive(Debug)]
struct DiscoverableAgent {
    id: String,
    model: String,
    reasoning_effort: ReasoningEffort,
}

struct SubAgentRuntime {
    state: Arc<GatewayState>,
    session_store: Arc<SessionStore>,
    session_metadata: Arc<SqliteSessionMetadata>,
    background_runs: RwLock<HashMap<String, String>>,
}

pub(super) fn register_sub_agent_tool(
    tool_registry: &mut ToolRegistry,
    state: &Arc<GatewayState>,
    session_store: &Arc<SessionStore>,
    session_metadata: &Arc<SqliteSessionMetadata>,
) {
    let runtime = Arc::new(SubAgentRuntime {
        state: Arc::clone(state),
        session_store: Arc::clone(session_store),
        session_metadata: Arc::clone(session_metadata),
        background_runs: RwLock::new(HashMap::new()),
    });
    let execute = Arc::new(move |request| {
        let runtime = Arc::clone(&runtime);
        Box::pin(async move { runtime.execute(request).await })
            as futures::future::BoxFuture<'static, chelix_tools::Result<Value>>
    });
    tool_registry.register(Box::new(SubAgentTool::new(execute)));
}

impl SubAgentRuntime {
    #[tracing::instrument(name = "sub_agent.dispatch", skip_all, fields(action = request.action()))]
    async fn execute(&self, request: SubAgentRequest) -> chelix_tools::Result<Value> {
        match request {
            SubAgentRequest::Explore => self.explore().await,
            SubAgentRequest::Run {
                parent_session_key,
                sender_agent_id,
                agent_id,
                task,
                mode,
            } => {
                self.run(
                    &parent_session_key,
                    sender_agent_id.as_deref(),
                    &agent_id,
                    &task,
                    mode,
                )
                .await
            },
            SubAgentRequest::Status {
                parent_session_key,
                session_key,
            } => self.status(&parent_session_key, &session_key).await,
            SubAgentRequest::List { parent_session_key } => self.list(&parent_session_key).await,
            SubAgentRequest::Result {
                parent_session_key,
                session_key,
            } => self.result(&parent_session_key, &session_key).await,
            SubAgentRequest::Cancel {
                parent_session_key,
                session_key,
            } => self.cancel(&parent_session_key, &session_key).await,
        }
    }

    #[tracing::instrument(name = "sub_agent.explore", skip_all)]
    async fn explore(&self) -> chelix_tools::Result<Value> {
        let agents_config = self.agents_config()?;
        let guard = agents_config.read().await;
        let mut agents = guard
            .entries
            .iter()
            .filter_map(|(id, agent)| {
                discoverable_agent_from_config(
                    id,
                    agent,
                    chelix_config::load_subagent_prompt_for_agent(id),
                )
                .ok()
                .map(|_| {
                    serde_json::json!({
                        "id": id,
                        "name": agent.name,
                        "description": agent.description,
                    })
                })
            })
            .collect::<Vec<_>>();
        agents.sort_by(|left, right| {
            let left_id = left.get("id").and_then(Value::as_str).unwrap_or("");
            let right_id = right.get("id").and_then(Value::as_str).unwrap_or("");
            left_id.cmp(right_id)
        });
        Ok(serde_json::json!({ "agents": agents }))
    }

    #[tracing::instrument(
        name = "sub_agent.run",
        skip_all,
        fields(parent_session_key, agent_id, mode = mode.as_str())
    )]
    async fn run(
        &self,
        parent_session_key: &str,
        sender_agent_id: Option<&str>,
        agent_id: &str,
        task: &str,
        mode: SubAgentMode,
    ) -> chelix_tools::Result<Value> {
        let started = Instant::now();
        let agent = self.discoverable_agent(agent_id).await?;
        let sender_badge = self.sender_badge_prefix(sender_agent_id).await?;
        let model_reasoning = crate::model_reasoning::resolve_model_reasoning(
            self.state.services.model.as_ref(),
            &agent.model,
            &agent.reasoning_effort,
        )
        .await
        .map_err(|error| chelix_tools::Error::message(error.to_string()))?;
        let parent = self
            .session_metadata
            .try_get(parent_session_key)
            .await
            .map_err(tool_error)?
            .ok_or_else(|| {
                chelix_tools::Error::message(format!(
                    "parent session {parent_session_key:?} does not exist"
                ))
            })?;
        let owner_key = parent
            .sandbox_owner_key
            .unwrap_or_else(|| parent.key.clone());
        if self
            .session_metadata
            .try_get(&owner_key)
            .await
            .map_err(tool_error)?
            .is_none()
        {
            return Err(chelix_tools::Error::message(format!(
                "sandbox owner session {owner_key:?} referenced by {parent_session_key:?} does not exist"
            )));
        }

        let session_key = format!("session:{}", uuid::Uuid::new_v4());
        let label = sub_agent_label(task);
        self.session_metadata
            .create_subagent_session(
                &session_key,
                &label,
                parent_session_key,
                &owner_key,
                agent_id,
                &model_reasoning,
            )
            .await
            .map_err(tool_error)?;

        info!(
            session_key,
            agent_id,
            mode = mode.as_str(),
            sandbox_owner_key = owner_key,
            "sub-agent session created"
        );

        let chat = self.state.chat();
        let effective_task = if sender_badge.is_empty() {
            task.to_string()
        } else {
            format!("{sender_badge}{task}")
        };
        match mode {
            SubAgentMode::Blocking => {
                let response = match chat
                    .send_sync(
                        ChatSendSyncRequest::text(effective_task),
                        ChatExecutionContext::internal(SessionKey::new(session_key.clone())),
                    )
                    .await
                {
                    Ok(response) => response,
                    Err(error) => {
                        record_run_metric(mode, "failed", started.elapsed());
                        return Err(chelix_tools::Error::message(error.to_string()));
                    },
                };
                let output = serde_json::json!({
                    "sessionKey": session_key,
                    "agentId": agent.id,
                    "mode": mode.as_str(),
                    "text": response_field(&response, "text")?,
                    "inputTokens": response_field(&response, "inputTokens")?,
                    "outputTokens": response_field(&response, "outputTokens")?,
                    "durationMs": response_field(&response, "durationMs")?,
                });
                record_run_metric(mode, "completed", started.elapsed());
                info!(
                    session_key,
                    agent_id,
                    duration_ms = started.elapsed().as_millis(),
                    "blocking sub-agent run completed"
                );
                Ok(output)
            },
            SubAgentMode::Background => {
                let response = match chat
                    .send(
                        ChatSendRequest::text(effective_task),
                        ChatExecutionContext::internal(SessionKey::new(session_key.clone())),
                    )
                    .await
                {
                    Ok(response) => response,
                    Err(error) => {
                        record_run_metric(mode, "failed", started.elapsed());
                        return Err(chelix_tools::Error::message(error.to_string()));
                    },
                };
                let run_id = response
                    .get("runId")
                    .and_then(Value::as_str)
                    .ok_or_else(|| {
                        chelix_tools::Error::message("chat.send response is missing runId")
                    })?
                    .to_string();
                self.background_runs
                    .write()
                    .await
                    .insert(session_key.clone(), run_id.clone());
                record_background_started_metric();
                Ok(serde_json::json!({
                    "sessionKey": session_key,
                    "agentId": agent.id,
                    "mode": mode.as_str(),
                    "runId": run_id,
                    "status": "running",
                }))
            },
        }
    }

    #[tracing::instrument(
        name = "sub_agent.status",
        skip_all,
        fields(parent_session_key, session_key)
    )]
    async fn status(
        &self,
        parent_session_key: &str,
        session_key: &str,
    ) -> chelix_tools::Result<Value> {
        sub_agent_status(
            &self.session_metadata,
            self.state.chat().as_ref(),
            parent_session_key,
            session_key,
        )
        .await
    }

    #[tracing::instrument(name = "sub_agent.list", skip_all, fields(parent_session_key))]
    async fn list(&self, parent_session_key: &str) -> chelix_tools::Result<Value> {
        sub_agent_list(
            &self.session_metadata,
            self.state.chat().as_ref(),
            parent_session_key,
        )
        .await
    }

    #[tracing::instrument(
        name = "sub_agent.result",
        skip_all,
        fields(parent_session_key, session_key)
    )]
    async fn result(
        &self,
        parent_session_key: &str,
        session_key: &str,
    ) -> chelix_tools::Result<Value> {
        sub_agent_result(
            &self.session_metadata,
            self.state.chat().as_ref(),
            &self.session_store,
            &self.background_runs,
            parent_session_key,
            session_key,
        )
        .await
    }

    #[tracing::instrument(
        name = "sub_agent.cancel",
        skip_all,
        fields(parent_session_key, session_key)
    )]
    async fn cancel(
        &self,
        parent_session_key: &str,
        session_key: &str,
    ) -> chelix_tools::Result<Value> {
        sub_agent_cancel(
            &self.session_metadata,
            self.state.chat().as_ref(),
            parent_session_key,
            session_key,
        )
        .await
    }

    fn agents_config(&self) -> chelix_tools::Result<&Arc<RwLock<chelix_config::AgentsConfig>>> {
        self.state
            .services
            .agents_config
            .as_ref()
            .ok_or_else(|| chelix_tools::Error::message("agent configuration is not available"))
    }

    async fn discoverable_agent(&self, agent_id: &str) -> chelix_tools::Result<DiscoverableAgent> {
        let agents_config = self.agents_config()?;
        let guard = agents_config.read().await;
        let agent = guard.get(agent_id).ok_or_else(|| {
            chelix_tools::Error::message(format!(
                "agent {agent_id:?} is not configured and is not available from sub_agent explore"
            ))
        })?;
        discoverable_agent_from_config(
            agent_id,
            agent,
            chelix_config::load_subagent_prompt_for_agent(agent_id),
        )
    }

    async fn sender_badge_prefix(
        &self,
        sender_agent_id: Option<&str>,
    ) -> chelix_tools::Result<String> {
        let Some(sender_agent_id) = sender_agent_id else {
            return Ok(String::new());
        };
        let agents_config = self.agents_config()?;
        let guard = agents_config.read().await;
        let agent = guard.get(sender_agent_id).ok_or_else(|| {
            chelix_tools::Error::message(format!("unknown sender agent '{sender_agent_id}'"))
        })?;
        if agent.prepend_sender_badge {
            Ok(chelix_tools::sessions_communicate::sender_badge(
                &agent.name,
            ))
        } else {
            Ok(String::new())
        }
    }
}

fn discoverable_agent_from_config(
    agent_id: &str,
    agent: &chelix_config::AgentConfig,
    subagent_prompt: Option<String>,
) -> chelix_tools::Result<DiscoverableAgent> {
    if subagent_prompt.is_none_or(|prompt| prompt.trim().is_empty()) {
        return Err(chelix_tools::Error::message(format!(
            "agent {agent_id:?} has no non-empty SUBAGENT.md and is not available from sub_agent explore"
        )));
    }
    Ok(DiscoverableAgent {
        id: agent_id.to_string(),
        model: agent.model.clone(),
        reasoning_effort: agent.reasoning_effort.clone(),
    })
}

async fn sub_agent_status(
    metadata: &SqliteSessionMetadata,
    chat: &dyn ChatService,
    parent_session_key: &str,
    session_key: &str,
) -> chelix_tools::Result<Value> {
    let entry = owned_child_entry(metadata, parent_session_key, session_key).await?;
    status_payload(chat, entry).await
}

async fn sub_agent_list(
    metadata: &SqliteSessionMetadata,
    chat: &dyn ChatService,
    parent_session_key: &str,
) -> chelix_tools::Result<Value> {
    let entries = metadata
        .list_children_result(parent_session_key)
        .await
        .map_err(tool_error)?;
    let mut sessions = Vec::with_capacity(entries.len());
    for entry in entries {
        sessions.push(status_payload(chat, entry).await?);
    }
    Ok(serde_json::json!({ "sessions": sessions }))
}

async fn sub_agent_result(
    metadata: &SqliteSessionMetadata,
    chat: &dyn ChatService,
    session_store: &SessionStore,
    background_runs: &RwLock<HashMap<String, String>>,
    parent_session_key: &str,
    session_key: &str,
) -> chelix_tools::Result<Value> {
    let entry = owned_child_entry(metadata, parent_session_key, session_key).await?;
    require_completed(chat, session_key).await?;
    let run_id = background_runs
        .read()
        .await
        .get(session_key)
        .cloned()
        .ok_or_else(|| {
            chelix_tools::Error::message(format!(
                "no background run is tracked for session {session_key:?}"
            ))
        })?;
    let messages = session_store
        .read_by_run_id(session_key, &run_id)
        .await
        .map_err(tool_error)?;
    let (text, duration_ms) = last_assistant_result(messages)?.ok_or_else(|| {
        chelix_tools::Error::message(format!("background run {run_id:?} has no assistant result"))
    })?;
    record_background_completion_metric(duration_ms);
    Ok(serde_json::json!({
        "sessionKey": session_key,
        "agentId": entry.agent_id,
        "text": text,
    }))
}

async fn sub_agent_cancel(
    metadata: &SqliteSessionMetadata,
    chat: &dyn ChatService,
    parent_session_key: &str,
    session_key: &str,
) -> chelix_tools::Result<Value> {
    owned_child_entry(metadata, parent_session_key, session_key).await?;
    abort_sub_agent(chat, session_key).await
}

async fn status_payload(
    chat: &dyn ChatService,
    entry: SessionEntry,
) -> chelix_tools::Result<Value> {
    let active = chat_active(chat, &entry.key).await?;
    Ok(serde_json::json!({
        "sessionKey": entry.key,
        "agentId": entry.agent_id,
        "label": entry.label,
        "status": if active { "running" } else { "idle" },
        "messageCount": entry.message_count,
        "createdAt": entry.created_at,
        "updatedAt": entry.updated_at,
    }))
}

#[tracing::instrument(skip_all, fields(parent_session_key, session_key))]
async fn owned_child_entry(
    metadata: &SqliteSessionMetadata,
    parent_session_key: &str,
    session_key: &str,
) -> chelix_tools::Result<SessionEntry> {
    let entry = metadata
        .try_get(session_key)
        .await
        .map_err(tool_error)?
        .ok_or_else(|| {
            chelix_tools::Error::message(format!("session {session_key:?} does not exist"))
        })?;
    if entry.parent_session_key.as_deref() != Some(parent_session_key) {
        return Err(chelix_tools::Error::message(format!(
            "access denied to sub-agent session {session_key:?}"
        )));
    }
    Ok(entry)
}

#[tracing::instrument(skip_all, fields(session_key))]
async fn chat_active(chat: &dyn ChatService, session_key: &str) -> chelix_tools::Result<bool> {
    let response = chat
        .active(serde_json::json!({ "sessionKey": session_key }))
        .await
        .map_err(|error| chelix_tools::Error::message(error.to_string()))?;
    response
        .get("active")
        .and_then(Value::as_bool)
        .ok_or_else(|| chelix_tools::Error::message("chat.active response is missing active"))
}

#[tracing::instrument(skip_all, fields(session_key))]
async fn require_completed(chat: &dyn ChatService, session_key: &str) -> chelix_tools::Result<()> {
    if chat_active(chat, session_key).await? {
        return Err(chelix_tools::Error::message(format!(
            "sub-agent run for session {session_key:?} is still running"
        )));
    }
    Ok(())
}

#[tracing::instrument(skip_all, fields(session_key))]
async fn abort_sub_agent(chat: &dyn ChatService, session_key: &str) -> chelix_tools::Result<Value> {
    let response = chat
        .abort(serde_json::json!({ "sessionKey": session_key }))
        .await
        .map_err(|error| chelix_tools::Error::message(error.to_string()))?;
    let aborted = response_field(&response, "aborted")?;
    let run_id = response_field(&response, "runId")?;
    Ok(serde_json::json!({
        "sessionKey": session_key,
        "aborted": aborted,
        "runId": run_id,
    }))
}

fn last_assistant_result(
    messages: Vec<Value>,
) -> chelix_tools::Result<Option<(String, Option<u64>)>> {
    for message in messages.into_iter().rev() {
        let persisted = serde_json::from_value::<PersistedMessage>(message)
            .map_err(|error| chelix_tools::Error::message(error.to_string()))?;
        if let PersistedMessage::Assistant {
            content,
            duration_ms,
            ..
        } = persisted
        {
            return Ok(Some((content, duration_ms)));
        }
    }
    Ok(None)
}

fn response_field<'a>(response: &'a Value, name: &str) -> chelix_tools::Result<&'a Value> {
    response
        .get(name)
        .ok_or_else(|| chelix_tools::Error::message(format!("chat response is missing {name}")))
}

fn sub_agent_label(task: &str) -> String {
    task.trim().chars().take(64).collect()
}

fn tool_error(error: impl std::fmt::Display) -> chelix_tools::Error {
    chelix_tools::Error::message(error.to_string())
}

#[cfg(feature = "metrics")]
fn record_run_metric(mode: SubAgentMode, status: &'static str, duration: std::time::Duration) {
    use chelix_metrics::{counter, histogram, labels};

    counter!(
        chelix_metrics::sub_agent::RUNS_TOTAL,
        labels::MODE => mode.as_str(),
        labels::STATUS => status
    )
    .increment(1);
    histogram!(
        chelix_metrics::sub_agent::RUN_DURATION_SECONDS,
        labels::MODE => mode.as_str()
    )
    .record(duration.as_secs_f64());
}

#[cfg(not(feature = "metrics"))]
fn record_run_metric(_mode: SubAgentMode, _status: &'static str, _duration: std::time::Duration) {}

#[cfg(feature = "metrics")]
fn record_background_started_metric() {
    use chelix_metrics::{counter, labels};

    counter!(
        chelix_metrics::sub_agent::RUNS_TOTAL,
        labels::MODE => SubAgentMode::Background.as_str(),
        labels::STATUS => "running"
    )
    .increment(1);
}

#[cfg(not(feature = "metrics"))]
fn record_background_started_metric() {}

#[cfg(feature = "metrics")]
fn record_background_completion_metric(duration_ms: Option<u64>) {
    use chelix_metrics::{counter, histogram, labels};

    counter!(
        chelix_metrics::sub_agent::RUNS_TOTAL,
        labels::MODE => SubAgentMode::Background.as_str(),
        labels::STATUS => "completed"
    )
    .increment(1);
    if let Some(duration_ms) = duration_ms {
        histogram!(
            chelix_metrics::sub_agent::RUN_DURATION_SECONDS,
            labels::MODE => SubAgentMode::Background.as_str()
        )
        .record(std::time::Duration::from_millis(duration_ms).as_secs_f64());
    }
}

#[cfg(not(feature = "metrics"))]
fn record_background_completion_metric(_duration_ms: Option<u64>) {}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::unwrap_used)]

    use {
        super::*,
        async_trait::async_trait,
        chelix_service_traits::{
            ChatCompactRequest, ChatContextRequest, ChatExecutionContext, ChatFullContextRequest,
            ChatRawPromptRequest, ChatSendRequest, ChatSendSyncRequest, ServiceResult,
        },
        std::sync::atomic::{AtomicUsize, Ordering},
    };

    struct LifecycleChatService {
        active: bool,
        abort_response: Value,
        abort_calls: AtomicUsize,
    }

    impl LifecycleChatService {
        fn new(active: bool, abort_response: Value) -> Self {
            Self {
                active,
                abort_response,
                abort_calls: AtomicUsize::new(0),
            }
        }
    }

    #[async_trait]
    impl ChatService for LifecycleChatService {
        async fn send(
            &self,
            _request: ChatSendRequest,
            _context: ChatExecutionContext,
        ) -> ServiceResult {
            Err("send is not used by this test service".into())
        }

        async fn send_sync(
            &self,
            _request: ChatSendSyncRequest,
            _context: ChatExecutionContext,
        ) -> ServiceResult {
            Err("send_sync is not used by this test service".into())
        }

        async fn abort(&self, _params: Value) -> ServiceResult {
            self.abort_calls.fetch_add(1, Ordering::SeqCst);
            Ok(self.abort_response.clone())
        }

        async fn history(&self, _params: Value) -> ServiceResult {
            Err("history is not used by this test service".into())
        }

        async fn inject(&self, _params: Value) -> ServiceResult {
            Err("inject is not used by this test service".into())
        }

        async fn clear(&self, _params: Value) -> ServiceResult {
            Err("clear is not used by this test service".into())
        }

        async fn compact(
            &self,
            _request: ChatCompactRequest,
            _context: ChatExecutionContext,
        ) -> ServiceResult {
            Err("compact is not used by this test service".into())
        }

        async fn context(
            &self,
            _request: ChatContextRequest,
            _context: ChatExecutionContext,
        ) -> ServiceResult {
            Err("context is not used by this test service".into())
        }

        async fn raw_prompt(
            &self,
            _request: ChatRawPromptRequest,
            _context: ChatExecutionContext,
        ) -> ServiceResult {
            Err("raw_prompt is not used by this test service".into())
        }

        async fn full_context(
            &self,
            _request: ChatFullContextRequest,
            _context: ChatExecutionContext,
        ) -> ServiceResult {
            Err("full_context is not used by this test service".into())
        }

        async fn active(&self, _params: Value) -> ServiceResult {
            Ok(serde_json::json!({ "active": self.active }))
        }
    }

    fn configured_agent() -> chelix_config::AgentConfig {
        chelix_config::AgentConfig::new(
            "Reviewer",
            "provider::model",
            ReasoningEffort::from("high"),
        )
    }

    async fn sqlite_metadata() -> SqliteSessionMetadata {
        let pool = sqlx::SqlitePool::connect("sqlite::memory:").await.unwrap();
        chelix_projects::run_migrations(&pool).await.unwrap();
        chelix_sessions::run_migrations(&pool).await.unwrap();
        SqliteSessionMetadata::new(pool)
    }

    fn test_model_reasoning() -> chelix_common::ResolvedModelReasoning {
        chelix_common::ResolvedModelReasoning::try_new(
            "provider::model".to_string(),
            ReasoningEffort::from("high"),
        )
        .unwrap_or_else(|error| panic!("valid test pair: {error}"))
    }

    async fn create_parent(metadata: &SqliteSessionMetadata, session_key: &str) {
        metadata
            .create_llm_session(session_key, None, &test_model_reasoning(), Some("main"))
            .await
            .unwrap();
    }

    async fn configure_child(
        metadata: &SqliteSessionMetadata,
        session_key: &str,
        parent_session_key: &str,
    ) {
        metadata
            .create_subagent_session(
                session_key,
                "Reviewer task",
                parent_session_key,
                parent_session_key,
                "reviewer",
                &test_model_reasoning(),
            )
            .await
            .unwrap();
    }

    #[test]
    fn discoverable_agent_requires_non_empty_prompt() {
        let configured = configured_agent();
        discoverable_agent_from_config(
            "reviewer",
            &configured,
            Some("Review the task".to_string()),
        )
        .unwrap();

        let error = discoverable_agent_from_config("reviewer", &configured, None).unwrap_err();
        assert!(error.to_string().contains("no non-empty SUBAGENT.md"));
        let error = discoverable_agent_from_config("reviewer", &configured, Some("  ".to_string()))
            .unwrap_err();
        assert!(error.to_string().contains("no non-empty SUBAGENT.md"));
    }

    #[tokio::test]
    async fn lifecycle_actions_do_not_access_foreign_children() {
        let metadata = sqlite_metadata().await;
        create_parent(&metadata, "session:parent").await;
        create_parent(&metadata, "session:other").await;
        configure_child(&metadata, "session:own-child", "session:parent").await;
        configure_child(&metadata, "session:foreign-child", "session:other").await;
        let chat = LifecycleChatService::new(
            false,
            serde_json::json!({ "aborted": true, "runId": "run-foreign" }),
        );
        let dir = tempfile::tempdir().unwrap();
        let store = SessionStore::new(dir.path().to_path_buf());
        let runs = RwLock::new(HashMap::from([(
            "session:foreign-child".to_string(),
            "run-foreign".to_string(),
        )]));

        let status_error =
            sub_agent_status(&metadata, &chat, "session:parent", "session:foreign-child")
                .await
                .unwrap_err();
        assert!(status_error.to_string().contains("access denied"));

        let listed = sub_agent_list(&metadata, &chat, "session:parent")
            .await
            .unwrap();
        let sessions = listed["sessions"].as_array().unwrap();
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0]["sessionKey"], "session:own-child");

        let result_error = sub_agent_result(
            &metadata,
            &chat,
            &store,
            &runs,
            "session:parent",
            "session:foreign-child",
        )
        .await
        .unwrap_err();
        assert!(result_error.to_string().contains("access denied"));

        let cancel_error =
            sub_agent_cancel(&metadata, &chat, "session:parent", "session:foreign-child")
                .await
                .unwrap_err();
        assert!(cancel_error.to_string().contains("access denied"));
    }

    #[tokio::test]
    async fn background_result_is_extracted_only_from_the_tracked_run() {
        let metadata = sqlite_metadata().await;
        create_parent(&metadata, "session:parent").await;
        configure_child(&metadata, "session:child", "session:parent").await;
        let dir = tempfile::tempdir().unwrap();
        let store = SessionStore::new(dir.path().to_path_buf());
        store
            .append(
                "session:child",
                &serde_json::json!({
                    "role": "assistant",
                    "content": "other run",
                    "run_id": "run-2",
                }),
            )
            .await
            .unwrap();
        store
            .append(
                "session:child",
                &serde_json::json!({
                    "role": "assistant",
                    "content": "tracked draft",
                    "run_id": "run-1",
                }),
            )
            .await
            .unwrap();
        store
            .append(
                "session:child",
                &serde_json::json!({
                    "role": "assistant",
                    "content": "tracked final",
                    "durationMs": 42,
                    "run_id": "run-1",
                }),
            )
            .await
            .unwrap();
        let runs = RwLock::new(HashMap::from([(
            "session:child".to_string(),
            "run-1".to_string(),
        )]));
        let chat = LifecycleChatService::new(false, Value::Null);

        let result = sub_agent_result(
            &metadata,
            &chat,
            &store,
            &runs,
            "session:parent",
            "session:child",
        )
        .await
        .unwrap();

        assert_eq!(
            result,
            serde_json::json!({
                "sessionKey": "session:child",
                "agentId": "reviewer",
                "text": "tracked final",
            })
        );
    }

    #[tokio::test]
    async fn result_requires_an_inactive_chat_run() {
        let metadata = sqlite_metadata().await;
        create_parent(&metadata, "session:parent").await;
        configure_child(&metadata, "session:child", "session:parent").await;
        let dir = tempfile::tempdir().unwrap();
        let store = SessionStore::new(dir.path().to_path_buf());
        let runs = RwLock::new(HashMap::from([(
            "session:child".to_string(),
            "run-1".to_string(),
        )]));
        let active = LifecycleChatService::new(true, Value::Null);

        let error = sub_agent_result(
            &metadata,
            &active,
            &store,
            &runs,
            "session:parent",
            "session:child",
        )
        .await
        .unwrap_err();

        assert!(error.to_string().contains("still running"));
    }

    #[tokio::test]
    async fn cancel_preserves_active_and_completed_abort_results() {
        let metadata = sqlite_metadata().await;
        create_parent(&metadata, "session:parent").await;
        configure_child(&metadata, "session:child", "session:parent").await;
        let active = LifecycleChatService::new(
            true,
            serde_json::json!({ "aborted": true, "runId": "run-1" }),
        );
        assert_eq!(
            sub_agent_cancel(&metadata, &active, "session:parent", "session:child",)
                .await
                .unwrap(),
            serde_json::json!({
                "sessionKey": "session:child",
                "aborted": true,
                "runId": "run-1",
            })
        );
        assert_eq!(active.abort_calls.load(Ordering::SeqCst), 1);

        let completed = LifecycleChatService::new(
            false,
            serde_json::json!({ "aborted": false, "runId": null }),
        );
        assert_eq!(
            sub_agent_cancel(&metadata, &completed, "session:parent", "session:child",)
                .await
                .unwrap(),
            serde_json::json!({
                "sessionKey": "session:child",
                "aborted": false,
                "runId": null,
            })
        );
        assert_eq!(completed.abort_calls.load(Ordering::SeqCst), 1);
    }

    fn badge_test_agents() -> Arc<RwLock<chelix_config::AgentsConfig>> {
        let mut agents = chelix_config::AgentsConfig {
            default: "reviewer".to_string(),
            ..Default::default()
        };
        let mut coder = chelix_config::AgentConfig::new(
            "Coder",
            "provider::model",
            ReasoningEffort::from("high"),
        );
        coder.prepend_sender_badge = true;
        let mut quiet = chelix_config::AgentConfig::new(
            "Quiet",
            "provider::model",
            ReasoningEffort::from("high"),
        );
        quiet.prepend_sender_badge = false;
        let reviewer = configured_agent();
        agents.entries.insert("coder".to_string(), coder);
        agents.entries.insert("quiet".to_string(), quiet);
        agents.entries.insert("reviewer".to_string(), reviewer);
        Arc::new(RwLock::new(agents))
    }

    async fn build_badge_runtime(
        agents: Arc<RwLock<chelix_config::AgentsConfig>>,
        metadata: Arc<SqliteSessionMetadata>,
        store: Arc<SessionStore>,
    ) -> SubAgentRuntime {
        let services = crate::services::GatewayServices::noop().with_agents_config(agents);
        let state = GatewayState::new(crate::auth::resolve_auth(None, None), services);
        SubAgentRuntime {
            state,
            session_store: store,
            session_metadata: metadata,
            background_runs: RwLock::new(HashMap::new()),
        }
    }

    #[tokio::test]
    async fn sender_badge_uses_sender_name_when_enabled() {
        let metadata = Arc::new(sqlite_metadata().await);
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(SessionStore::new(dir.path().to_path_buf()));
        let runtime = build_badge_runtime(badge_test_agents(), metadata, store).await;
        assert_eq!(
            runtime.sender_badge_prefix(Some("coder")).await.unwrap(),
            "[From the \"Coder\" agent]\n\n"
        );
    }

    #[tokio::test]
    async fn sender_badge_empty_when_disabled_or_absent() {
        let metadata = Arc::new(sqlite_metadata().await);
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(SessionStore::new(dir.path().to_path_buf()));
        let runtime = build_badge_runtime(badge_test_agents(), metadata, store).await;
        assert_eq!(
            runtime.sender_badge_prefix(Some("quiet")).await.unwrap(),
            ""
        );
        assert_eq!(runtime.sender_badge_prefix(None).await.unwrap(), "");
    }

    #[tokio::test]
    async fn sender_badge_rejects_unknown_sender() {
        let metadata = Arc::new(sqlite_metadata().await);
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(SessionStore::new(dir.path().to_path_buf()));
        let runtime = build_badge_runtime(badge_test_agents(), metadata, store).await;
        let error = runtime
            .sender_badge_prefix(Some("missing"))
            .await
            .unwrap_err();
        assert!(error.to_string().contains("unknown sender agent"));
    }

    #[test]
    fn effective_task_places_badge_first() {
        let badge = chelix_tools::sessions_communicate::sender_badge("Coder");
        let task = "Do work";
        let effective = if badge.is_empty() {
            task.to_string()
        } else {
            format!("{badge}{task}")
        };
        assert_eq!(effective, "[From the \"Coder\" agent]\n\nDo work");
    }

    struct CapturingChat {
        sync_text: Arc<tokio::sync::Mutex<Option<String>>>,
        async_text: Arc<tokio::sync::Mutex<Option<String>>>,
        sync_called: Arc<std::sync::atomic::AtomicBool>,
        async_called: Arc<std::sync::atomic::AtomicBool>,
    }

    #[async_trait]
    impl ChatService for CapturingChat {
        async fn send(
            &self,
            request: ChatSendRequest,
            _context: ChatExecutionContext,
        ) -> ServiceResult {
            self.async_called.store(true, Ordering::SeqCst);
            let text = match request.message {
                chelix_service_traits::ChatSendMessage::Text(text) => text,
                chelix_service_traits::ChatSendMessage::Content(_) => {
                    return Err("content messages are not used by this test".into());
                },
            };
            *self.async_text.lock().await = Some(text);
            Ok(serde_json::json!({ "runId": "run-test" }))
        }

        async fn send_sync(
            &self,
            request: ChatSendSyncRequest,
            _context: ChatExecutionContext,
        ) -> ServiceResult {
            self.sync_called.store(true, Ordering::SeqCst);
            *self.sync_text.lock().await = Some(request.text);
            Ok(serde_json::json!({
                "text": "done",
                "inputTokens": 1,
                "outputTokens": 1,
                "durationMs": 1,
            }))
        }

        async fn abort(&self, _params: Value) -> ServiceResult {
            Err("abort is not used by this test".into())
        }

        async fn history(&self, _params: Value) -> ServiceResult {
            Err("history is not used by this test".into())
        }

        async fn inject(&self, _params: Value) -> ServiceResult {
            Err("inject is not used by this test".into())
        }

        async fn clear(&self, _params: Value) -> ServiceResult {
            Err("clear is not used by this test".into())
        }

        async fn compact(
            &self,
            _request: ChatCompactRequest,
            _context: ChatExecutionContext,
        ) -> ServiceResult {
            Err("compact is not used by this test".into())
        }

        async fn context(
            &self,
            _request: ChatContextRequest,
            _context: ChatExecutionContext,
        ) -> ServiceResult {
            Err("context is not used by this test".into())
        }

        async fn raw_prompt(
            &self,
            _request: ChatRawPromptRequest,
            _context: ChatExecutionContext,
        ) -> ServiceResult {
            Err("raw_prompt is not used by this test".into())
        }

        async fn full_context(
            &self,
            _request: ChatFullContextRequest,
            _context: ChatExecutionContext,
        ) -> ServiceResult {
            Err("full_context is not used by this test".into())
        }

        async fn active(&self, _params: Value) -> ServiceResult {
            Ok(serde_json::json!({ "active": false }))
        }
    }

    struct AcceptProviderModel;

    #[async_trait]
    impl chelix_service_traits::ModelService for AcceptProviderModel {
        async fn list(&self) -> ServiceResult {
            Ok(serde_json::json!([]))
        }

        async fn list_all(&self) -> ServiceResult {
            Ok(serde_json::json!([]))
        }

        async fn resolve_model_reasoning(
            &self,
            model: &str,
            reasoning_effort: Option<&ReasoningEffort>,
        ) -> Result<
            chelix_service_traits::ResolvedModelReasoning,
            chelix_service_traits::ServiceError,
        > {
            let effort = reasoning_effort.ok_or_else(|| {
                chelix_service_traits::ServiceError::message("reasoning effort is required")
            })?;
            chelix_common::ResolvedModelReasoning::try_new(model.to_string(), effort.clone())
                .map_err(|error| chelix_service_traits::ServiceError::message(error.to_string()))
        }

        async fn disable(&self, _params: Value) -> ServiceResult {
            Ok(serde_json::json!({}))
        }

        async fn enable(&self, _params: Value) -> ServiceResult {
            Ok(serde_json::json!({}))
        }
    }

    struct DataDirTestGuard {
        _lock: std::sync::MutexGuard<'static, ()>,
    }

    impl DataDirTestGuard {
        fn with_reviewer_prompt() -> (Self, tempfile::TempDir) {
            let guard = Self {
                _lock: crate::config_override_test_lock(),
            };
            let data_dir = tempfile::tempdir().unwrap();
            let agent_dir = data_dir.path().join("agents").join("reviewer");
            std::fs::create_dir_all(&agent_dir).unwrap();
            std::fs::write(agent_dir.join("SUBAGENT.md"), "Review").unwrap();
            chelix_config::set_data_dir(data_dir.path().to_path_buf());
            (guard, data_dir)
        }
    }

    impl Drop for DataDirTestGuard {
        fn drop(&mut self) {
            chelix_config::clear_data_dir();
        }
    }

    #[tokio::test]
    async fn sub_agent_run_sends_badged_task_in_both_modes() {
        let (_guard, _data_dir) = DataDirTestGuard::with_reviewer_prompt();
        let metadata = Arc::new(sqlite_metadata().await);
        create_parent(&metadata, "session:parent").await;
        let store_dir = tempfile::tempdir().unwrap();
        let store = Arc::new(SessionStore::new(store_dir.path().to_path_buf()));
        let agents = badge_test_agents();
        let model: Arc<dyn chelix_service_traits::ModelService> = Arc::new(AcceptProviderModel);
        let chat = Arc::new(CapturingChat {
            sync_text: Arc::new(tokio::sync::Mutex::new(None)),
            async_text: Arc::new(tokio::sync::Mutex::new(None)),
            sync_called: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            async_called: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        });
        let services = crate::services::GatewayServices::noop()
            .with_agents_config(Arc::clone(&agents))
            .with_model(model)
            .with_chat(chat.clone());
        let state = GatewayState::new(crate::auth::resolve_auth(None, None), services);
        let runtime = SubAgentRuntime {
            state,
            session_store: Arc::clone(&store),
            session_metadata: Arc::clone(&metadata),
            background_runs: RwLock::new(HashMap::new()),
        };
        // Blocking with badge enabled uses the sender name, not the target name.
        runtime
            .run(
                "session:parent",
                Some("coder"),
                "reviewer",
                "Do work",
                SubAgentMode::Blocking,
            )
            .await
            .unwrap();
        assert_eq!(
            chat.sync_text.lock().await.as_deref(),
            Some("[From the \"Coder\" agent]\n\nDo work")
        );
        // Background with badge enabled.
        runtime
            .run(
                "session:parent",
                Some("coder"),
                "reviewer",
                "Do work",
                SubAgentMode::Background,
            )
            .await
            .unwrap();
        assert_eq!(
            chat.async_text.lock().await.as_deref(),
            Some("[From the \"Coder\" agent]\n\nDo work")
        );
        // Badge disabled sends the raw task even though the target enables it.
        runtime
            .run(
                "session:parent",
                Some("quiet"),
                "reviewer",
                "Do work",
                SubAgentMode::Blocking,
            )
            .await
            .unwrap();
        assert_eq!(chat.sync_text.lock().await.as_deref(), Some("Do work"));
        // No sender sends the raw task.
        runtime
            .run(
                "session:parent",
                None,
                "reviewer",
                "Do work",
                SubAgentMode::Blocking,
            )
            .await
            .unwrap();
        assert_eq!(chat.sync_text.lock().await.as_deref(), Some("Do work"));
        // Unknown sender fails before chat and before creating a child.
        let children_before = metadata
            .list_children_result("session:parent")
            .await
            .unwrap()
            .len();
        chat.sync_called.store(false, Ordering::SeqCst);
        let error = runtime
            .run(
                "session:parent",
                Some("missing"),
                "reviewer",
                "Do work",
                SubAgentMode::Blocking,
            )
            .await
            .unwrap_err();
        assert!(error.to_string().contains("unknown sender agent"));
        assert!(!chat.sync_called.load(Ordering::SeqCst));
        let children_after = metadata
            .list_children_result("session:parent")
            .await
            .unwrap()
            .len();
        assert_eq!(children_before, children_after);
    }
}
