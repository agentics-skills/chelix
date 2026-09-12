use std::{collections::HashMap, sync::Arc, time::SystemTime};

use {
    async_trait::async_trait,
    chelix_common::{
        ItemPositionAllocator, ProviderItemId, ProviderItemUpdate, ProviderItemUpdatePayload,
        ProviderSegmentId, ProviderSegmentMaterializer, ProviderSegmentOutcome,
    },
    chelix_config::schema::ExternalAgentsConfig,
    chelix_external_agents::{
        AcpPermissionHandler, AcpPermissionOptionKind, AcpPermissionRequest, AgentTransportKind,
        ContextSnapshot, ExternalAgentEvent, ExternalAgentRegistry, ExternalAgentSession,
        ExternalAgentSpec,
        runtimes::{acp::AcpTransport, claude_code::ClaudeCodeTransport, codex::CodexTransport},
        types::ContextTurn,
    },
    chelix_service_traits::{
        ChatCompactRequest, ChatContextRequest, ChatExecutionContext, ChatFullContextRequest,
        ChatRawPromptRequest, ChatSendMessage, ChatSendRequest, ChatSendSyncRequest, ChatService,
        ExternalAgentService, ModelService, ServiceError, ServiceResult, SessionBusyReason,
        SessionService, SessionTerminal,
    },
    chelix_sessions::{MessageContent, PersistedMessage, QueuedPromptChannelMetadata},
    futures::StreamExt,
    serde_json::Value,
    tokio::sync::Mutex,
    tracing::warn,
};

use chelix_tools::approval::{ApprovalDecision, ApprovalManager};

use crate::{broadcast::BroadcastOpts, state::GatewayState};

/// Identity of the visible message item synthesized for external-agent runs.
///
/// External agents report no output items, so the adapter assigns this identity
/// once per segment. All visible text of the segment belongs to it.
const EXTERNAL_AGENT_MESSAGE_ITEM_ID: &str = "msg_0";

/// Identity of the reasoning item synthesized for external-agent runs.
const EXTERNAL_AGENT_REASONING_ITEM_ID: &str = "rs_0";

pub struct GatewayExternalAgentService {
    registry: ExternalAgentRegistry,
    config: ExternalAgentsConfig,
    session_metadata: Arc<chelix_sessions::metadata::SqliteSessionMetadata>,
    agents_config: Arc<tokio::sync::RwLock<chelix_config::AgentsConfig>>,
    model_service: Arc<dyn ModelService>,
    live_sessions: Mutex<HashMap<LiveSessionKey, LiveSessionEntry>>,
}

type LiveExternalAgentSession = Arc<Mutex<Box<dyn ExternalAgentSession>>>;

const LIVE_SESSION_IDLE_TTL: std::time::Duration = std::time::Duration::from_secs(60 * 60);

struct LiveSessionEntry {
    session: LiveExternalAgentSession,
    last_used: std::time::Instant,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct LiveSessionKey {
    session_key: String,
    kind: AgentTransportKind,
}

struct GatewayAcpPermissionHandler {
    approval_manager: Arc<ApprovalManager>,
}

impl GatewayAcpPermissionHandler {
    fn new(approval_manager: Arc<ApprovalManager>) -> Self {
        Self { approval_manager }
    }
}

#[async_trait]
impl AcpPermissionHandler for GatewayAcpPermissionHandler {
    async fn select_option(&self, request: AcpPermissionRequest) -> anyhow::Result<Option<String>> {
        let command = format!(
            "ACP permission requested for {} [{}]",
            request.tool_call,
            request
                .options
                .iter()
                .map(|option| option.name.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        );
        let session_key = request.chelix_session_key.as_deref();
        let (request_id, decision_rx) = self
            .approval_manager
            .create_request(&command, session_key)
            .await;
        if let Some(session_key) = request.chelix_session_key.as_deref() {
            tracing::info!(request_id, session_key, "ACP permission request is pending");
        }
        match self.approval_manager.wait_for_decision(decision_rx).await {
            ApprovalDecision::Approved => Ok(select_allowed_acp_option(&request)),
            ApprovalDecision::Denied | ApprovalDecision::Timeout => {
                Ok(select_rejected_acp_option(&request))
            },
        }
    }
}

fn select_allowed_acp_option(request: &AcpPermissionRequest) -> Option<String> {
    request
        .options
        .iter()
        .find(|option| option.kind == AcpPermissionOptionKind::AllowOnce)
        .or_else(|| {
            request
                .options
                .iter()
                .find(|option| option.kind == AcpPermissionOptionKind::AllowAlways)
        })
        .map(|option| option.id.clone())
}

fn select_rejected_acp_option(request: &AcpPermissionRequest) -> Option<String> {
    request
        .options
        .iter()
        .find(|option| option.kind == AcpPermissionOptionKind::RejectOnce)
        .or_else(|| {
            request
                .options
                .iter()
                .find(|option| option.kind == AcpPermissionOptionKind::RejectAlways)
        })
        .map(|option| option.id.clone())
}

impl GatewayExternalAgentService {
    pub fn new(
        config: ExternalAgentsConfig,
        session_metadata: Arc<chelix_sessions::metadata::SqliteSessionMetadata>,
        approval_manager: Arc<ApprovalManager>,
        agents_config: Arc<tokio::sync::RwLock<chelix_config::AgentsConfig>>,
        model_service: Arc<dyn ModelService>,
    ) -> Self {
        let mut registry = ExternalAgentRegistry::new();
        registry.register(Box::new(ClaudeCodeTransport::new()));
        registry.register(Box::new(CodexTransport::new()));
        registry.register(Box::new(
            AcpTransport::new("acp".to_string()).with_permission_handler(Arc::new(
                GatewayAcpPermissionHandler::new(approval_manager),
            )),
        ));
        Self {
            registry,
            config,
            session_metadata,
            agents_config,
            model_service,
            live_sessions: Mutex::new(HashMap::new()),
        }
    }

    #[cfg(test)]
    fn with_registry(
        config: ExternalAgentsConfig,
        session_metadata: Arc<chelix_sessions::metadata::SqliteSessionMetadata>,
        registry: ExternalAgentRegistry,
        agents_config: Arc<tokio::sync::RwLock<chelix_config::AgentsConfig>>,
        model_service: Arc<dyn ModelService>,
    ) -> Self {
        Self {
            registry,
            config,
            session_metadata,
            agents_config,
            model_service,
            live_sessions: Mutex::new(HashMap::new()),
        }
    }

    async fn session_for_binding(
        &self,
        session_key: &str,
        kind: AgentTransportKind,
    ) -> anyhow::Result<LiveExternalAgentSession> {
        self.shutdown_idle_sessions().await;
        let key = LiveSessionKey {
            session_key: session_key.to_string(),
            kind,
        };
        let mut live_sessions = self.live_sessions.lock().await;
        if let Some(entry) = live_sessions.get_mut(&key) {
            let is_alive = entry.session.lock().await.is_alive().await;
            if is_alive {
                entry.last_used = std::time::Instant::now();
                return Ok(Arc::clone(&entry.session));
            }
        }
        let spec = self.spec_for_kind(kind)?;
        let mut spec = spec;
        spec.session_key = Some(session_key.to_string());
        spec.external_session_id = self
            .session_metadata
            .get(session_key)
            .await?
            .and_then(|entry| entry.external_session_id().map(str::to_string));
        let session = Arc::new(Mutex::new(self.registry.start_session(&spec).await?));
        live_sessions.insert(key, LiveSessionEntry {
            session: Arc::clone(&session),
            last_used: std::time::Instant::now(),
        });
        Ok(session)
    }

    async fn shutdown_idle_sessions(&self) {
        let sessions = {
            let mut live_sessions = self.live_sessions.lock().await;
            let now = std::time::Instant::now();
            let keys = live_sessions
                .iter()
                .filter(|(_, entry)| now.duration_since(entry.last_used) >= LIVE_SESSION_IDLE_TTL)
                .map(|(key, _)| key.clone())
                .collect::<Vec<_>>();
            keys.into_iter()
                .filter_map(|key| live_sessions.remove(&key).map(|entry| entry.session))
                .collect::<Vec<_>>()
        };
        for session in sessions {
            let mut session = session.lock().await;
            if let Err(error) = session.shutdown().await {
                warn!(%error, "failed to shut down idle external agent session");
            }
        }
    }

    pub(crate) async fn shutdown_binding(&self, session_key: &str) {
        let sessions = {
            let mut live_sessions = self.live_sessions.lock().await;
            let keys = live_sessions
                .keys()
                .filter(|key| key.session_key == session_key)
                .cloned()
                .collect::<Vec<_>>();
            keys.into_iter()
                .filter_map(|key| live_sessions.remove(&key).map(|entry| entry.session))
                .collect::<Vec<_>>()
        };
        for session in sessions {
            let mut session = session.lock().await;
            if let Err(error) = session.shutdown().await {
                warn!(%error, session_key, "failed to shut down external agent session");
            }
        }
    }

