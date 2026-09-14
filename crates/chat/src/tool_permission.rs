//! In-memory tool-call permission waits. Nothing here is persisted.

use std::{
    collections::HashMap,
    future::Future,
    sync::{
        Mutex, PoisonError,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

use {
    chelix_agents::runner::{
        AgentRunError, OnToolPermission, ToolPermissionDecision, ToolPermissionPhase,
        ToolPermissionRequest,
    },
    chelix_sessions::metadata::{SqliteSessionMetadata, ToolPermissionMode, ToolPermissionType},
    serde::{Deserialize, Serialize},
    tokio::sync::{Notify, RwLock, oneshot},
    tokio_util::sync::CancellationToken,
};

use crate::runtime::ChatRuntime;

const EVENT_REQUESTED: &str = "tool.permission.requested";
const EVENT_RESOLVED: &str = "tool.permission.resolved";

fn lock_mutex<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Wall-clock pause used only by the agent-run timeout watchdog.
pub struct AgentTimeoutClock {
    paused_count: AtomicUsize,
    paused_total: Mutex<Duration>,
    pause_started: Mutex<Option<Instant>>,
    notify: Notify,
}

impl AgentTimeoutClock {
    #[must_use]
    pub fn new() -> Self {
        Self {
            paused_count: AtomicUsize::new(0),
            paused_total: Mutex::new(Duration::ZERO),
            pause_started: Mutex::new(None),
            notify: Notify::new(),
        }
    }

    pub fn pause(&self) {
        let previous = self.paused_count.fetch_add(1, Ordering::SeqCst);
        if previous == 0 {
            *lock_mutex(&self.pause_started) = Some(Instant::now());
            self.notify.notify_waiters();
        }
    }

    pub fn resume(&self) {
        let previous = self.paused_count.fetch_sub(1, Ordering::SeqCst);
        if previous == 1 {
            if let Some(started) = lock_mutex(&self.pause_started).take() {
                *lock_mutex(&self.paused_total) += started.elapsed();
            }
            self.notify.notify_waiters();
        }
    }

    #[must_use]
    pub fn is_paused(&self) -> bool {
        self.paused_count.load(Ordering::SeqCst) > 0
    }

    #[must_use]
    pub fn paused_duration(&self) -> Duration {
        let extra = lock_mutex(&self.pause_started)
            .as_ref()
            .map(Instant::elapsed)
            .unwrap_or(Duration::ZERO);
        *lock_mutex(&self.paused_total) + extra
    }

    pub async fn notified(&self) {
        self.notify.notified().await;
    }
}

impl Default for AgentTimeoutClock {
    fn default() -> Self {
        Self::new()
    }
}

struct PauseGuard {
    clock: std::sync::Arc<AgentTimeoutClock>,
}

impl PauseGuard {
    fn enter(clock: &std::sync::Arc<AgentTimeoutClock>) -> Self {
        clock.pause();
        Self {
            clock: std::sync::Arc::clone(clock),
        }
    }
}

impl Drop for PauseGuard {
    fn drop(&mut self) {
        self.clock.resume();
    }
}

/// Serializable pending permission shown to late-joining clients.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PendingToolPermission {
    pub session_key: String,
    pub run_id: String,
    pub tool_call_id: String,
    pub tool_name: String,
    pub phase: ToolPermissionPhase,
}

struct PendingSlot {
    view: PendingToolPermission,
    tx: oneshot::Sender<ToolPermissionDecision>,
}

/// Session-scoped in-memory permission waits.
pub struct ToolPermissionManager {
    pending: RwLock<HashMap<String, PendingSlot>>,
}

impl ToolPermissionManager {
    #[must_use]
    pub fn new() -> Self {
        Self {
            pending: RwLock::new(HashMap::new()),
        }
    }

    fn request_key(session_key: &str, tool_call_id: &str, phase: ToolPermissionPhase) -> String {
        format!("{session_key}:{tool_call_id}:{phase:?}")
    }

    pub async fn pending_for_session(&self, session_key: &str) -> Vec<PendingToolPermission> {
        let mut requests: Vec<_> = self
            .pending
            .read()
            .await
            .values()
            .filter(|slot| slot.view.session_key == session_key)
            .map(|slot| slot.view.clone())
            .collect();
        requests.sort_by(|left, right| {
            left.tool_call_id
                .cmp(&right.tool_call_id)
                .then_with(|| format!("{:?}", left.phase).cmp(&format!("{:?}", right.phase)))
        });
        requests
    }

    pub async fn resolve(
        &self,
        session_key: &str,
        tool_call_id: &str,
        phase: ToolPermissionPhase,
        decision: ToolPermissionDecision,
    ) -> Result<PendingToolPermission, String> {
        if let ToolPermissionDecision::Deny { feedback } = &decision
            && feedback.trim().is_empty()
        {
            return Err("deny feedback must not be empty".to_owned());
        }
        let key = Self::request_key(session_key, tool_call_id, phase);
        let Some(slot) = self.pending.write().await.remove(&key) else {
            return Err("no pending tool permission request".to_owned());
        };
        let view = slot.view.clone();
        let _ = slot.tx.send(decision);
        Ok(view)
    }

    pub async fn drop_session(&self, session_key: &str) -> Vec<PendingToolPermission> {
        let mut pending = self.pending.write().await;
        let keys: Vec<_> = pending
            .iter()
            .filter(|(_, slot)| slot.view.session_key == session_key)
            .map(|(key, _)| key.clone())
            .collect();
        let mut dropped = Vec::new();
        for key in keys {
            if let Some(slot) = pending.remove(&key) {
                dropped.push(slot.view);
            }
        }
        dropped
    }

    async fn register(
        &self,
        run_id: String,
        request: &ToolPermissionRequest,
    ) -> (
        String,
        PendingToolPermission,
        oneshot::Receiver<ToolPermissionDecision>,
    ) {
        let key = Self::request_key(&request.session_key, &request.tool_call_id, request.phase);
        let (tx, rx) = oneshot::channel();
        let view = PendingToolPermission {
            session_key: request.session_key.clone(),
            run_id,
            tool_call_id: request.tool_call_id.clone(),
            tool_name: request.tool_name.clone(),
            phase: request.phase,
        };
        self.pending.write().await.insert(key.clone(), PendingSlot {
            view: view.clone(),
            tx,
        });
        (key, view, rx)
    }

    async fn recv(
        &self,
        key: String,
        rx: oneshot::Receiver<ToolPermissionDecision>,
    ) -> Result<ToolPermissionDecision, AgentRunError> {
        match rx.await {
            Ok(decision) => {
                self.pending.write().await.remove(&key);
                Ok(decision)
            },
            Err(_) => {
                self.pending.write().await.remove(&key);
                Err(AgentRunError::Cancelled)
            },
        }
    }
}

impl Default for ToolPermissionManager {
    fn default() -> Self {
        Self::new()
    }
}

/// Runtime wiring for the optional permission callback.
#[derive(Clone)]
pub struct ToolPermissionRuntime {
    pub manager: std::sync::Arc<ToolPermissionManager>,
    pub metadata: std::sync::Arc<SqliteSessionMetadata>,
}

pub fn permission_callback(
    runtime: ToolPermissionRuntime,
    run_id: String,
    state: std::sync::Arc<dyn ChatRuntime>,
    clock: std::sync::Arc<AgentTimeoutClock>,
    cancellation_token: CancellationToken,
) -> OnToolPermission {
    std::sync::Arc::new(move |request: ToolPermissionRequest| {
        let runtime = runtime.clone();
        let run_id = run_id.clone();
        let state = std::sync::Arc::clone(&state);
        let clock = std::sync::Arc::clone(&clock);
        let cancellation_token = cancellation_token.clone();
        Box::pin(async move {
            gate_permission(runtime, run_id, state, clock, cancellation_token, request).await
        })
    })
}

async fn gate_permission(
    runtime: ToolPermissionRuntime,
    run_id: String,
    state: std::sync::Arc<dyn ChatRuntime>,
    clock: std::sync::Arc<AgentTimeoutClock>,
    cancellation_token: CancellationToken,
    request: ToolPermissionRequest,
) -> Result<ToolPermissionDecision, AgentRunError> {
    let entry = runtime
        .metadata
        .get(&request.session_key)
        .await
        .map_err(|error| AgentRunError::Other(anyhow::anyhow!(error.to_string())))?
        .ok_or_else(|| {
            AgentRunError::Other(anyhow::anyhow!(
                "session '{}' not found for tool permission",
                request.session_key
            ))
        })?;
    if entry.tool_permission_mode == ToolPermissionMode::Auto {
        return Ok(ToolPermissionDecision::Approve);
    }
    if entry.tool_permission_type != ToolPermissionType::Manual {
        return Err(AgentRunError::Other(anyhow::anyhow!(
            "unsupported tool permission type"
        )));
    }

    let _pause = PauseGuard::enter(&clock);
    let (key, view, rx) = runtime.manager.register(run_id, &request).await;
    state
        .broadcast(
            EVENT_REQUESTED,
            serde_json::to_value(&view)
                .map_err(|error| AgentRunError::Other(anyhow::anyhow!(error.to_string())))?,
        )
        .await;

    let wait = runtime.manager.recv(key, rx);
    let decision = match cancellation_token.run_until_cancelled(wait).await {
        Some(result) => result?,
        None => {
            let dropped = runtime.manager.drop_session(&view.session_key).await;
            broadcast_cancelled(&state, &dropped).await;
            return Err(AgentRunError::Cancelled);
        },
    };
    state
        .broadcast(
            EVENT_RESOLVED,
            serde_json::json!({
                "sessionKey": view.session_key,
                "runId": view.run_id,
                "toolCallId": view.tool_call_id,
                "toolName": view.tool_name,
                "phase": view.phase,
                "decision": decision,
            }),
        )
        .await;
    Ok(decision)
}

async fn broadcast_cancelled(
    state: &std::sync::Arc<dyn ChatRuntime>,
    dropped: &[PendingToolPermission],
) {
    for view in dropped {
        state
            .broadcast(
                EVENT_RESOLVED,
                serde_json::json!({
                    "sessionKey": view.session_key,
                    "runId": view.run_id,
                    "toolCallId": view.tool_call_id,
                    "toolName": view.tool_name,
                    "phase": view.phase,
                    "decision": "cancelled",
                }),
            )
            .await;
    }
}

pub async fn await_with_paused_timeout<F>(
    timeout_secs: u64,
    started: Instant,
    clock: &AgentTimeoutClock,
    cancellation_token: &CancellationToken,
    future: F,
) -> Result<chelix_agents::runner::AgentRunResult, AgentRunError>
where
    F: Future<Output = Result<chelix_agents::runner::AgentRunResult, AgentRunError>>,
{
    if timeout_secs == 0 {
        return future.await;
    }
    let timeout = Duration::from_secs(timeout_secs);
    tokio::pin!(future);
    let watchdog = async {
        loop {
            let notified = clock.notified();
            tokio::pin!(notified);
            let unpaused = started.elapsed().saturating_sub(clock.paused_duration());
            if unpaused >= timeout {
                return;
            }
            if clock.is_paused() {
                notified.await;
                continue;
            }
            let remaining = timeout.saturating_sub(unpaused);
            tokio::select! {
                () = notified => {},
                () = tokio::time::sleep(remaining) => {},
            }
        }
    };
    tokio::select! {
        result = &mut future => result,
        () = watchdog => {
            cancellation_token.cancel();
            if let Err(error) = future.await
                && !matches!(error, AgentRunError::Cancelled)
            {
                return Err(AgentRunError::Other(anyhow::anyhow!(
                    "agent run timed out after {timeout_secs}s; cancellation failed: {error}"
                )));
            }
            Err(AgentRunError::Other(anyhow::anyhow!(
                "agent run timed out after {timeout_secs}s"
            )))
        },
    }
}

#[allow(clippy::unwrap_used, clippy::expect_used)]
#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn clock_pause_refcount_does_not_resume_early() {
        let clock = AgentTimeoutClock::new();
        clock.pause();
        clock.pause();
        assert!(clock.is_paused());
        clock.resume();
        assert!(clock.is_paused());
        clock.resume();
        assert!(!clock.is_paused());
    }

    #[tokio::test]
    async fn resolve_delivers_decision() {
        let manager = std::sync::Arc::new(ToolPermissionManager::new());
        let wait = {
            let manager = std::sync::Arc::clone(&manager);
            tokio::spawn(async move {
                let request = ToolPermissionRequest {
                    session_key: "session:a".into(),
                    tool_call_id: "call-1".into(),
                    tool_name: "example".into(),
                    phase: ToolPermissionPhase::BeforeExecution,
                };
                let (key, _view, rx) = manager.register("run-1".into(), &request).await;
                manager.recv(key, rx).await
            })
        };
        loop {
            if !manager.pending_for_session("session:a").await.is_empty() {
                break;
            }
            tokio::task::yield_now().await;
        }
        manager
            .resolve(
                "session:a",
                "call-1",
                ToolPermissionPhase::BeforeExecution,
                ToolPermissionDecision::Approve,
            )
            .await
            .unwrap();
        let decision = wait.await.unwrap().unwrap();
        assert_eq!(decision, ToolPermissionDecision::Approve);
        assert!(manager.pending_for_session("session:a").await.is_empty());
    }

    #[tokio::test]
    async fn empty_deny_feedback_is_rejected() {
        let manager = ToolPermissionManager::new();
        let error = manager
            .resolve(
                "session:a",
                "call-1",
                ToolPermissionPhase::BeforeExecution,
                ToolPermissionDecision::Deny {
                    feedback: "  ".into(),
                },
            )
            .await
            .unwrap_err();
        assert_eq!(error, "deny feedback must not be empty");
    }

    fn dummy_run_result() -> chelix_agents::runner::AgentRunResult {
        chelix_agents::runner::AgentRunResult {
            output: chelix_agents::runner::AssistantIterationOutput::default(),
            final_text_source: chelix_agents::runner::FinalTextSource::NewSegment,
            iterations: 0,
            tool_calls_made: 0,
            usage: chelix_agents::model::Usage::default(),
            request_usage: chelix_agents::model::Usage::default(),
            raw_llm_responses: Vec::new(),
        }
    }

    #[tokio::test]
    async fn paused_clock_does_not_timeout_the_run() {
        let clock = AgentTimeoutClock::new();
        let token = CancellationToken::new();
        let (tx, rx) = oneshot::channel::<()>();
        clock.pause();
        let started = Instant::now();
        let fut = async move {
            rx.await.map_err(|_| AgentRunError::Cancelled)?;
            Ok(dummy_run_result())
        };
        let wait = await_with_paused_timeout(1, started, &clock, &token, fut);
        tokio::pin!(wait);
        tokio::select! {
            result = &mut wait => panic!("run finished while paused: {result:?}"),
            () = tokio::time::sleep(Duration::from_millis(1200)) => {},
        }
        clock.resume();
        tx.send(()).unwrap();
        assert!(wait.await.is_ok());
    }
}
