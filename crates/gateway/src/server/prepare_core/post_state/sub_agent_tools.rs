use std::{sync::Arc, time::Instant};

use {
    chelix_agents::tool_registry::ToolRegistry,
    chelix_common::{ProviderSegmentOutcome, ReasoningEffort},
    chelix_service_traits::{ChatExecutionContext, ChatSendRequest, ChatService, SessionTerminal},
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
            SubAgentRequest::Attach {
                parent_session_key,
                session_key,
            } => self.attach(&parent_session_key, &session_key).await,
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
        if let Err(error) = inspect_started_send(&response) {
            record_run_metric(mode, "failed", started.elapsed());
            return Err(error);
        }
        if let Err(error) = ensure_child_started(chat.as_ref(), &session_key).await {
            record_run_metric(mode, "failed", started.elapsed());
            return Err(error);
        }
        record_run_started(mode);

        match mode {
            SubAgentMode::Background => Ok(serde_json::json!({
                "sessionKey": session_key,
                "agentId": agent.id,
                "mode": mode.as_str(),
                "status": "running",
            })),
            SubAgentMode::Blocking => {
                let last_terminal = wait_for_child(chat.as_ref(), &session_key).await?;
                let snapshot =
                    snapshot_from_store(self.session_store.as_ref(), &session_key, last_terminal)
                        .await?;
                let status = snapshot.status;
                match blocking_run_output(&session_key, &agent.id, snapshot) {
                    Ok(output) => {
                        record_run_metric(mode, status.metric_status(), started.elapsed());
                        info!(
                            session_key,
                            agent_id,
                            duration_ms = started.elapsed().as_millis(),
                            "blocking sub-agent run reached a final gate"
                        );
                        Ok(output)
                    },
                    Err(error) => {
                        record_run_metric(mode, "failed", started.elapsed());
                        Err(error)
                    },
                }
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
            self.session_store.as_ref(),
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
            self.session_store.as_ref(),
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
            self.session_store.as_ref(),
            parent_session_key,
            session_key,
        )
        .await
    }

    #[tracing::instrument(
        name = "sub_agent.attach",
        skip_all,
        fields(parent_session_key, session_key)
    )]
    async fn attach(
        &self,
        parent_session_key: &str,
        session_key: &str,
    ) -> chelix_tools::Result<Value> {
        sub_agent_attach(
            &self.session_metadata,
            self.state.chat().as_ref(),
            self.session_store.as_ref(),
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ChildStatus {
    Running,
    Cancelled,
    Completed,
    Idle,
}

impl ChildStatus {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Cancelled => "cancelled",
            Self::Completed => "completed",
            Self::Idle => "idle",
        }
    }

    const fn metric_status(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Cancelled => "cancelled",
            Self::Completed => "completed",
            Self::Idle => "failed",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ChildSnapshot {
    status: ChildStatus,
    text: Option<String>,
    last_terminal: Option<SessionTerminal>,
}

fn status_from_runtime_terminal(terminal: SessionTerminal) -> ChildStatus {
    match terminal {
        SessionTerminal::Cancelled => ChildStatus::Cancelled,
        SessionTerminal::Completed => ChildStatus::Completed,
        SessionTerminal::Failed => ChildStatus::Idle,
    }
}

async fn sub_agent_status(
    metadata: &SqliteSessionMetadata,
    chat: &dyn ChatService,
    session_store: &SessionStore,
    parent_session_key: &str,
    session_key: &str,
) -> chelix_tools::Result<Value> {
    let entry = owned_child_entry(metadata, parent_session_key, session_key).await?;
    status_payload(chat, session_store, entry).await
}

async fn sub_agent_list(
    metadata: &SqliteSessionMetadata,
    chat: &dyn ChatService,
    session_store: &SessionStore,
    parent_session_key: &str,
) -> chelix_tools::Result<Value> {
    let entries = metadata
        .list_children_result(parent_session_key)
        .await
        .map_err(tool_error)?;
    let mut sessions = Vec::with_capacity(entries.len());
    for entry in entries {
        sessions.push(status_payload(chat, session_store, entry).await?);
    }
    Ok(serde_json::json!({ "sessions": sessions }))
}

async fn sub_agent_result(
    metadata: &SqliteSessionMetadata,
    chat: &dyn ChatService,
    session_store: &SessionStore,
    parent_session_key: &str,
    session_key: &str,
) -> chelix_tools::Result<Value> {
    let entry = owned_child_entry(metadata, parent_session_key, session_key).await?;
    let snapshot = child_snapshot(chat, session_store, session_key).await?;
    Ok(result_output(
        session_key,
        entry.agent_id.as_deref(),
        snapshot,
    ))
}

async fn sub_agent_attach(
    metadata: &SqliteSessionMetadata,
    chat: &dyn ChatService,
    session_store: &SessionStore,
    parent_session_key: &str,
    session_key: &str,
) -> chelix_tools::Result<Value> {
    let entry = owned_child_entry(metadata, parent_session_key, session_key).await?;
    let last_terminal = if chat_active(chat, session_key).await? {
        wait_for_child(chat, session_key).await?
    } else {
        chat.session_terminal(session_key)
            .await
            .map_err(|error| chelix_tools::Error::message(error.to_string()))?
    };
    let snapshot = snapshot_from_store(session_store, session_key, last_terminal).await?;
    Ok(result_output(
        session_key,
        entry.agent_id.as_deref(),
        snapshot,
    ))
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
    session_store: &SessionStore,
    entry: SessionEntry,
) -> chelix_tools::Result<Value> {
    let status = child_status(chat, session_store, &entry.key).await?;
    Ok(serde_json::json!({
        "sessionKey": entry.key,
        "agentId": entry.agent_id,
        "label": entry.label,
        "status": status.as_str(),
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

fn child_turn_started(active: bool, last_terminal: Option<SessionTerminal>) -> bool {
    active || last_terminal.is_some()
}

#[tracing::instrument(skip_all, fields(session_key))]
async fn ensure_child_started(
    chat: &dyn ChatService,
    session_key: &str,
) -> chelix_tools::Result<()> {
    let active = chat_active(chat, session_key).await?;
    let last_terminal = chat
        .session_terminal(session_key)
        .await
        .map_err(|error| chelix_tools::Error::message(error.to_string()))?;
    if child_turn_started(active, last_terminal) {
        return Ok(());
    }
    Err(chelix_tools::Error::message(format!(
        "chat.send did not start the sub-agent turn for session {session_key:?}"
    )))
}

#[tracing::instrument(skip_all, fields(session_key))]
async fn wait_for_child(
    chat: &dyn ChatService,
    session_key: &str,
) -> chelix_tools::Result<Option<SessionTerminal>> {
    chat.wait_for_session_gate(session_key)
        .await
        .map_err(|error| chelix_tools::Error::message(error.to_string()))
}

async fn snapshot_from_store(
    session_store: &SessionStore,
    session_key: &str,
    last_terminal: Option<SessionTerminal>,
) -> chelix_tools::Result<ChildSnapshot> {
    let messages = session_store
        .read_typed(session_key)
        .await
        .map_err(tool_error)?;
    Ok(snapshot_from_messages(&messages, last_terminal))
}

async fn child_status(
    chat: &dyn ChatService,
    session_store: &SessionStore,
    session_key: &str,
) -> chelix_tools::Result<ChildStatus> {
    if chat_active(chat, session_key).await? {
        return Ok(ChildStatus::Running);
    }
    let last_terminal = chat
        .session_terminal(session_key)
        .await
        .map_err(|error| chelix_tools::Error::message(error.to_string()))?;
    if let Some(terminal) = last_terminal {
        return Ok(status_from_runtime_terminal(terminal));
    }
    let messages = session_store
        .read_typed(session_key)
        .await
        .map_err(tool_error)?;
    Ok(snapshot_from_messages(&messages, None).status)
}

async fn child_snapshot(
    chat: &dyn ChatService,
    session_store: &SessionStore,
    session_key: &str,
) -> chelix_tools::Result<ChildSnapshot> {
    if chat_active(chat, session_key).await? {
        return Ok(ChildSnapshot {
            status: ChildStatus::Running,
            text: None,
            last_terminal: None,
        });
    }
    let last_terminal = chat
        .session_terminal(session_key)
        .await
        .map_err(|error| chelix_tools::Error::message(error.to_string()))?;
    match last_terminal {
        Some(SessionTerminal::Cancelled) => {
            return Ok(ChildSnapshot {
                status: ChildStatus::Cancelled,
                text: None,
                last_terminal,
            });
        },
        Some(SessionTerminal::Failed) => {
            return Ok(ChildSnapshot {
                status: ChildStatus::Idle,
                text: None,
                last_terminal,
            });
        },
        Some(SessionTerminal::Completed) | None => {},
    }
    let messages = session_store
        .read_typed(session_key)
        .await
        .map_err(tool_error)?;
    Ok(snapshot_from_messages(&messages, last_terminal))
}

fn snapshot_from_messages(
    messages: &[PersistedMessage],
    last_terminal: Option<SessionTerminal>,
) -> ChildSnapshot {
    if let Some(terminal) = last_terminal {
        let status = status_from_runtime_terminal(terminal);
        let text = (status == ChildStatus::Completed).then(|| completed_gate_text(messages));
        return ChildSnapshot {
            status,
            text,
            last_terminal,
        };
    }
    let after_last_user = messages_after_last_user(messages);
    if after_last_user.iter().any(|message| match message {
        PersistedMessage::ToolLifecycle { lifecycle } => lifecycle.update.is_user_stop(),
        _ => false,
    }) {
        return ChildSnapshot {
            status: ChildStatus::Cancelled,
            text: None,
            last_terminal,
        };
    }
    let last_close = after_last_user
        .iter()
        .enumerate()
        .rev()
        .find_map(|(index, message)| match message {
            PersistedMessage::ProviderSegmentClose { outcome, .. } => Some((index, *outcome)),
            _ => None,
        });
    match last_close {
        Some((_, ProviderSegmentOutcome::Cancelled)) => ChildSnapshot {
            status: ChildStatus::Cancelled,
            text: None,
            last_terminal,
        },
        Some((index, ProviderSegmentOutcome::Completed)) => {
            if after_last_user[index + 1..]
                .iter()
                .any(|message| matches!(message, PersistedMessage::ToolLifecycle { .. }))
            {
                ChildSnapshot {
                    status: ChildStatus::Idle,
                    text: None,
                    last_terminal,
                }
            } else {
                ChildSnapshot {
                    status: ChildStatus::Completed,
                    text: Some(completed_gate_text(messages)),
                    last_terminal,
                }
            }
        },
        _ => ChildSnapshot {
            status: ChildStatus::Idle,
            text: None,
            last_terminal,
        },
    }
}

fn messages_after_last_user(messages: &[PersistedMessage]) -> &[PersistedMessage] {
    match messages
        .iter()
        .rposition(|message| matches!(message, PersistedMessage::User { .. }))
    {
        Some(index) => &messages[index + 1..],
        None => &[],
    }
}

fn messages_before_trailing_users(messages: &[PersistedMessage]) -> &[PersistedMessage] {
    match messages
        .iter()
        .rposition(|message| !matches!(message, PersistedMessage::User { .. }))
    {
        Some(index) => &messages[..=index],
        None => &[],
    }
}

fn completed_gate_text(messages: &[PersistedMessage]) -> String {
    last_assistant_text(messages_after_last_user(messages))
        .or_else(|| last_assistant_text(messages_before_trailing_users(messages)))
        .unwrap_or_default()
}

fn last_assistant_text(messages: &[PersistedMessage]) -> Option<String> {
    messages.iter().rev().find_map(|message| match message {
        PersistedMessage::Assistant { content, .. } => Some(content.clone()),
        _ => None,
    })
}

fn result_output(session_key: &str, agent_id: Option<&str>, snapshot: ChildSnapshot) -> Value {
    let mut output = serde_json::json!({
        "sessionKey": session_key,
        "agentId": agent_id,
        "status": snapshot.status.as_str(),
    });
    if snapshot.status == ChildStatus::Completed {
        output["text"] = Value::String(snapshot.text.unwrap_or_default());
    }
    output
}

fn blocking_run_output(
    session_key: &str,
    agent_id: &str,
    snapshot: ChildSnapshot,
) -> chelix_tools::Result<Value> {
    match snapshot.status {
        ChildStatus::Completed | ChildStatus::Cancelled => {
            let mut output = result_output(session_key, Some(agent_id), snapshot);
            output["mode"] = Value::String(SubAgentMode::Blocking.as_str().to_string());
            Ok(output)
        },
        ChildStatus::Idle if snapshot.last_terminal == Some(SessionTerminal::Failed) => Err(
            chelix_tools::Error::message(format!("sub-agent session {session_key:?} failed")),
        ),
        ChildStatus::Idle => Err(chelix_tools::Error::message(format!(
            "sub-agent session {session_key:?} finished without a final gate"
        ))),
        ChildStatus::Running => Err(chelix_tools::Error::message(format!(
            "sub-agent session {session_key:?} is still running after wait"
        ))),
    }
}

fn inspect_started_send(response: &Value) -> chelix_tools::Result<()> {
    if response.get("queued").and_then(Value::as_bool) == Some(true) {
        return Err(chelix_tools::Error::message(
            "chat.send queued the sub-agent turn instead of starting it",
        ));
    }
    if response.get("rejected").and_then(Value::as_bool) == Some(true) {
        return Err(chelix_tools::Error::message(
            "chat.send rejected the sub-agent turn",
        ));
    }
    if response.get("ok").and_then(Value::as_bool) == Some(false) {
        return Err(chelix_tools::Error::message(
            "chat.send did not start the sub-agent turn",
        ));
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
    Ok(serde_json::json!({
        "sessionKey": session_key,
        "aborted": aborted,
    }))
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
fn record_run_started(mode: SubAgentMode) {
    use chelix_metrics::{counter, labels};

    counter!(
        chelix_metrics::sub_agent::RUNS_TOTAL,
        labels::MODE => mode.as_str(),
        labels::STATUS => "running"
    )
    .increment(1);
}

#[cfg(not(feature = "metrics"))]
fn record_run_started(_mode: SubAgentMode) {}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::unwrap_used)]

    use {
        super::*,
        async_trait::async_trait,
        chelix_service_traits::{
            ChatCompactRequest, ChatContextRequest, ChatExecutionContext, ChatFullContextRequest,
            ChatRawPromptRequest, ChatSendRequest, ChatSendSyncRequest, ServiceResult,
            SessionTerminal,
        },
        std::sync::atomic::{AtomicBool, AtomicUsize, Ordering},
        tokio::sync::RwLock,
    };

    struct LifecycleChatService {
        active: bool,
        abort_response: Value,
        abort_calls: AtomicUsize,
        last_terminal: Option<SessionTerminal>,
    }

    impl LifecycleChatService {
        fn new(active: bool, abort_response: Value) -> Self {
            Self {
                active,
                abort_response,
                abort_calls: AtomicUsize::new(0),
                last_terminal: None,
            }
        }

        fn with_terminal(mut self, terminal: SessionTerminal) -> Self {
            self.last_terminal = Some(terminal);
            self
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

        async fn session_terminal(
            &self,
            _session_key: &str,
        ) -> Result<Option<SessionTerminal>, chelix_service_traits::ServiceError> {
            Ok(self.last_terminal)
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

    fn persisted(value: Value) -> PersistedMessage {
        serde_json::from_value(value)
            .unwrap_or_else(|error| panic!("valid persisted message: {error}"))
    }

    fn user_stop_lifecycle() -> Value {
        // SessionStore.append projects UI history and requires runId on tool_lifecycle.
        serde_json::json!({
            "role": "tool_lifecycle",
            "toolCallId": "call-1",
            "toolName": "execute_command",
            "sequence": 1,
            "emittedAtMs": 1,
            "runId": "run-1",
            "stage": "cancelled",
            "reason": chelix_common::tool_lifecycle::AGENT_RUN_CANCELLED_REASON,
        })
    }

    async fn append_completed_turn(
        store: &SessionStore,
        session_key: &str,
        user: &str,
        assistant: &str,
    ) {
        store
            .append(
                session_key,
                &serde_json::json!({ "role": "user", "content": user }),
            )
            .await
            .unwrap();
        store
            .append(
                session_key,
                &serde_json::json!({ "role": "assistant", "content": assistant }),
            )
            .await
            .unwrap();
        store
            .append(
                session_key,
                &serde_json::json!({
                    "role": "provider_segment_close",
                    "segmentId": "seg-final",
                    "outcome": "completed",
                }),
            )
            .await
            .unwrap();
    }

    #[test]
    fn snapshot_user_stop_after_completed_close_is_cancelled() {
        let snapshot = snapshot_from_messages(
            &[
                persisted(serde_json::json!({ "role": "user", "content": "go" })),
                persisted(serde_json::json!({
                    "role": "assistant",
                    "content": "partial",
                })),
                persisted(serde_json::json!({
                    "role": "provider_segment_close",
                    "segmentId": "seg-1",
                    "outcome": "completed",
                })),
                persisted(user_stop_lifecycle()),
            ],
            None,
        );
        assert_eq!(snapshot.status, ChildStatus::Cancelled);
    }

    #[test]
    fn snapshot_retry_cancelled_then_completed_is_completed() {
        let snapshot = snapshot_from_messages(
            &[
                persisted(serde_json::json!({ "role": "user", "content": "go" })),
                persisted(serde_json::json!({
                    "role": "provider_segment_close",
                    "segmentId": "seg-retry",
                    "outcome": "cancelled",
                })),
                persisted(serde_json::json!({
                    "role": "assistant",
                    "content": "final answer",
                })),
                persisted(serde_json::json!({
                    "role": "provider_segment_close",
                    "segmentId": "seg-final",
                    "outcome": "completed",
                })),
            ],
            None,
        );
        assert_eq!(snapshot.status, ChildStatus::Completed);
        assert_eq!(snapshot.text.as_deref(), Some("final answer"));
    }

    #[test]
    fn snapshot_empty_history_is_idle() {
        let snapshot = snapshot_from_messages(&[], None);
        assert_eq!(snapshot.status, ChildStatus::Idle);
        assert_eq!(snapshot.text, None);
    }

    fn executing_lifecycle() -> Value {
        serde_json::json!({
            "role": "tool_lifecycle",
            "toolCallId": "call-1",
            "toolName": "execute_command",
            "sequence": 1,
            "emittedAtMs": 1,
            "stage": "executing",
            "arguments": {},
            "startedAtMs": 1,
        })
    }

    fn assistant_with_tool_calls(content: &str) -> Value {
        serde_json::json!({
            "role": "assistant",
            "content": content,
            "tool_calls": [{
                "id": "call-1",
                "type": "function",
                "function": { "name": "execute_command", "arguments": "{}" }
            }],
        })
    }

    #[test]
    fn snapshot_tool_lifecycle_after_completed_close_is_idle() {
        let snapshot = snapshot_from_messages(
            &[
                persisted(serde_json::json!({ "role": "user", "content": "go" })),
                persisted(assistant_with_tool_calls("checking")),
                persisted(serde_json::json!({
                    "role": "provider_segment_close",
                    "segmentId": "seg-1",
                    "outcome": "completed",
                })),
                persisted(executing_lifecycle()),
            ],
            None,
        );
        assert_eq!(snapshot.status, ChildStatus::Idle);
        assert_eq!(snapshot.text, None);
    }

    #[test]
    fn snapshot_finalized_tool_call_segment_without_later_lifecycle_is_completed() {
        let snapshot = snapshot_from_messages(
            &[
                persisted(serde_json::json!({ "role": "user", "content": "go" })),
                persisted(assistant_with_tool_calls("final from tool segment")),
                persisted(serde_json::json!({
                    "role": "provider_segment_close",
                    "segmentId": "seg-final",
                    "outcome": "completed",
                })),
            ],
            None,
        );
        assert_eq!(snapshot.status, ChildStatus::Completed);
        assert_eq!(snapshot.text.as_deref(), Some("final from tool segment"));
    }

    #[test]
    fn snapshot_completed_text_skips_trailing_user_batch() {
        let snapshot = snapshot_from_messages(
            &[
                persisted(serde_json::json!({ "role": "user", "content": "first" })),
                persisted(serde_json::json!({
                    "role": "assistant",
                    "content": "gate text",
                })),
                persisted(serde_json::json!({
                    "role": "provider_segment_close",
                    "segmentId": "seg-final",
                    "outcome": "completed",
                })),
                persisted(serde_json::json!({ "role": "user", "content": "queued-1" })),
                persisted(serde_json::json!({ "role": "user", "content": "queued-2" })),
            ],
            Some(SessionTerminal::Completed),
        );
        assert_eq!(snapshot.status, ChildStatus::Completed);
        assert_eq!(snapshot.text.as_deref(), Some("gate text"));
    }

    #[test]
    fn snapshot_failed_terminal_is_idle() {
        let snapshot = snapshot_from_messages(
            &[persisted(
                serde_json::json!({ "role": "user", "content": "go" }),
            )],
            Some(SessionTerminal::Failed),
        );
        assert_eq!(snapshot.status, ChildStatus::Idle);
    }

    fn completed_close_after_user(assistant: &str) -> Vec<PersistedMessage> {
        vec![
            persisted(serde_json::json!({ "role": "user", "content": "go" })),
            persisted(serde_json::json!({
                "role": "assistant",
                "content": assistant,
            })),
            persisted(serde_json::json!({
                "role": "provider_segment_close",
                "segmentId": "seg-1",
                "outcome": "completed",
            })),
        ]
    }

    #[test]
    fn snapshot_runtime_cancelled_wins_over_completed_close() {
        let snapshot = snapshot_from_messages(
            &completed_close_after_user("partial"),
            Some(SessionTerminal::Cancelled),
        );
        assert_eq!(snapshot.status, ChildStatus::Cancelled);
        assert_eq!(snapshot.text, None);
    }

    #[test]
    fn snapshot_runtime_failed_wins_over_completed_close() {
        let snapshot = snapshot_from_messages(
            &completed_close_after_user("partial"),
            Some(SessionTerminal::Failed),
        );
        assert_eq!(snapshot.status, ChildStatus::Idle);
        assert_eq!(snapshot.text, None);
    }

    #[test]
    fn blocking_run_output_distinguishes_failed_from_missing_gate() {
        let failed = blocking_run_output("session:child", "reviewer", ChildSnapshot {
            status: ChildStatus::Idle,
            text: None,
            last_terminal: Some(SessionTerminal::Failed),
        })
        .unwrap_err();
        assert!(failed.to_string().contains("failed"));
        assert!(!failed.to_string().contains("without a final gate"));

        let missing = blocking_run_output("session:child", "reviewer", ChildSnapshot {
            status: ChildStatus::Idle,
            text: None,
            last_terminal: None,
        })
        .unwrap_err();
        assert!(missing.to_string().contains("without a final gate"));
    }

    #[test]
    fn inspect_started_send_rejects_queued_and_rejected_turns() {
        inspect_started_send(&serde_json::json!({ "ok": true, "queued": true })).unwrap_err();
        inspect_started_send(&serde_json::json!({ "ok": false, "rejected": true })).unwrap_err();
        inspect_started_send(&serde_json::json!({ "ok": false })).unwrap_err();
        inspect_started_send(&serde_json::json!({ "ok": true })).unwrap();
    }

    #[test]
    fn child_turn_started_requires_active_or_terminal() {
        assert!(!child_turn_started(false, None));
        assert!(child_turn_started(true, None));
        assert!(child_turn_started(false, Some(SessionTerminal::Completed)));
        assert!(child_turn_started(false, Some(SessionTerminal::Failed)));
    }

    #[test]
    fn blocking_run_output_returns_completed_text() {
        let output = blocking_run_output("session:child", "reviewer", ChildSnapshot {
            status: ChildStatus::Completed,
            text: Some("final answer".to_string()),
            last_terminal: Some(SessionTerminal::Completed),
        })
        .unwrap();
        assert_eq!(
            output,
            serde_json::json!({
                "sessionKey": "session:child",
                "agentId": "reviewer",
                "status": "completed",
                "text": "final answer",
                "mode": "blocking",
            })
        );
    }

    #[tokio::test]
    async fn ensure_child_started_rejects_unstarted_send() {
        let chat = LifecycleChatService::new(false, Value::Null);
        let error = ensure_child_started(&chat, "session:child")
            .await
            .unwrap_err();
        assert!(error.to_string().contains("did not start"));
        assert!(error.to_string().contains("session:child"));
    }

    #[tokio::test]
    async fn ensure_child_started_accepts_finished_terminal() {
        let chat =
            LifecycleChatService::new(false, Value::Null).with_terminal(SessionTerminal::Completed);
        ensure_child_started(&chat, "session:child").await.unwrap();
    }

    #[tokio::test]
    async fn lifecycle_actions_do_not_access_foreign_children() {
        let metadata = sqlite_metadata().await;
        create_parent(&metadata, "session:parent").await;
        create_parent(&metadata, "session:other").await;
        configure_child(&metadata, "session:own-child", "session:parent").await;
        configure_child(&metadata, "session:foreign-child", "session:other").await;
        let chat = LifecycleChatService::new(false, serde_json::json!({ "aborted": true }));
        let dir = tempfile::tempdir().unwrap();
        let store = SessionStore::new(dir.path().to_path_buf());

        let status_error = sub_agent_status(
            &metadata,
            &chat,
            &store,
            "session:parent",
            "session:foreign-child",
        )
        .await
        .unwrap_err();
        assert!(status_error.to_string().contains("access denied"));

        let listed = sub_agent_list(&metadata, &chat, &store, "session:parent")
            .await
            .unwrap();
        let sessions = listed["sessions"].as_array().unwrap();
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0]["sessionKey"], "session:own-child");

        let result_error = sub_agent_result(
            &metadata,
            &chat,
            &store,
            "session:parent",
            "session:foreign-child",
        )
        .await
        .unwrap_err();
        assert!(result_error.to_string().contains("access denied"));

        let attach_error = sub_agent_attach(
            &metadata,
            &chat,
            &store,
            "session:parent",
            "session:foreign-child",
        )
        .await
        .unwrap_err();
        assert!(attach_error.to_string().contains("access denied"));

        let cancel_error =
            sub_agent_cancel(&metadata, &chat, "session:parent", "session:foreign-child")
                .await
                .unwrap_err();
        assert!(cancel_error.to_string().contains("access denied"));
    }

    #[tokio::test]
    async fn result_returns_last_assistant_after_last_user_for_any_mode() {
        let metadata = sqlite_metadata().await;
        create_parent(&metadata, "session:parent").await;
        configure_child(&metadata, "session:child", "session:parent").await;
        let dir = tempfile::tempdir().unwrap();
        let store = SessionStore::new(dir.path().to_path_buf());
        append_completed_turn(&store, "session:child", "previous", "stale").await;
        append_completed_turn(&store, "session:child", "current", "latest final").await;
        let chat = LifecycleChatService::new(false, Value::Null);

        let result = sub_agent_result(&metadata, &chat, &store, "session:parent", "session:child")
            .await
            .unwrap();

        assert_eq!(
            result,
            serde_json::json!({
                "sessionKey": "session:child",
                "agentId": "reviewer",
                "status": "completed",
                "text": "latest final",
            })
        );
    }

    #[tokio::test]
    async fn result_reports_running_instead_of_error() {
        let metadata = sqlite_metadata().await;
        create_parent(&metadata, "session:parent").await;
        configure_child(&metadata, "session:child", "session:parent").await;
        let dir = tempfile::tempdir().unwrap();
        let store = SessionStore::new(dir.path().to_path_buf());
        let active = LifecycleChatService::new(true, Value::Null);

        let result = sub_agent_result(
            &metadata,
            &active,
            &store,
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
                "status": "running",
            })
        );
    }

    #[tokio::test]
    async fn result_reports_cancelled_on_user_stop() {
        let metadata = sqlite_metadata().await;
        create_parent(&metadata, "session:parent").await;
        configure_child(&metadata, "session:child", "session:parent").await;
        let dir = tempfile::tempdir().unwrap();
        let store = SessionStore::new(dir.path().to_path_buf());
        store
            .append(
                "session:child",
                &serde_json::json!({ "role": "user", "content": "go" }),
            )
            .await
            .unwrap();
        store
            .append(
                "session:child",
                &serde_json::json!({
                    "role": "provider_segment_close",
                    "segmentId": "seg-1",
                    "outcome": "completed",
                }),
            )
            .await
            .unwrap();
        store
            .append("session:child", &user_stop_lifecycle())
            .await
            .unwrap();
        let chat = LifecycleChatService::new(false, Value::Null);

        let result = sub_agent_result(&metadata, &chat, &store, "session:parent", "session:child")
            .await
            .unwrap();
        assert_eq!(result["status"], "cancelled");
    }

    #[tokio::test]
    async fn result_runtime_failed_is_idle_despite_completed_close() {
        let metadata = sqlite_metadata().await;
        create_parent(&metadata, "session:parent").await;
        configure_child(&metadata, "session:child", "session:parent").await;
        let dir = tempfile::tempdir().unwrap();
        let store = SessionStore::new(dir.path().to_path_buf());
        append_completed_turn(&store, "session:child", "task", "partial").await;
        let chat =
            LifecycleChatService::new(false, Value::Null).with_terminal(SessionTerminal::Failed);

        let result = sub_agent_result(&metadata, &chat, &store, "session:parent", "session:child")
            .await
            .unwrap();
        assert_eq!(
            result,
            serde_json::json!({
                "sessionKey": "session:child",
                "agentId": "reviewer",
                "status": "idle",
            })
        );
    }

    #[tokio::test]
    async fn status_uses_runtime_terminal_without_history() {
        let metadata = sqlite_metadata().await;
        create_parent(&metadata, "session:parent").await;
        configure_child(&metadata, "session:child", "session:parent").await;
        let dir = tempfile::tempdir().unwrap();
        let store = SessionStore::new(dir.path().to_path_buf());
        let chat =
            LifecycleChatService::new(false, Value::Null).with_terminal(SessionTerminal::Completed);

        let status = sub_agent_status(&metadata, &chat, &store, "session:parent", "session:child")
            .await
            .unwrap();
        assert_eq!(status["status"], "completed");
    }

    #[tokio::test]
    async fn attach_returns_last_gate_when_idle() {
        let metadata = sqlite_metadata().await;
        create_parent(&metadata, "session:parent").await;
        configure_child(&metadata, "session:child", "session:parent").await;
        let dir = tempfile::tempdir().unwrap();
        let store = SessionStore::new(dir.path().to_path_buf());
        append_completed_turn(&store, "session:child", "task", "done").await;
        let chat = LifecycleChatService::new(false, Value::Null);

        let result = sub_agent_attach(&metadata, &chat, &store, "session:parent", "session:child")
            .await
            .unwrap();
        assert_eq!(result["status"], "completed");
        assert_eq!(result["text"], "done");
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
            })
        );
        assert_eq!(active.abort_calls.load(Ordering::SeqCst), 1);

        let completed = LifecycleChatService::new(false, serde_json::json!({ "aborted": false }));
        assert_eq!(
            sub_agent_cancel(&metadata, &completed, "session:parent", "session:child",)
                .await
                .unwrap(),
            serde_json::json!({
                "sessionKey": "session:child",
                "aborted": false,
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
        sent_text: Arc<tokio::sync::Mutex<Option<String>>>,
        send_called: Arc<AtomicBool>,
        send_sync_called: Arc<AtomicBool>,
    }

    #[async_trait]
    impl ChatService for CapturingChat {
        async fn send(
            &self,
            request: ChatSendRequest,
            _context: ChatExecutionContext,
        ) -> ServiceResult {
            self.send_called.store(true, Ordering::SeqCst);
            let text = match request.message {
                chelix_service_traits::ChatSendMessage::Text(text) => text,
                chelix_service_traits::ChatSendMessage::Content(_) => {
                    return Err("content messages are not used by this test".into());
                },
            };
            *self.sent_text.lock().await = Some(text);
            Ok(serde_json::json!({ "ok": true }))
        }

        async fn send_sync(
            &self,
            _request: ChatSendSyncRequest,
            _context: ChatExecutionContext,
        ) -> ServiceResult {
            self.send_sync_called.store(true, Ordering::SeqCst);
            Err("send_sync must not be used by sub_agent run".into())
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

        async fn wait_for_session_gate(
            &self,
            _session_key: &str,
        ) -> Result<Option<SessionTerminal>, chelix_service_traits::ServiceError> {
            Ok(Some(SessionTerminal::Completed))
        }

        async fn session_terminal(
            &self,
            _session_key: &str,
        ) -> Result<Option<SessionTerminal>, chelix_service_traits::ServiceError> {
            Ok(Some(SessionTerminal::Completed))
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
            sent_text: Arc::new(tokio::sync::Mutex::new(None)),
            send_called: Arc::new(AtomicBool::new(false)),
            send_sync_called: Arc::new(AtomicBool::new(false)),
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
        };
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
            chat.sent_text.lock().await.as_deref(),
            Some("[From the \"Coder\" agent]\n\nDo work")
        );
        assert!(chat.send_called.load(Ordering::SeqCst));
        assert!(!chat.send_sync_called.load(Ordering::SeqCst));
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
            chat.sent_text.lock().await.as_deref(),
            Some("[From the \"Coder\" agent]\n\nDo work")
        );
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
        assert_eq!(chat.sent_text.lock().await.as_deref(), Some("Do work"));
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
        assert_eq!(chat.sent_text.lock().await.as_deref(), Some("Do work"));
        let children_before = metadata
            .list_children_result("session:parent")
            .await
            .unwrap()
            .len();
        chat.send_called.store(false, Ordering::SeqCst);
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
        assert!(!chat.send_called.load(Ordering::SeqCst));
        let children_after = metadata
            .list_children_result("session:parent")
            .await
            .unwrap()
            .len();
        assert_eq!(children_before, children_after);
        assert!(!chat.send_sync_called.load(Ordering::SeqCst));
    }

    struct WaitingChat {
        active: AtomicBool,
        release: tokio::sync::Mutex<Option<tokio::sync::oneshot::Receiver<()>>>,
    }

    #[async_trait]
    impl ChatService for WaitingChat {
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
            Ok(serde_json::json!({ "active": self.active.load(Ordering::SeqCst) }))
        }

        async fn wait_for_session_gate(
            &self,
            _session_key: &str,
        ) -> Result<Option<SessionTerminal>, chelix_service_traits::ServiceError> {
            if let Some(release) = self.release.lock().await.take() {
                let _ = release.await;
            }
            self.active.store(false, Ordering::SeqCst);
            Ok(Some(SessionTerminal::Completed))
        }

        async fn session_terminal(
            &self,
            _session_key: &str,
        ) -> Result<Option<SessionTerminal>, chelix_service_traits::ServiceError> {
            if self.active.load(Ordering::SeqCst) {
                Ok(None)
            } else {
                Ok(Some(SessionTerminal::Completed))
            }
        }
    }

    #[tokio::test]
    async fn attach_waits_for_the_current_execution() {
        let metadata = sqlite_metadata().await;
        create_parent(&metadata, "session:parent").await;
        configure_child(&metadata, "session:child", "session:parent").await;
        let dir = tempfile::tempdir().unwrap();
        let store = SessionStore::new(dir.path().to_path_buf());
        append_completed_turn(&store, "session:child", "task", "attached final").await;
        let (tx, rx) = tokio::sync::oneshot::channel();
        let chat = WaitingChat {
            active: AtomicBool::new(true),
            release: tokio::sync::Mutex::new(Some(rx)),
        };
        let attach = sub_agent_attach(&metadata, &chat, &store, "session:parent", "session:child");
        tokio::pin!(attach);
        tokio::select! {
            biased;
            result = &mut attach => {
                panic!("attach returned before the current execution finished: {result:?}");
            },
            () = tokio::task::yield_now() => {}
        }
        tx.send(()).expect("attach wait is subscribed");
        let result = attach.await.unwrap();
        assert_eq!(result["status"], "completed");
        assert_eq!(result["text"], "attached final");
    }
}