    async fn resolved_llm_agent_for_entry(
        &self,
        entry: &chelix_sessions::metadata::SessionEntry,
    ) -> Result<(String, chelix_common::ResolvedModelReasoning), ServiceError> {
        let (agent_id, model, reasoning_effort) = {
            let agents = self.agents_config.read().await;
            let agent_id = match entry
                .agent_id
                .as_deref()
                .map(str::trim)
                .filter(|agent_id| !agent_id.is_empty())
            {
                Some(agent_id) => {
                    if agents.get(agent_id).is_none() {
                        return Err(ServiceError::message(format!(
                            "session '{}' references unknown agent '{agent_id}'",
                            entry.key
                        )));
                    }
                    agent_id.to_string()
                },
                None => {
                    let default_agent_id = agents.default.trim();
                    if default_agent_id.is_empty() || agents.get(default_agent_id).is_none() {
                        return Err(ServiceError::message(
                            "agents.default must reference an existing agent",
                        ));
                    }
                    default_agent_id.to_string()
                },
            };
            let agent = agents.get(&agent_id).ok_or_else(|| {
                ServiceError::message(format!("agent '{agent_id}' is not configured"))
            })?;
            (
                agent_id,
                agent.model.clone(),
                agent.reasoning_effort.clone(),
            )
        };
        let model_reasoning = self
            .model_service
            .resolve_model_reasoning(&model, Some(&reasoning_effort))
            .await?;
        Ok((agent_id, model_reasoning))
    }

    fn spec_for_kind(&self, kind: AgentTransportKind) -> anyhow::Result<ExternalAgentSpec> {
        if !self.config.enabled {
            anyhow::bail!("external agents are disabled")
        }
        let mut spec = ExternalAgentSpec::new(kind);
        if let Some(agent_config) = self.config.agents.get(kind.as_str()) {
            spec.binary = agent_config.binary.clone();
            spec.args = agent_config.args.clone();
            spec.env = agent_config.env.clone();
            spec.working_dir = agent_config.working_dir.as_ref().map(Into::into);
            spec.timeout_secs = agent_config.timeout_secs;
            spec.use_tmux = agent_config.use_tmux.unwrap_or(false);
        }
        Ok(spec)
    }
}

pub struct ExternalAgentSessionService {
    inner: Arc<dyn SessionService>,
    external_agents: Arc<GatewayExternalAgentService>,
}

impl ExternalAgentSessionService {
    pub fn new(
        inner: Arc<dyn SessionService>,
        external_agents: Arc<GatewayExternalAgentService>,
    ) -> Self {
        Self {
            inner,
            external_agents,
        }
    }
}

#[async_trait]
impl SessionService for ExternalAgentSessionService {
    async fn list(&self) -> ServiceResult {
        self.inner.list().await
    }

    async fn preview(&self, params: Value) -> ServiceResult {
        self.inner.preview(params).await
    }

    async fn resolve(&self, params: Value) -> ServiceResult {
        self.inner.resolve(params).await
    }

    async fn patch(&self, params: Value) -> ServiceResult {
        self.inner.patch(params).await
    }

    async fn voice_generate(&self, params: Value) -> ServiceResult {
        self.inner.voice_generate(params).await
    }

    async fn share_create(&self, params: Value) -> ServiceResult {
        self.inner.share_create(params).await
    }

    async fn share_list(&self, params: Value) -> ServiceResult {
        self.inner.share_list(params).await
    }

    async fn share_revoke(&self, params: Value) -> ServiceResult {
        self.inner.share_revoke(params).await
    }

    async fn reset(&self, params: Value) -> ServiceResult {
        if let Some(session_key) = session_key_param(&params) {
            self.external_agents.shutdown_binding(&session_key).await;
        }
        self.inner.reset(params).await
    }

    async fn delete(&self, params: Value) -> ServiceResult {
        if let Some(session_key) = session_key_param(&params) {
            self.external_agents.shutdown_binding(&session_key).await;
        }
        self.inner.delete(params).await
    }

    async fn truncate_tail(&self, params: Value) -> ServiceResult {
        if let Some(session_key) = session_key_param(&params) {
            self.external_agents.shutdown_binding(&session_key).await;
        }
        self.inner.truncate_tail(params).await
    }

    async fn compact(&self, params: Value) -> ServiceResult {
        self.inner.compact(params).await
    }

    async fn search(&self, params: Value) -> ServiceResult {
        self.inner.search(params).await
    }

    async fn fork(&self, params: Value) -> ServiceResult {
        self.inner.fork(params).await
    }

    async fn branches(&self, params: Value) -> ServiceResult {
        self.inner.branches(params).await
    }

    async fn run_detail(&self, params: Value) -> ServiceResult {
        self.inner.run_detail(params).await
    }

    async fn clear_all(&self) -> ServiceResult {
        for entry in self
            .external_agents
            .session_metadata
            .list()
            .await
            .map_err(ServiceError::message)?
        {
            self.external_agents.shutdown_binding(&entry.key).await;
        }
        self.inner.clear_all().await
    }

    async fn mark_seen(&self, key: &str) {
        self.inner.mark_seen(key).await;
    }
}

#[async_trait]
impl ExternalAgentService for GatewayExternalAgentService {
    async fn list(&self) -> ServiceResult {
        if !self.config.enabled {
            return Ok(serde_json::json!([]));
        }
        Ok(serde_json::to_value(self.registry.list_agents().await)
            .unwrap_or_else(|_| serde_json::json!([])))
    }

    async fn bind(&self, params: Value) -> ServiceResult {
        if !self.config.enabled {
            return Err("external agents are disabled".into());
        }
        let session_key = params
            .get("sessionKey")
            .or_else(|| params.get("session_key"))
            .and_then(|value| value.as_str())
            .ok_or_else(|| "missing sessionKey".to_string())?;
        let kind = params
            .get("kind")
            .and_then(|value| value.as_str())
            .ok_or_else(|| "missing kind".to_string())?
            .parse::<AgentTransportKind>()
            .map_err(|error| error.to_string())?;
        if !self.registry.has_kind(kind) {
            return Err(format!("external agent kind is not registered: {kind}").into());
        }
        self.shutdown_binding(session_key).await;
        self.session_metadata
            .bind_external(
                session_key,
                None,
                &chelix_sessions::metadata::ExternalSessionIdentity::new(kind, None),
            )
            .await
            .map_err(ServiceError::message)?;
        Ok(serde_json::json!({ "ok": true, "sessionKey": session_key, "kind": kind.as_str() }))
    }

    async fn unbind(&self, params: Value) -> ServiceResult {
        let session_key = params
            .get("sessionKey")
            .or_else(|| params.get("session_key"))
            .and_then(|value| value.as_str())
            .ok_or_else(|| "missing sessionKey".to_string())?;
        let entry = self
            .session_metadata
            .get(session_key)
            .await
            .map_err(ServiceError::message)?
            .ok_or_else(|| ServiceError::message(format!("session '{session_key}' not found")))?;
        match &entry.backing {
            chelix_sessions::metadata::SessionBacking::LlmExternal { .. } => {
                self.session_metadata
                    .unbind_llm_external(session_key, entry.version)
                    .await
                    .map_err(ServiceError::message)?;
            },
            chelix_sessions::metadata::SessionBacking::External { .. } => {
                let (agent_id, model_reasoning) = self.resolved_llm_agent_for_entry(&entry).await?;
                self.session_metadata
                    .replace_external_with_llm(
                        session_key,
                        entry.version,
                        &agent_id,
                        &model_reasoning,
                    )
                    .await
                    .map_err(ServiceError::message)?;
            },
            chelix_sessions::metadata::SessionBacking::Llm { .. } => {
                return Err(ServiceError::message(format!(
                    "session '{session_key}' has no external binding"
                )));
            },
        }
        self.shutdown_binding(session_key).await;
        Ok(serde_json::json!({ "ok": true, "sessionKey": session_key }))
    }

    async fn status(&self, params: Value) -> ServiceResult {
        let session_key = params
            .get("sessionKey")
            .or_else(|| params.get("session_key"))
            .and_then(|value| value.as_str())
            .ok_or_else(|| "missing sessionKey".to_string())?;
        let entry = self
            .session_metadata
            .get(session_key)
            .await
            .map_err(ServiceError::message)?;
        let kind = entry.as_ref().and_then(|entry| entry.external_agent_kind());
        Ok(serde_json::json!({
            "bound": kind.is_some(),
            "sessionKey": session_key,
            "kind": kind.map(|kind| kind.as_str()),
            "externalSessionId": entry.and_then(|entry| entry.external_session_id().map(str::to_string)),
        }))
    }

    async fn shutdown_session(&self, session_key: &str) {
        self.shutdown_binding(session_key).await;
    }
}

pub struct ExternalAgentChatService {
    inner: Arc<dyn ChatService>,
    external_agents: Arc<GatewayExternalAgentService>,
    state: Arc<GatewayState>,
    session_store: Arc<chelix_sessions::store::SessionStore>,
    session_metadata: Arc<chelix_sessions::metadata::SqliteSessionMetadata>,
}

impl ExternalAgentChatService {
    pub fn new(
        inner: Arc<dyn ChatService>,
        external_agents: Arc<GatewayExternalAgentService>,
        state: Arc<GatewayState>,
        session_store: Arc<chelix_sessions::store::SessionStore>,
        session_metadata: Arc<chelix_sessions::metadata::SqliteSessionMetadata>,
    ) -> Self {
        Self {
            inner,
            external_agents,
            state,
            session_store,
            session_metadata,
        }
    }

    async fn external_kind_for_session(
        &self,
        session_key: &str,
    ) -> Option<Result<AgentTransportKind, ServiceError>> {
        if !self.external_agents.config.enabled {
            return None;
        }
        let entry = match self.session_metadata.get(session_key).await {
            Ok(entry) => entry?,
            Err(error) => return Some(Err(ServiceError::message(error.to_string()))),
        };
        entry.external_agent_kind().map(Ok)
    }

    async fn send_external(
        &self,
        text: String,
        seq: Option<u64>,
        client_message_id: Option<String>,
        channel: Option<Value>,
        session_key: String,
        kind: AgentTransportKind,
    ) -> ServiceResult {
        let _session_permit = match self
            .state
            .services
            .session_mutations
            .try_acquire_turn(&session_key)
            .await
        {
            Ok(permit) => permit,
            Err(error) if error.reason() == SessionBusyReason::ReservedMutation => {
                return Err(ServiceError::message(
                    "Session history is being updated; please try again.",
                ));
            },
            Err(_) => {
                return Err(ServiceError::message(
                    "External agent session is busy; please wait for the active turn to finish.",
                ));
            },
        };
        let run_id = uuid::Uuid::new_v4().to_string();
        let ui = self
            .session_store
            .ui_history
            .session(&session_key)
            .await
            .map_err(ServiceError::message)?;
        let ui_run = ui
            .begin_run(chelix_sessions::ui_history_types::UiRunMetadata {
                run_id: run_id.clone(),
                model: kind.as_str().to_string(),
                provider: "external-agent".to_string(),
                reasoning_effort: None,
            })
            .map_err(ServiceError::message)?;
        let result: ServiceResult = async {
        let created_at = now_ms();
        let mut history = self
            .session_store
            .read(&session_key)
            .await
            .map_err(ServiceError::message)?;
        let user_msg = PersistedMessage::User {
            content: MessageContent::Text(text.clone()),
            created_at: Some(created_at),
            audio: None,
            documents: None,
            channel,
            seq,
            run_id: Some(run_id.clone()),
        };
        let mut user_value = user_msg.to_value();
        if let Some(id) = client_message_id {
            chelix_sessions::ui_history_types::validate_client_message_id(&id).map_err(ServiceError::message)?;
            user_value["clientMessageId"] = Value::String(id);
        }
        let user_message_index = self
            .session_store
            .append_with_index(&session_key, &user_value)
            .await
            .map_err(|error| error.to_string())?;
        let assistant_message_index = user_message_index
            .checked_add(1)
            .ok_or_else(|| ServiceError::message("assistant message index overflow"))?;
        history.push(user_value);
        let message_count = self
            .session_store
            .ui_message_count(&session_key)
            .await
            .map_err(ServiceError::message)?;
        self.session_metadata
            .touch(&session_key, message_count)
            .await
            .map_err(ServiceError::message)?;

        crate::broadcast::broadcast(
            &self.state,
            "chat",
            serde_json::json!({
                "runId": run_id,
                "sessionKey": session_key,
                "state": "running",
                "model": kind.as_str(),
                "provider": "external-agent",
                "seq": seq,
            }),
            BroadcastOpts::default(),
        )
        .await;

        let context = context_from_history(&history);
        let start = std::time::Instant::now();
        let live_session = self
            .external_agents
            .session_for_binding(&session_key, kind)
            .await
            .map_err(|error| error.to_string())?;
        let mut session = live_session.lock().await;
        let external_session_id = session.external_session_id().map(str::to_string);
        if external_session_id.is_some() {
            self.session_metadata
                .update_external_session_id(&session_key, external_session_id.as_deref())
                .await
                .map_err(ServiceError::message)?;
        }
        let mut events = match session.send_prompt(&text, Some(&context)).await {
            Ok(events) => events,
            Err(error) => {
                let error = error.to_string();
                drop(session);
                self.external_agents.shutdown_binding(&session_key).await;
                return Err(error.into());
            },
        };
        let mut assistant_text = String::new();
        let mut token_usage = None;
        let mut external_error = None;
        let mut completed = false;
        let segment_id = ProviderSegmentId::new(format!("seg_{run_id}"));
        let mut materializer = ProviderSegmentMaterializer::new(segment_id.clone());
        ui_run.start_attempt(Some(segment_id.clone())).map_err(ServiceError::message)?;
        let mut health = ui_run.health();
        let mut update_seq: u64 = 0;
        // The transport reports text and thinking as separate event kinds with
        // no ordering between them, so the position of an item is assigned once
        // when its identity first arrives.
        let mut item_positions = ItemPositionAllocator::default();
        crate::broadcast::broadcast(
            &self.state,
            "chat",
            serde_json::json!({
                "runId": run_id,
                "sessionKey": session_key,
                "state": "segment_start",
                "segmentId": segment_id.0,
                "seq": seq,
            }),
            BroadcastOpts::default(),
        )
        .await;
        loop {
            let event = tokio::select! {
                event = events.next() => event,
                changed = health.changed() => {
                    if changed.is_err() {
                        external_error = Some("UI history health subscription closed".to_string());
                        break;
                    }
                    if let Some(error) = health.borrow_and_update().failure.clone() {
                        external_error = Some(error);
                        break;
                    }
                    continue;
                },
            };
            let Some(event) = event else { break; };
            match event {
                ExternalAgentEvent::TextDelta(delta) => {
                    assistant_text.push_str(&delta);
                    update_seq += 1;
                    let item_id = ProviderItemId::new(EXTERNAL_AGENT_MESSAGE_ITEM_ID);
                    let position = item_positions.position_for(&item_id);
                    let update = ProviderItemUpdate {
                        segment_id: segment_id.clone(),
                        item_id,
                        position,
                        update_seq,
                        payload: ProviderItemUpdatePayload::MessageDelta {
                            delta: delta.clone(),
                        },
                    };
                    if let Err(error) = materializer.apply_update(&update) {
                        external_error = Some(error.to_string());
                        break;
                    }
                    if let Err(error) = ui_run.copy(PersistedMessage::ProviderUpdate {
                        update, created_at: Some(now_ms()), seq, run_id: Some(run_id.clone()),
                    }) {
                        external_error = Some(error.to_string());
                        break;
                    }
                },
                ExternalAgentEvent::ThinkingDelta(delta) => {
                    update_seq += 1;
                    let item_id = ProviderItemId::new(EXTERNAL_AGENT_REASONING_ITEM_ID);
                    let position = item_positions.position_for(&item_id);
                    let update = ProviderItemUpdate {
                        segment_id: segment_id.clone(),
                        item_id,
                        position,
                        update_seq,
                        payload: ProviderItemUpdatePayload::ReasoningTextDelta { delta },
                    };
                    if let Err(error) = materializer.apply_update(&update) {
                        external_error = Some(error.to_string());
                        break;
                    }
                    if let Err(error) = ui_run.copy(PersistedMessage::ProviderUpdate {
                        update, created_at: Some(now_ms()), seq, run_id: Some(run_id.clone()),
                    }) {
                        external_error = Some(error.to_string());
                        break;
                    }
                },
                ExternalAgentEvent::Error(error) => {
                    if let Err(retention_error) = ui_run.error(chelix_sessions::ui_history_types::UiProviderError {
                        run_id: run_id.clone(), segment_id: Some(segment_id.clone()), created_at: now_ms(),
                        raw: error.clone(), details: serde_json::json!({"title": "External agent error", "detail": error}), retry_after_ms: None,
                    }) {
                        tracing::error!(%retention_error, "failed to retain external-agent error");
                    }
                    external_error = Some(error);
                    break;
                },
                ExternalAgentEvent::Done { usage } => {
                    token_usage = usage;
                    completed = true;
                    break;
                },
                ExternalAgentEvent::ToolCallStart { .. }
                | ExternalAgentEvent::ToolCallEnd { .. } => {},
            }
        }
        if let Some(external_session_id) = session.external_session_id().map(str::to_string)
            && let Err(error) = self.session_metadata.update_external_session_id(&session_key, Some(&external_session_id)).await
        {
            external_error = Some(format!("{}failed to update external session identity: {error}", external_error.as_ref().map(|error| format!("{error}; ")).unwrap_or_default()));
        }
        drop(session);
        if external_error.is_none() && !completed {
            external_error =
                Some("The external agent stream ended without a terminal event.".to_string());
        }
        let segment_outcome = if external_error.is_some() {
            ProviderSegmentOutcome::Failed
        } else {
            ProviderSegmentOutcome::Completed
        };
        ui_run
            .copy(PersistedMessage::ProviderSegmentClose {
                segment_id: segment_id.clone(),
                outcome: segment_outcome,
                created_at: Some(now_ms()),
                seq,
                run_id: Some(run_id.clone()),
            })
            .map_err(ServiceError::message)?;
        if let Some(error) = &external_error
            && ui_run.recorded_error(error).map_err(ServiceError::message)?.is_none()
        {
            ui_run
                .error(chelix_sessions::ui_history_types::UiProviderError {
                    run_id: run_id.clone(),
                    segment_id: Some(segment_id.clone()),
                    created_at: now_ms(),
                    raw: error.clone(),
                    details: serde_json::json!({ "detail": error }),
                    retry_after_ms: None,
                })
                .map_err(ServiceError::message)?;
        }
        materializer
            .close(segment_outcome)
            .map_err(|error| ServiceError::message(error.to_string()))?;
        let duration_ms = start.elapsed().as_millis() as u64;
        let provider_items = if materializer.segment.items.is_empty() {
            None
        } else {
            Some(materializer.segment.items.clone())
        };
        let reasoning = materializer.segment.reasoning_content();
        let assistant_msg = PersistedMessage::Assistant {
            content: assistant_text.clone(),
            created_at: Some(now_ms()),
            model: Some(kind.as_str().to_string()),
            provider: Some("external-agent".to_string()),
            reasoning_effort: None,
            input_tokens: token_usage.as_ref().map(|usage| usage.input_tokens),
            output_tokens: token_usage.as_ref().map(|usage| usage.output_tokens),
            cache_read_tokens: None,
            cache_write_tokens: None,
            duration_ms: Some(duration_ms),
            request_input_tokens: None,
            request_output_tokens: None,
            request_cache_read_tokens: None,
            request_cache_write_tokens: None,
            tool_calls: None,
            reasoning: reasoning.clone(),
            provider_items: provider_items.clone(),
            segment_id: Some(segment_id.clone()),
            llm_api_response: None,
            audio: None,
            seq,
            run_id: Some(run_id.clone()),
        };
        if let Err(error) = self.session_store.append_at_index(
            &session_key, &assistant_msg.to_value(), assistant_message_index,
        ).await {
            return Err(ServiceError::message(format!(
                "{}failed to persist external-agent response: {error}",
                external_error.as_ref().map(|error| format!("{error}; ")).unwrap_or_default(),
            )));
        }
        ui_run.merge_metadata(
            &chelix_sessions::ui_history_types::UiMessageId::segment(&segment_id),
            std::collections::BTreeMap::from([("replyMedium".to_string(), Value::String("text".to_string()))]),
        ).map_err(ServiceError::message)?;
        if let Some(error) = external_error {
            return Err(error.into());
        }
        Ok(serde_json::json!({ "ok": true, "runId": run_id }))
        }.await;
        let mut result = result;
        if let Err(error) = &result {
            self.external_agents.shutdown_binding(&session_key).await;
            let raw = error.to_string();
            let retained = ui_run.recorded_error(&raw).and_then(|existing| {
                if existing.is_none() {
                    ui_run.error(chelix_sessions::ui_history_types::UiProviderError {
                        run_id: run_id.clone(), segment_id: None, created_at: now_ms(), raw: raw.clone(),
                        details: serde_json::json!({"title": "External agent error", "detail": raw}), retry_after_ms: None,
                    })?;
                }
                Ok(())
            });
            if let Err(error) = retained {
                tracing::error!(%error, "failed to retain external-agent failure");
            }
        }
        if let Err(error) = ui_run.finish().await {
            tracing::error!(%error, "failed to finalize external-agent UI history");
            result = Err(ServiceError::message(format!(
                "{}UI history finalization failed: {error}",
                result
                    .as_ref()
                    .err()
                    .map(|error| format!("{error}; "))
                    .unwrap_or_default(),
            )));
        }
        let count_result = async {
            let count = self
                .session_store
                .ui_message_count(&session_key)
                .await
                .map_err(ServiceError::message)?;
            self.session_metadata
                .touch(&session_key, count)
                .await
                .map_err(ServiceError::message)
        }
        .await;
        if let Err(error) = count_result {
            tracing::error!(%error, "failed to update external-agent message count");
            result = Err(error);
        }
        crate::broadcast::broadcast(
            &self.state, "chat",
            serde_json::json!({"runId": run_id, "sessionKey": session_key, "state": if result.is_ok() { "final" } else { "error" }}),
            BroadcastOpts::default(),
        ).await;
        result
    }
}

#[async_trait]
impl ChatService for ExternalAgentChatService {
    async fn send(&self, request: ChatSendRequest, context: ChatExecutionContext) -> ServiceResult {
        let session_key = context.session_id.to_string();
        if let Some(kind) = self.external_kind_for_session(&session_key).await {
            let kind = kind?;
            let text = match &request.message {
                ChatSendMessage::Text(text) => text.clone(),
                ChatSendMessage::Content(_) => {
                    return Err(ServiceError::message(
                        "external agents currently require text input",
                    ));
                },
            };
            let channel = external_channel_value(&context)?;
            return self
                .send_external(
                    text,
                    request.client_sequence,
                    request.client_message_id,
                    channel,
                    session_key,
                    kind,
                )
                .await;
        }
        self.inner.send(request, context).await
    }

    async fn send_sync(
        &self,
        request: ChatSendSyncRequest,
        context: ChatExecutionContext,
    ) -> ServiceResult {
        let session_key = context.session_id.to_string();
        if context.agent_id.is_none()
            && let Some(kind) = self.external_kind_for_session(&session_key).await
        {
            let channel = external_channel_value(&context)?;
            return self
                .send_external(
                    request.text.clone(),
                    None,
                    None,
                    channel,
                    session_key,
                    kind?,
                )
                .await;
        }
        self.inner.send_sync(request, context).await
    }

    async fn abort(&self, params: Value) -> ServiceResult {
        let session_key = resolve_session_key(&params, &self.state).await;
        self.external_agents.shutdown_binding(&session_key).await;
        self.inner.abort(params).await
    }

    async fn queued_prompts_status(
        &self,
        session_id: chelix_sessions::SessionKey,
    ) -> Result<chelix_sessions::QueuedPromptsStatus, ServiceError> {
        self.inner.queued_prompts_status(session_id).await
    }

    async fn queued_prompts_remove(
        &self,
        id: i64,
    ) -> Result<chelix_sessions::QueuedPromptsStatus, ServiceError> {
        self.inner.queued_prompts_remove(id).await
    }

    async fn history(&self, params: Value) -> ServiceResult {
        self.inner.history(params).await
    }

    async fn inject(&self, params: Value) -> ServiceResult {
        self.inner.inject(params).await
    }

    async fn clear(&self, params: Value) -> ServiceResult {
        let session_key = resolve_session_key(&params, &self.state).await;
        self.external_agents.shutdown_binding(&session_key).await;
        self.inner.clear(params).await
    }

    async fn compact(
        &self,
        request: ChatCompactRequest,
        context: ChatExecutionContext,
    ) -> ServiceResult {
        self.inner.compact(request, context).await
    }

    async fn context(
        &self,
        request: ChatContextRequest,
        context: ChatExecutionContext,
    ) -> ServiceResult {
        self.inner.context(request, context).await
    }

    async fn raw_prompt(
        &self,
        request: ChatRawPromptRequest,
        context: ChatExecutionContext,
    ) -> ServiceResult {
        self.inner.raw_prompt(request, context).await
    }

    async fn full_context(
        &self,
        request: ChatFullContextRequest,
        context: ChatExecutionContext,
    ) -> ServiceResult {
        self.inner.full_context(request, context).await
    }

    async fn refresh_prompt_memory(&self, params: Value) -> ServiceResult {
        self.inner.refresh_prompt_memory(params).await
    }

    async fn active(&self, params: Value) -> ServiceResult {
        self.inner.active(params).await
    }

    async fn active_session_keys(&self) -> Vec<String> {
        self.inner.active_session_keys().await
    }

    async fn active_voice_pending(&self, session_key: &str) -> bool {
        self.inner.active_voice_pending(session_key).await
    }

    async fn active_tool_invocations(
        &self,
        session_key: &str,
    ) -> Vec<chelix_common::ActiveToolInvocation> {
        self.inner.active_tool_invocations(session_key).await
    }

    async fn peek(&self, params: Value) -> ServiceResult {
        self.inner.peek(params).await
    }

    async fn wait_for_session_gate(
        &self,
        session_key: &str,
    ) -> Result<Option<SessionTerminal>, ServiceError> {
        self.inner.wait_for_session_gate(session_key).await
    }

    async fn session_terminal(
        &self,
        session_key: &str,
    ) -> Result<Option<SessionTerminal>, ServiceError> {
        self.inner.session_terminal(session_key).await
    }
}

fn external_channel_value(context: &ChatExecutionContext) -> Result<Option<Value>, ServiceError> {
    context
        .channel
        .as_ref()
        .map(|metadata| {
            serde_json::to_value(QueuedPromptChannelMetadata {
                channel_type: metadata.channel_type,
                sender_name: metadata.sender_name.clone(),
                username: metadata.username.clone(),
                sender_id: metadata.sender_id.clone(),
                message_kind: metadata.message_kind,
            })
            .map_err(|error| ServiceError::message(error.to_string()))
        })
        .transpose()
}

async fn resolve_session_key(params: &Value, state: &GatewayState) -> String {
    if let Some(key) = params
        .get("_session_key")
        .or_else(|| params.get("sessionKey"))
        .or_else(|| params.get("session_key"))
        .and_then(|value| value.as_str())
    {
        return key.to_string();
    }
    let conn_id = params.get("_conn_id").and_then(|value| value.as_str());
    if let Some(conn_id) = conn_id
        && let Some(key) = state
            .client_registry
            .read()
            .await
            .active_sessions
            .get(conn_id)
            .cloned()
    {
        return key;
    }
    "main".to_string()
}

fn context_from_history(history: &[Value]) -> ContextSnapshot {
    let recent_turns = history
        .iter()
        .rev()
        .take(20)
        .filter_map(|value| serde_json::from_value::<PersistedMessage>(value.clone()).ok())
        .filter_map(|message| match message {
            PersistedMessage::User { content, .. } => Some(ContextTurn {
                role: "user".to_string(),
                content: message_content_text(&content),
            }),
            PersistedMessage::Assistant { content, .. } => Some(ContextTurn {
                role: "assistant".to_string(),
                content,
            }),
            PersistedMessage::System { content, .. } => Some(ContextTurn {
                role: "system".to_string(),
                content,
            }),
            _ => None,
        })
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    ContextSnapshot {
        recent_turns,
        ..ContextSnapshot::default()
    }
}

fn session_key_param(params: &Value) -> Option<String> {
    params
        .get("key")
        .or_else(|| params.get("sessionKey"))
        .or_else(|| params.get("session_key"))
        .or_else(|| params.get("_session_key"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

fn message_content_text(content: &MessageContent) -> String {
    match content {
        MessageContent::Text(text) => text.clone(),
        MessageContent::Multimodal(blocks) => blocks
            .iter()
            .filter_map(|block| match block {
                chelix_sessions::ContentBlock::Text { text } => Some(text.as_str()),
                chelix_sessions::ContentBlock::ImageUrl { .. } => None,
            })
            .collect::<Vec<_>>()
            .join("\n"),
    }
}

fn now_ms() -> u64 {
    match SystemTime::now().duration_since(SystemTime::UNIX_EPOCH) {
        Ok(duration) => duration.as_millis() as u64,
        Err(error) => {
            warn!(%error, "system clock is before UNIX_EPOCH");
            0
        },
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use std::{pin::Pin, sync::atomic::Ordering};

    use {
        super::*,
        crate::{
            auth::{AuthMode, ResolvedAuth},
            services::GatewayServices,
        },
        chelix_external_agents::{
            ExternalAgentTransport,
            types::{AcpPermissionOption, ExternalAgentStatus},
        },
        chelix_service_traits::{ExternalAgentService, NoopChatService},
        chelix_sessions::{metadata::SqliteSessionMetadata, store::SessionStore},
        futures::{Stream, stream},
    };

    struct ExactTestModelService;

    #[async_trait]
    impl ModelService for ExactTestModelService {
        async fn list(&self) -> ServiceResult {
            Ok(serde_json::json!([]))
        }

        async fn list_all(&self) -> ServiceResult {
            Ok(serde_json::json!([]))
        }

        async fn resolve_model_reasoning(
            &self,
            model: &str,
            reasoning_effort: Option<&chelix_common::ReasoningEffort>,
        ) -> Result<chelix_common::ResolvedModelReasoning, ServiceError> {
            let reasoning_effort = reasoning_effort.ok_or_else(|| {
                ServiceError::message("reasoning effort is required for test model resolution")
            })?;
            chelix_common::ResolvedModelReasoning::try_new(
                model.to_string(),
                reasoning_effort.clone(),
            )
            .map_err(|error| ServiceError::message(error.to_string()))
        }

        async fn disable(&self, _params: Value) -> ServiceResult {
            Ok(serde_json::json!({}))
        }

        async fn enable(&self, _params: Value) -> ServiceResult {
            Ok(serde_json::json!({}))
        }
    }

    fn test_agents_config() -> Arc<tokio::sync::RwLock<chelix_config::AgentsConfig>> {
        let mut agents = chelix_config::AgentsConfig {
            default: "main".to_string(),
            ..chelix_config::AgentsConfig::default()
        };
        agents.entries.insert(
            "main".to_string(),
            chelix_config::AgentConfig::new(
                "Main",
                "test::model",
                chelix_common::ReasoningEffort::from("off"),
            ),
        );
        Arc::new(tokio::sync::RwLock::new(agents))
    }

    #[derive(Default)]
    struct FakeAgentState {
        starts: std::sync::atomic::AtomicUsize,
        prompts: std::sync::Mutex<Vec<String>>,
        shutdowns: std::sync::atomic::AtomicUsize,
    }

    struct FakeTransport {
        state: Arc<FakeAgentState>,
    }

    #[async_trait]
    impl ExternalAgentTransport for FakeTransport {
        fn name(&self) -> &str {
            "fake"
        }

        async fn is_available(&self) -> bool {
            true
        }

        fn supported_kinds(&self) -> &[AgentTransportKind] {
            &[AgentTransportKind::Codex]
        }

        async fn start_session(
            &self,
            _spec: &ExternalAgentSpec,
        ) -> anyhow::Result<Box<dyn ExternalAgentSession>> {
            let start_index = self.state.starts.fetch_add(1, Ordering::SeqCst) + 1;
            Ok(Box::new(FakeSession {
                state: Arc::clone(&self.state),
                external_session_id: format!("fake-session-{start_index}"),
                alive: true,
            }))
        }
    }

    struct FakeSession {
        state: Arc<FakeAgentState>,
        external_session_id: String,
        alive: bool,
    }

    #[async_trait]
    impl ExternalAgentSession for FakeSession {
        fn external_session_id(&self) -> Option<&str> {
            Some(&self.external_session_id)
        }

        async fn send_prompt(
            &mut self,
            prompt: &str,
            _context: Option<&ContextSnapshot>,
        ) -> anyhow::Result<Pin<Box<dyn Stream<Item = ExternalAgentEvent> + Send>>> {
            if prompt == "fail" {
                anyhow::bail!("fake send failure");
            }
            if prompt == "event-error" {
                return Ok(Box::pin(stream::iter([ExternalAgentEvent::Error(
                    "fake event failure".to_string(),
                )])));
            }
            if prompt == "partial-error" {
                return Ok(Box::pin(stream::iter([
                    ExternalAgentEvent::TextDelta("partial reply".to_string()),
                    ExternalAgentEvent::Error("fake partial failure".to_string()),
                ])));
            }
            if prompt == "thinking-first" {
                return Ok(Box::pin(stream::iter([
                    ExternalAgentEvent::ThinkingDelta("weighing options".to_string()),
                    ExternalAgentEvent::TextDelta("the answer".to_string()),
                    ExternalAgentEvent::Done { usage: None },
                ])));
            }
            self.state
                .prompts
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .push(prompt.to_string());
            Ok(Box::pin(stream::iter([
                ExternalAgentEvent::TextDelta(format!("reply to {prompt}")),
                ExternalAgentEvent::Done {
                    usage: (prompt == "usage").then_some(
                        chelix_external_agents::types::TokenUsage {
                            input_tokens: 7,
                            output_tokens: 11,
                        },
                    ),
                },
            ])))
        }

        async fn is_alive(&self) -> bool {
            self.alive
        }

        async fn shutdown(&mut self) -> anyhow::Result<()> {
            self.alive = false;
            self.state.shutdowns.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }

        fn status(&self) -> ExternalAgentStatus {
            if self.alive {
                ExternalAgentStatus::Idle
            } else {
                ExternalAgentStatus::Stopped
            }
        }
    }

    async fn sqlite_pool() -> sqlx::SqlitePool {
        let pool = sqlx::SqlitePool::connect("sqlite::memory:").await.unwrap();
        chelix_projects::run_migrations(&pool).await.unwrap();
        SqliteSessionMetadata::init(&pool).await.unwrap();
        pool
    }

    fn test_pair() -> chelix_common::ResolvedModelReasoning {
        chelix_common::ResolvedModelReasoning::try_new(
            "test::model".to_string(),
            chelix_common::ReasoningEffort::from("off"),
        )
        .unwrap()
    }

    async fn create_test_session(metadata: &SqliteSessionMetadata, key: &str) {
        metadata
            .create_llm_session(key, None, &test_pair(), Some("main"))
            .await
            .unwrap();
    }

    fn fake_external_agents(
        metadata: Arc<SqliteSessionMetadata>,
        state: Arc<FakeAgentState>,
    ) -> Arc<GatewayExternalAgentService> {
        fake_external_agents_with_config(
            ExternalAgentsConfig {
                enabled: true,
                ..ExternalAgentsConfig::default()
            },
            metadata,
            state,
        )
    }

    fn fake_external_agents_with_config(
        config: ExternalAgentsConfig,
        metadata: Arc<SqliteSessionMetadata>,
        state: Arc<FakeAgentState>,
    ) -> Arc<GatewayExternalAgentService> {
        let mut registry = ExternalAgentRegistry::new();
        registry.register(Box::new(FakeTransport { state }));
        Arc::new(GatewayExternalAgentService::with_registry(
            config,
            metadata,
            registry,
            test_agents_config(),
            Arc::new(ExactTestModelService),
        ))
    }

    fn test_gateway_state() -> Arc<GatewayState> {
        GatewayState::new(
            ResolvedAuth {
                mode: AuthMode::Token,
                token: None,
                password: None,
            },
            GatewayServices::noop(),
        )
    }

    #[derive(Default)]
    struct SyncTrackingChatService {
        calls: std::sync::Mutex<Vec<&'static str>>,
    }

    #[async_trait]
    impl ChatService for SyncTrackingChatService {
        async fn send(
            &self,
            _request: ChatSendRequest,
            _context: ChatExecutionContext,
        ) -> ServiceResult {
            self.calls
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .push("send");
            Ok(serde_json::json!({ "source": "send" }))
        }

        async fn send_sync(
            &self,
            _request: ChatSendSyncRequest,
            _context: ChatExecutionContext,
        ) -> ServiceResult {
            self.calls
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .push("send_sync");
            Ok(serde_json::json!({ "source": "send_sync" }))
        }

        async fn abort(&self, _params: Value) -> ServiceResult {
            Ok(serde_json::json!({}))
        }

        async fn history(&self, _params: Value) -> ServiceResult {
            Ok(serde_json::json!([]))
        }

        async fn inject(&self, _params: Value) -> ServiceResult {
            Ok(serde_json::json!({ "ok": true }))
        }

        async fn clear(&self, _params: Value) -> ServiceResult {
            Ok(serde_json::json!({ "ok": true }))
        }

        async fn compact(
            &self,
            _request: ChatCompactRequest,
            _context: ChatExecutionContext,
        ) -> ServiceResult {
            Ok(serde_json::json!({ "ok": true }))
        }

        async fn context(
            &self,
            _request: ChatContextRequest,
            _context: ChatExecutionContext,
        ) -> ServiceResult {
            Ok(serde_json::json!({}))
        }

        async fn raw_prompt(
            &self,
            _request: ChatRawPromptRequest,
            _context: ChatExecutionContext,
        ) -> ServiceResult {
            Ok(serde_json::json!({}))
        }

        async fn full_context(
            &self,
            _request: ChatFullContextRequest,
            _context: ChatExecutionContext,
        ) -> ServiceResult {
            Ok(serde_json::json!({}))
        }

        async fn wait_for_session_gate(
            &self,
            _session_key: &str,
        ) -> Result<Option<SessionTerminal>, ServiceError> {
            self.calls
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .push("wait_for_session_gate");
            Ok(Some(SessionTerminal::Completed))
        }

        async fn session_terminal(
            &self,
            _session_key: &str,
        ) -> Result<Option<SessionTerminal>, ServiceError> {
            self.calls
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .push("session_terminal");
            Ok(Some(SessionTerminal::Cancelled))
        }
    }

    fn test_chat_context() -> ChatExecutionContext {
        ChatExecutionContext::internal(chelix_sessions::SessionKey::new("main"))
    }

    async fn test_chat_service(
        external_agents: Arc<GatewayExternalAgentService>,
        metadata: Arc<SqliteSessionMetadata>,
        session_store: Arc<SessionStore>,
    ) -> ExternalAgentChatService {
        ExternalAgentChatService::new(
            Arc::new(NoopChatService),
            external_agents,
            test_gateway_state(),
            session_store,
            metadata,
        )
    }

    #[tokio::test]
    async fn bind_unbind_and_status_update_metadata() {
        let metadata = Arc::new(SqliteSessionMetadata::new(sqlite_pool().await));
        let agent_state = Arc::new(FakeAgentState::default());
        let service = fake_external_agents(Arc::clone(&metadata), agent_state);

        let bound = service
            .bind(serde_json::json!({ "sessionKey": "main", "kind": "codex" }))
            .await
            .expect("bind external agent");
        assert_eq!(bound["kind"], "codex");

        let status = service
            .status(serde_json::json!({ "sessionKey": "main" }))
            .await
            .expect("status");
        assert_eq!(status["bound"], true);
        assert_eq!(status["kind"], "codex");
        let external_entry = metadata
            .get("main")
            .await
            .expect("load external-only entry")
            .expect("external-only entry exists");

        service
            .unbind(serde_json::json!({ "sessionKey": "main" }))
            .await
            .expect("unbind external agent");
        let status = service
            .status(serde_json::json!({ "sessionKey": "main" }))
            .await
            .expect("status after unbind");
        assert_eq!(status["bound"], false);
        assert!(status["kind"].is_null());
        let llm_entry = metadata
            .get("main")
            .await
            .expect("load LLM entry")
            .expect("LLM entry exists after unbind");
        assert_eq!(llm_entry.id, external_entry.id);
        assert_eq!(llm_entry.key, external_entry.key);
        assert_eq!(llm_entry.version, external_entry.version + 1);
        assert_eq!(llm_entry.agent_id.as_deref(), Some("main"));
        assert_eq!(llm_entry.model(), Some("test::model"));
        assert_eq!(
            llm_entry
                .reasoning_effort()
                .map(chelix_common::ReasoningEffort::as_str),
            Some("off")
        );
        assert!(matches!(
            llm_entry.backing,
            chelix_sessions::metadata::SessionBacking::Llm { .. }
        ));
        assert_eq!(llm_entry.external_agent_kind(), None);
        assert_eq!(llm_entry.external_session_id(), None);
    }

    #[tokio::test]
    async fn list_returns_empty_when_external_agents_disabled() {
        let metadata = Arc::new(SqliteSessionMetadata::new(sqlite_pool().await));
        let agent_state = Arc::new(FakeAgentState::default());
        let service = fake_external_agents_with_config(
            ExternalAgentsConfig::default(),
            Arc::clone(&metadata),
            Arc::clone(&agent_state),
        );

        let agents = service.list().await.expect("list external agents");

        assert_eq!(agents, serde_json::json!([]));
        assert_eq!(agent_state.starts.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn disabled_external_agents_do_not_route_stale_bindings() {
        let dir = tempfile::tempdir().unwrap();
        let session_store = Arc::new(SessionStore::new(dir.path().to_path_buf()));
        let metadata = Arc::new(SqliteSessionMetadata::new(sqlite_pool().await));
        create_test_session(&metadata, "main").await;
        metadata
            .bind_external(
                "main",
                None,
                &chelix_sessions::metadata::ExternalSessionIdentity::new(
                    AgentTransportKind::Codex,
                    None,
                ),
            )
            .await
            .unwrap();
        let agent_state = Arc::new(FakeAgentState::default());
        let external_agents = fake_external_agents_with_config(
            ExternalAgentsConfig::default(),
            Arc::clone(&metadata),
            Arc::clone(&agent_state),
        );
        let chat = test_chat_service(
            Arc::clone(&external_agents),
            Arc::clone(&metadata),
            Arc::clone(&session_store),
        )
        .await;

        let error = chat
            .send(ChatSendRequest::text("hello"), test_chat_context())
            .await
            .expect_err("disabled external agents should fall back to inner chat");

        assert_eq!(error.to_string(), "chat not configured");
        assert_eq!(agent_state.starts.load(Ordering::SeqCst), 0);
        assert!(
            agent_state
                .prompts
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .is_empty()
        );
    }

    #[test]
    fn acp_permission_selection_prefers_matching_decision_kind() {
        let request = AcpPermissionRequest {
            chelix_session_key: Some("main".to_string()),
            acp_session_id: "acp-1".to_string(),
            tool_call: "run tool".to_string(),
            options: vec![
                AcpPermissionOption {
                    id: "reject".to_string(),
                    name: "Reject".to_string(),
                    kind: AcpPermissionOptionKind::RejectOnce,
                },
                AcpPermissionOption {
                    id: "allow".to_string(),
                    name: "Allow".to_string(),
                    kind: AcpPermissionOptionKind::AllowOnce,
                },
            ],
        };

        assert_eq!(
            select_allowed_acp_option(&request),
            Some("allow".to_string())
        );
        assert_eq!(
            select_rejected_acp_option(&request),
            Some("reject".to_string())
        );
    }

    #[tokio::test]
    async fn session_gate_methods_delegate_to_inner() {
        let dir = tempfile::tempdir().unwrap();
        let session_store = Arc::new(SessionStore::new(dir.path().to_path_buf()));
        let metadata = Arc::new(SqliteSessionMetadata::new(sqlite_pool().await));
        create_test_session(&metadata, "main").await;
        let agent_state = Arc::new(FakeAgentState::default());
        let external_agents = fake_external_agents(Arc::clone(&metadata), agent_state);
        let inner = Arc::new(SyncTrackingChatService::default());
        let inner_chat: Arc<dyn ChatService> = inner.clone();
        let chat = ExternalAgentChatService::new(
            inner_chat,
            external_agents,
            test_gateway_state(),
            session_store,
            metadata,
        );

        assert_eq!(
            chat.wait_for_session_gate("main")
                .await
                .expect("wait_for_session_gate delegates to inner chat"),
            Some(SessionTerminal::Completed)
        );
        assert_eq!(
            chat.session_terminal("main")
                .await
                .expect("session_terminal delegates to inner chat"),
            Some(SessionTerminal::Cancelled)
        );
        assert_eq!(
            *inner
                .calls
                .lock()
                .unwrap_or_else(|error| error.into_inner()),
            vec!["wait_for_session_gate", "session_terminal"]
        );
    }

    #[tokio::test]
    async fn unbound_chat_send_sync_delegates_to_inner_send_sync() {
        let dir = tempfile::tempdir().unwrap();
        let session_store = Arc::new(SessionStore::new(dir.path().to_path_buf()));
        let metadata = Arc::new(SqliteSessionMetadata::new(sqlite_pool().await));
        create_test_session(&metadata, "main").await;
        let agent_state = Arc::new(FakeAgentState::default());
        let external_agents = fake_external_agents(Arc::clone(&metadata), agent_state);
        let inner = Arc::new(SyncTrackingChatService::default());
        let inner_chat: Arc<dyn ChatService> = inner.clone();
        let chat = ExternalAgentChatService::new(
            inner_chat,
            external_agents,
            test_gateway_state(),
            session_store,
            metadata,
        );

        let result = chat
            .send_sync(ChatSendSyncRequest::text("hello"), test_chat_context())
            .await
            .expect("send_sync delegates to inner chat");

        assert_eq!(result["source"], "send_sync");
        assert_eq!(
            *inner
                .calls
                .lock()
                .unwrap_or_else(|error| error.into_inner()),
            vec!["send_sync"]
        );
    }

    #[tokio::test]
    async fn bound_chat_send_sync_without_explicit_agent_uses_external_binding() {
        let dir = tempfile::tempdir().unwrap();
        let session_store = Arc::new(SessionStore::new(dir.path().to_path_buf()));
        let metadata = Arc::new(SqliteSessionMetadata::new(sqlite_pool().await));
        let agent_state = Arc::new(FakeAgentState::default());
        let external_agents = fake_external_agents(Arc::clone(&metadata), Arc::clone(&agent_state));
        external_agents
            .bind(serde_json::json!({ "sessionKey": "main", "kind": "codex" }))
            .await
            .expect("bind external agent");
        let inner = Arc::new(SyncTrackingChatService::default());
        let inner_chat: Arc<dyn ChatService> = inner.clone();
        let chat = ExternalAgentChatService::new(
            inner_chat,
            external_agents,
            test_gateway_state(),
            session_store,
            metadata,
        );

        let result = chat
            .send_sync(ChatSendSyncRequest::text("hello"), test_chat_context())
            .await
            .expect("bound send_sync uses external agent");

        assert_eq!(result["ok"], true);
        assert!(
            inner
                .calls
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .is_empty()
        );
        assert_eq!(agent_state.starts.load(Ordering::SeqCst), 1);
        assert_eq!(
            *agent_state
                .prompts
                .lock()
                .unwrap_or_else(|error| error.into_inner()),
            vec!["hello".to_string()]
        );
    }

    #[tokio::test]
    async fn bound_chat_send_sync_with_explicit_agent_delegates_to_inner() {
        let dir = tempfile::tempdir().unwrap();
        let session_store = Arc::new(SessionStore::new(dir.path().to_path_buf()));
        let metadata = Arc::new(SqliteSessionMetadata::new(sqlite_pool().await));
        let agent_state = Arc::new(FakeAgentState::default());
        let external_agents = fake_external_agents(Arc::clone(&metadata), Arc::clone(&agent_state));
        external_agents
            .bind(serde_json::json!({ "sessionKey": "main", "kind": "codex" }))
            .await
            .expect("bind external agent");
        let inner = Arc::new(SyncTrackingChatService::default());
        let inner_chat: Arc<dyn ChatService> = inner.clone();
        let chat = ExternalAgentChatService::new(
            inner_chat,
            external_agents,
            test_gateway_state(),
            session_store,
            metadata,
        );
        let mut context = test_chat_context();
        context.agent_id = Some("main".into());

        let result = chat
            .send_sync(ChatSendSyncRequest::text("hello"), context)
            .await
            .expect("explicit agent delegates to inner chat");

        assert_eq!(result["source"], "send_sync");
        assert_eq!(
            *inner
                .calls
                .lock()
                .unwrap_or_else(|error| error.into_inner()),
            vec!["send_sync"]
        );
        assert_eq!(agent_state.starts.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn bound_chat_send_sync_preserves_explicit_agent_inner_error() {
        let dir = tempfile::tempdir().unwrap();
        let session_store = Arc::new(SessionStore::new(dir.path().to_path_buf()));
        let metadata = Arc::new(SqliteSessionMetadata::new(sqlite_pool().await));
        let agent_state = Arc::new(FakeAgentState::default());
        let external_agents = fake_external_agents(Arc::clone(&metadata), Arc::clone(&agent_state));
        external_agents
            .bind(serde_json::json!({ "sessionKey": "main", "kind": "codex" }))
            .await
            .expect("bind external agent");
        let chat = ExternalAgentChatService::new(
            Arc::new(NoopChatService),
            external_agents,
            test_gateway_state(),
            session_store,
            metadata,
        );
        let mut context = test_chat_context();
        context.agent_id = Some("main".into());

        let error = chat
            .send_sync(ChatSendSyncRequest::text("hello"), context)
            .await
            .expect_err("inner error must be returned unchanged");

        assert_eq!(error.to_string(), "chat not configured");
        assert_eq!(agent_state.starts.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn bound_chat_send_reuses_live_external_session() {
        let dir = tempfile::tempdir().unwrap();
        let session_store = Arc::new(SessionStore::new(dir.path().to_path_buf()));
        let metadata = Arc::new(SqliteSessionMetadata::new(sqlite_pool().await));
        let agent_state = Arc::new(FakeAgentState::default());
        let external_agents = fake_external_agents(Arc::clone(&metadata), Arc::clone(&agent_state));
        external_agents
            .bind(serde_json::json!({ "sessionKey": "main", "kind": "codex" }))
            .await
            .expect("bind external agent");
        let chat = test_chat_service(
            Arc::clone(&external_agents),
            Arc::clone(&metadata),
            Arc::clone(&session_store),
        )
        .await;

        chat.send(ChatSendRequest::text("one"), test_chat_context())
            .await
            .expect("first send");
        chat.send(ChatSendRequest::text("two"), test_chat_context())
            .await
            .expect("second send");

        assert_eq!(agent_state.starts.load(Ordering::SeqCst), 1);
        assert_eq!(
            *agent_state
                .prompts
                .lock()
                .unwrap_or_else(|error| error.into_inner()),
            vec!["one".to_string(), "two".to_string()]
        );
        let history = session_store.read("main").await.expect("read history");
        assert_eq!(history.len(), 4);
        assert_eq!(history[1]["content"], "reply to one");
        assert_eq!(history[3]["provider"], "external-agent");
        assert_eq!(
            metadata
                .get("main")
                .await
                .unwrap()
                .and_then(|entry| entry.external_session_id().map(str::to_string)),
            Some("fake-session-1".to_string())
        );
    }

    #[tokio::test]
    async fn idle_live_external_sessions_are_evicted_before_reuse() {
        let metadata = Arc::new(SqliteSessionMetadata::new(sqlite_pool().await));
        let agent_state = Arc::new(FakeAgentState::default());
        let external_agents = fake_external_agents(Arc::clone(&metadata), Arc::clone(&agent_state));
        external_agents
            .bind(serde_json::json!({ "sessionKey": "main", "kind": "codex" }))
            .await
            .expect("bind external agent");

        let first = external_agents
            .session_for_binding("main", AgentTransportKind::Codex)
            .await
            .expect("first live session");
        drop(first);
        {
            let mut live_sessions = external_agents.live_sessions.lock().await;
            for entry in live_sessions.values_mut() {
                entry.last_used = std::time::Instant::now() - LIVE_SESSION_IDLE_TTL;
            }
        }

        let second = external_agents
            .session_for_binding("main", AgentTransportKind::Codex)
            .await
            .expect("second live session");
        drop(second);

        assert_eq!(agent_state.shutdowns.load(Ordering::SeqCst), 1);
        assert_eq!(agent_state.starts.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn send_failure_evicts_live_external_session() {
        let dir = tempfile::tempdir().unwrap();
        let session_store = Arc::new(SessionStore::new(dir.path().to_path_buf()));
        let metadata = Arc::new(SqliteSessionMetadata::new(sqlite_pool().await));
        let agent_state = Arc::new(FakeAgentState::default());
        let external_agents = fake_external_agents(Arc::clone(&metadata), Arc::clone(&agent_state));
        external_agents
            .bind(serde_json::json!({ "sessionKey": "main", "kind": "codex" }))
            .await
            .expect("bind external agent");
        let chat = test_chat_service(
            Arc::clone(&external_agents),
            Arc::clone(&metadata),
            Arc::clone(&session_store),
        )
        .await;

        chat.send(ChatSendRequest::text("one"), test_chat_context())
            .await
            .expect("first send");
        let error = chat
            .send(ChatSendRequest::text("fail"), test_chat_context())
            .await
            .expect_err("failing send should error");
        assert_eq!(error.to_string(), "fake send failure");
        assert_eq!(agent_state.shutdowns.load(Ordering::SeqCst), 1);

        chat.send(ChatSendRequest::text("two"), test_chat_context())
            .await
            .expect("send after eviction");
        assert_eq!(agent_state.starts.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn error_event_evicts_live_external_session() {
        let dir = tempfile::tempdir().unwrap();
        let session_store = Arc::new(SessionStore::new(dir.path().to_path_buf()));
        let metadata = Arc::new(SqliteSessionMetadata::new(sqlite_pool().await));
        let agent_state = Arc::new(FakeAgentState::default());
        let external_agents = fake_external_agents(Arc::clone(&metadata), Arc::clone(&agent_state));
        external_agents
            .bind(serde_json::json!({ "sessionKey": "main", "kind": "codex" }))
            .await
            .expect("bind external agent");
        let chat = test_chat_service(
            Arc::clone(&external_agents),
            Arc::clone(&metadata),
            Arc::clone(&session_store),
        )
        .await;

        let error = chat
            .send(ChatSendRequest::text("event-error"), test_chat_context())
            .await
            .expect_err("error event should fail chat send");
        assert_eq!(error.to_string(), "fake event failure");
        assert_eq!(agent_state.shutdowns.load(Ordering::SeqCst), 1);

        chat.send(ChatSendRequest::text("two"), test_chat_context())
            .await
            .expect("send after event-error eviction");
        assert_eq!(agent_state.starts.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn error_event_persists_visible_partial_at_reserved_index() {
        let dir = tempfile::tempdir().unwrap();
        let session_store = Arc::new(SessionStore::new(dir.path().to_path_buf()));
        let metadata = Arc::new(SqliteSessionMetadata::new(sqlite_pool().await));
        let agent_state = Arc::new(FakeAgentState::default());
        let external_agents = fake_external_agents(Arc::clone(&metadata), agent_state);
        external_agents
            .bind(serde_json::json!({ "sessionKey": "main", "kind": "codex" }))
            .await
            .expect("bind external agent");
        let chat = test_chat_service(
            Arc::clone(&external_agents),
            Arc::clone(&metadata),
            Arc::clone(&session_store),
        )
        .await;

        let error = chat
            .send(ChatSendRequest::text("partial-error"), test_chat_context())
            .await
            .expect_err("partial stream error should fail chat send");

        assert_eq!(error.to_string(), "fake partial failure");
        let history = session_store.read("main").await.expect("read history");
        assert_eq!(history.len(), 2);
        assert_eq!(history[0]["role"], "user");
        assert_eq!(history[1]["role"], "assistant");
        assert_eq!(history[1]["content"], "partial reply");
    }

    #[tokio::test]
    async fn bound_chat_send_persists_external_token_usage() {
        let dir = tempfile::tempdir().unwrap();
        let session_store = Arc::new(SessionStore::new(dir.path().to_path_buf()));
        let metadata = Arc::new(SqliteSessionMetadata::new(sqlite_pool().await));
        let agent_state = Arc::new(FakeAgentState::default());
        let external_agents = fake_external_agents(Arc::clone(&metadata), agent_state);
        external_agents
            .bind(serde_json::json!({ "sessionKey": "main", "kind": "codex" }))
            .await
            .expect("bind external agent");
        let chat = test_chat_service(
            Arc::clone(&external_agents),
            Arc::clone(&metadata),
            Arc::clone(&session_store),
        )
        .await;

        chat.send(ChatSendRequest::text("usage"), test_chat_context())
            .await
            .expect("send with usage");

        let history = session_store.read("main").await.expect("read history");
        assert_eq!(history[1]["inputTokens"], 7);
        assert_eq!(history[1]["outputTokens"], 11);
    }

    #[tokio::test]
    async fn external_agent_item_positions_follow_arrival_order() {
        let dir = tempfile::tempdir().unwrap();
        let session_store = Arc::new(SessionStore::new(dir.path().to_path_buf()));
        let metadata = Arc::new(SqliteSessionMetadata::new(sqlite_pool().await));
        let agent_state = Arc::new(FakeAgentState::default());
        let external_agents = fake_external_agents(Arc::clone(&metadata), agent_state);
        external_agents
            .bind(serde_json::json!({ "sessionKey": "main", "kind": "codex" }))
            .await
            .expect("bind external agent");
        let chat = test_chat_service(
            Arc::clone(&external_agents),
            Arc::clone(&metadata),
            Arc::clone(&session_store),
        )
        .await;

        chat.send(ChatSendRequest::text("thinking-first"), test_chat_context())
            .await
            .expect("send thinking-first");

        let history = session_store.read("main").await.expect("read history");
        let items = history[1]["providerItems"]
            .as_array()
            .expect("assistant message carries canonical items");
        // Reasoning arrived first, so it holds the first slot. A hardcoded
        // position would put it after the message regardless of arrival order.
        assert_eq!(items.len(), 2);
        assert_eq!(items[0]["id"], "rs_0");
        assert_eq!(items[0]["position"], 0);
        assert_eq!(items[1]["id"], "msg_0");
        assert_eq!(items[1]["position"], 1);
        assert!(
            history[1]["segmentId"]
                .as_str()
                .is_some_and(|id| id.starts_with("seg_"))
        );
    }

    #[tokio::test]
    async fn unbind_shuts_down_live_external_session() {
        let dir = tempfile::tempdir().unwrap();
        let session_store = Arc::new(SessionStore::new(dir.path().to_path_buf()));
        let metadata = Arc::new(SqliteSessionMetadata::new(sqlite_pool().await));
        let agent_state = Arc::new(FakeAgentState::default());
        let external_agents = fake_external_agents(Arc::clone(&metadata), Arc::clone(&agent_state));
        external_agents
            .bind(serde_json::json!({ "sessionKey": "main", "kind": "codex" }))
            .await
            .expect("bind external agent");
        let chat = test_chat_service(
            Arc::clone(&external_agents),
            Arc::clone(&metadata),
            Arc::clone(&session_store),
        )
        .await;
        chat.send(ChatSendRequest::text("one"), test_chat_context())
            .await
            .expect("send starts live session");

        external_agents
            .unbind(serde_json::json!({ "sessionKey": "main" }))
            .await
            .expect("unbind external agent");

        assert_eq!(agent_state.shutdowns.load(Ordering::SeqCst), 1);
        assert!(external_agents.live_sessions.lock().await.is_empty());
    }
}
