//! Coordinates `StopSession` with turns that already hold a permit or are about to enqueue.

use std::{
    collections::{HashMap, HashSet},
    sync::{Arc, Mutex},
};

use {tokio::sync::Notify, tokio_util::sync::CancellationToken};

use chelix_service_traits::{ServiceError, SessionTurnPermit};

use crate::types::ChatRunOutcome;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Phase {
    Acquiring,
    HoldingPermit,
    Enqueueing,
    Running,
}

pub(in crate::service) struct RunDone {
    token: CancellationToken,
    result: Mutex<Option<Result<(), String>>>,
    notify: Notify,
}

struct Guard {
    session_key: String,
    phase: Phase,
    cancel: CancellationToken,
    run_id: Option<String>,
}

struct GateState {
    stop_depth: HashMap<String, usize>,
    suppressed: HashSet<String>,
    epoch: HashMap<String, u64>,
    guards: HashMap<u64, Guard>,
    next_id: u64,
    permit_holders: HashMap<String, usize>,
    runs: HashMap<String, Arc<RunDone>>,
    current_run: HashMap<String, String>,
    finished_during_stop: HashMap<String, Vec<Result<(), String>>>,
}

/// Shared stop coordination for one `LiveChatService`.
pub(in crate::service) struct StopGate {
    state: Arc<Mutex<GateState>>,
    notify: Arc<Notify>,
}

/// Drop removes an uncommitted send guard.
pub(in crate::service) struct SendGuard {
    id: u64,
    gate: Arc<StopGate>,
    active: bool,
}

/// Drops the session permit before observers are told the holder count changed.
pub(in crate::service) struct PermitRelease {
    session_key: String,
    state: Arc<Mutex<GateState>>,
    notify: Arc<Notify>,
    permit: Option<SessionTurnPermit>,
}

impl StopGate {
    pub(in crate::service) fn new() -> Arc<Self> {
        Arc::new(Self {
            state: Arc::new(Mutex::new(GateState {
                stop_depth: HashMap::new(),
                suppressed: HashSet::new(),
                current_run: HashMap::new(),
                epoch: HashMap::new(),
                guards: HashMap::new(),
                next_id: 1,
                permit_holders: HashMap::new(),
                runs: HashMap::new(),
                finished_during_stop: HashMap::new(),
            })),
            notify: Arc::new(Notify::new()),
        })
    }

    pub(in crate::service) fn begin_send(
        self: &Arc<Self>,
        session_key: &str,
    ) -> Result<SendGuard, ServiceError> {
        let mut state = lock(&self.state);
        if state.stop_depth.get(session_key).copied().unwrap_or(0) > 0 {
            return Err(ServiceError::message("session stop is in progress"));
        }
        let id = state.next_id;
        state.next_id = state.next_id.saturating_add(1);
        state.guards.insert(id, Guard {
            session_key: session_key.to_string(),
            phase: Phase::Acquiring,
            cancel: CancellationToken::new(),
            run_id: None,
        });
        drop(state);
        Ok(SendGuard {
            id,
            gate: Arc::clone(self),
            active: true,
        })
    }

    pub(in crate::service) fn abandon(&self, id: u64) {
        let mut state = lock(&self.state);
        state.guards.remove(&id);
        drop(state);
        self.notify.notify_waiters();
    }

    pub(in crate::service) fn attach_permit(
        &self,
        guard: &mut SendGuard,
        permit: SessionTurnPermit,
    ) -> Result<PermitRelease, (SessionTurnPermit, ServiceError)> {
        let mut state = lock(&self.state);
        let Some(entry) = state.guards.get(&guard.id) else {
            return Err((
                permit,
                ServiceError::message("session start guard disappeared"),
            ));
        };
        let session_key = entry.session_key.clone();
        let cancelled = entry.cancel.is_cancelled();
        let stopping = state.stop_depth.get(&session_key).copied().unwrap_or(0) > 0;
        if cancelled || stopping {
            state.guards.remove(&guard.id);
            guard.active = false;
            drop(state);
            self.notify.notify_waiters();
            return Err((
                permit,
                ServiceError::message("session stop cancelled the turn before it started"),
            ));
        }
        if let Some(entry) = state.guards.get_mut(&guard.id) {
            entry.phase = Phase::HoldingPermit;
        }
        *state.permit_holders.entry(session_key.clone()).or_insert(0) += 1;
        drop(state);
        Ok(PermitRelease {
            session_key,
            state: Arc::clone(&self.state),
            notify: Arc::clone(&self.notify),
            permit: Some(permit),
        })
    }

    pub(in crate::service) fn begin_enqueue(&self, guard: &SendGuard) -> Result<(), ServiceError> {
        let mut state = lock(&self.state);
        let Some(entry) = state.guards.get(&guard.id) else {
            return Err(ServiceError::message("session enqueue guard disappeared"));
        };
        let session_key = entry.session_key.clone();
        let cancelled = entry.cancel.is_cancelled();
        let stopping = state.stop_depth.get(&session_key).copied().unwrap_or(0) > 0;
        if stopping || cancelled {
            state.guards.remove(&guard.id);
            drop(state);
            self.notify.notify_waiters();
            return Err(ServiceError::message(
                "session stop cancelled prompt enqueue",
            ));
        }
        if let Some(entry) = state.guards.get_mut(&guard.id) {
            entry.phase = Phase::Enqueueing;
        }
        *state.epoch.entry(session_key).or_insert(0) += 1;
        Ok(())
    }

    pub(in crate::service) fn allow_queue_broadcast(&self, guard: &SendGuard) -> bool {
        let state = lock(&self.state);
        let Some(entry) = state.guards.get(&guard.id) else {
            return false;
        };
        let session_key = entry.session_key.clone();
        state.stop_depth.get(&session_key).copied().unwrap_or(0) == 0
    }

    pub(in crate::service) fn finish_enqueue(&self, guard: &mut SendGuard) {
        self.abandon(guard.id);
        guard.active = false;
    }

    pub(in crate::service) fn confirm_start(
        &self,
        guard: &mut SendGuard,
    ) -> Result<(), ServiceError> {
        let mut state = lock(&self.state);
        let Some(entry) = state.guards.get(&guard.id) else {
            return Err(ServiceError::message("session start guard disappeared"));
        };
        let session_key = entry.session_key.clone();
        let cancelled = entry.cancel.is_cancelled();
        if cancelled || state.stop_depth.get(&session_key).copied().unwrap_or(0) > 0 {
            state.guards.remove(&guard.id);
            guard.active = false;
            drop(state);
            self.notify.notify_waiters();
            return Err(ServiceError::message(
                "session stop cancelled the turn before it started",
            ));
        }
        Ok(())
    }

    pub(in crate::service) fn publish_run(
        &self,
        active: &mut HashMap<String, CancellationToken>,
        guard: &mut SendGuard,
        session_key: &str,
        run_id: &str,
        token: CancellationToken,
    ) {
        let mut state = lock(&self.state);
        if state.stop_depth.get(session_key).copied().unwrap_or(0) > 0 {
            token.cancel();
        }
        state.runs.insert(
            run_id.to_string(),
            Arc::new(RunDone {
                token: token.clone(),
                result: Mutex::new(None),
                notify: Notify::new(),
            }),
        );
        state
            .current_run
            .insert(session_key.to_string(), run_id.to_string());
        if let Some(entry) = state.guards.get_mut(&guard.id) {
            entry.phase = Phase::Running;
            entry.run_id = Some(run_id.to_string());
        }
        active.insert(run_id.to_string(), token);
        guard.active = false;
    }

    pub(in crate::service) fn capture_run(
        &self,
        active: &HashMap<String, CancellationToken>,
        run_id: &str,
    ) -> Option<Arc<RunDone>> {
        let state = lock(&self.state);
        if !active.contains_key(run_id) {
            return None;
        }
        state.runs.get(run_id).cloned()
    }

    pub(in crate::service) fn finish_run(
        &self,
        active: &mut HashMap<String, CancellationToken>,
        run_id: &str,
        result: Result<(), String>,
    ) {
        let mut state = lock(&self.state);
        let session_key = state
            .guards
            .values()
            .find(|guard| guard.run_id.as_deref() == Some(run_id))
            .map(|guard| guard.session_key.clone())
            .or_else(|| {
                state
                    .current_run
                    .iter()
                    .find(|(_, current)| *current == run_id)
                    .map(|(key, _)| key.clone())
            });
        let slot = state.runs.get(run_id).cloned();
        if let Some(slot) = slot {
            *lock(&slot.result) = Some(result.clone());
            slot.notify.notify_waiters();
        }
        if let Some(session_key) = session_key
            && state.stop_depth.get(&session_key).copied().unwrap_or(0) > 0
        {
            state
                .finished_during_stop
                .entry(session_key)
                .or_default()
                .push(result);
        }
        state.runs.remove(run_id);
        state.current_run.retain(|_, current| current != run_id);
        active.remove(run_id);
        let finished = state
            .guards
            .iter()
            .find_map(|(id, guard)| (guard.run_id.as_deref() == Some(run_id)).then_some(*id));
        if let Some(id) = finished {
            state.guards.remove(&id);
        }
        drop(state);
        self.notify.notify_waiters();
    }

    pub(in crate::service) fn report_outcome(
        outcome: &ChatRunOutcome,
        detail: Option<String>,
    ) -> Result<(), String> {
        match outcome {
            ChatRunOutcome::Failed => Err(detail.unwrap_or_else(|| "run failed".to_string())),
            ChatRunOutcome::Cancelled | ChatRunOutcome::Completed(_) => Ok(()),
        }
    }

    pub(in crate::service) fn is_suppressed(&self, session_key: &str) -> bool {
        lock(&self.state).suppressed.contains(session_key)
    }

    #[cfg(test)]
    pub(in crate::service) fn stop_depth(&self, session_key: &str) -> usize {
        lock(&self.state)
            .stop_depth
            .get(session_key)
            .copied()
            .unwrap_or(0)
    }

    pub(in crate::service) fn snapshot_stop(
        &self,
        session_key: &str,
        expected_epoch: Option<u64>,
        seen: &mut usize,
    ) -> StopSnapshot {
        let state = lock(&self.state);
        let guards = state
            .guards
            .values()
            .all(|guard| guard.session_key != session_key);
        let holders = state.permit_holders.get(session_key).copied().unwrap_or(0);
        let epoch = state.epoch.get(session_key).copied().unwrap_or(0);
        let idle =
            guards && holders == 0 && expected_epoch.is_none_or(|expected| expected == epoch);
        let error = state
            .finished_during_stop
            .get(session_key)
            .and_then(|items| {
                let fresh = items.get(*seen..).unwrap_or(&[]);
                *seen = items.len();
                fresh
                    .iter()
                    .find_map(|result| result.as_ref().err().cloned())
            });
        StopSnapshot { idle, error, epoch }
    }

    pub(in crate::service) fn end_stop(&self, session_key: &str) {
        let mut state = lock(&self.state);
        end_stop_locked(&mut state, session_key);
        drop(state);
        self.notify.notify_waiters();
    }

    /// Returns whether this call owns a stop. `Stale` performs no side effects.
    pub(in crate::service) fn choose_stop(
        &self,
        session_key: &str,
        expected: Option<&str>,
        external_matches: bool,
    ) -> Option<Option<String>> {
        let mut state = lock(&self.state);
        let local = state.current_run.get(session_key).cloned();
        let matches = match expected {
            None => true,
            Some(expected) => local.as_deref() == Some(expected) || external_matches,
        };
        if !matches {
            return None;
        }
        begin_stop_locked(&mut state, session_key);
        Some(local)
    }

    pub(in crate::service) fn arm_next_batch(
        self: &Arc<Self>,
        session_key: &str,
        guard: &mut SendGuard,
    ) -> Result<(), ServiceError> {
        let mut state = lock(&self.state);
        if state.stop_depth.get(session_key).copied().unwrap_or(0) > 0
            || state.suppressed.contains(session_key)
        {
            return Err(ServiceError::message("session queue is suppressed"));
        }
        let id = state.next_id;
        state.next_id = state.next_id.saturating_add(1);
        state.guards.insert(id, Guard {
            session_key: session_key.to_string(),
            phase: Phase::HoldingPermit,
            cancel: CancellationToken::new(),
            run_id: None,
        });
        drop(state);
        guard.id = id;
        guard.gate = Arc::clone(self);
        guard.active = true;
        Ok(())
    }

    pub(in crate::service) fn notified(&self) -> tokio::sync::futures::Notified<'_> {
        self.notify.notified()
    }
}

impl super::types::LiveChatService {
    pub async fn current_local_run(&self, key: &str) -> Option<String> {
        self.active_runs_by_session.read().await.get(key).cloned()
    }

    pub async fn stop_run(
        &self,
        run_id: String,
    ) -> Result<chelix_service_traits::StopSessionOutcome, ServiceError> {
        let active = self.active_runs.read().await;
        let slot = self.stop_gate.capture_run(&active, &run_id);
        drop(active);
        let Some(slot) = slot else {
            return Ok(chelix_service_traits::StopSessionOutcome {
                cancelled: false,
                run_id: Some(run_id),
            });
        };
        let cancelled = if slot.token.is_cancelled() {
            false
        } else {
            slot.token.cancel();
            true
        };
        let result = wait_run_done(slot).await;
        result.map_err(ServiceError::message)?;
        Ok(chelix_service_traits::StopSessionOutcome {
            cancelled,
            run_id: Some(run_id),
        })
    }

    pub async fn stop_current_session(
        &self,
        key: &str,
        expected: Option<&str>,
        external_matches: bool,
    ) -> Result<chelix_service_traits::StopSessionOutcome, ServiceError> {
        let Some(local_run) = self.stop_gate.choose_stop(key, expected, external_matches) else {
            return Ok(chelix_service_traits::StopSessionOutcome {
                cancelled: false,
                run_id: expected.map(str::to_string),
            });
        };
        let mut stop_depth = StopDepthGuard {
            gate: &self.stop_gate,
            key: key.to_string(),
            active: true,
        };
        let mut cancelled = false;
        let mut run_id = local_run;
        let cancel_local = expected.is_none() || run_id.as_deref() == expected;
        let mut seen = 0;
        loop {
            let notified = self.stop_gate.notified();
            if cancel_local
                && let Some(current) = self.current_local_run(key).await
                && expected.is_none_or(|expected| expected == current)
            {
                let active = self.active_runs.read().await;
                let slot = self.stop_gate.capture_run(&active, &current);
                drop(active);
                if let Some(slot) = slot {
                    if !slot.token.is_cancelled() {
                        slot.token.cancel();
                        cancelled = true;
                    }
                    run_id = Some(current.clone());
                    wait_run_done(slot).await.map_err(ServiceError::message)?;
                }
            }
            let snapshot = self.stop_gate.snapshot_stop(key, None, &mut seen);
            if let Some(error) = snapshot.error {
                return Err(ServiceError::message(error));
            }
            if snapshot.idle {
                self.queued_prompts
                    .clear(chelix_sessions::SessionKey::new(key))
                    .await
                    .map_err(|error| ServiceError::message(error.to_string()))?;
                let status = chelix_sessions::QueuedPromptsStatus {
                    session_id: chelix_sessions::SessionKey::new(key),
                    prompts: Vec::new(),
                };
                if let Err(error) =
                    crate::prompt_queue::broadcast_queued_prompts_status(&self.state, &status).await
                {
                    tracing::warn!(%error, "failed to broadcast empty queued prompts status");
                }
                let snapshot = self
                    .stop_gate
                    .snapshot_stop(key, Some(snapshot.epoch), &mut seen);
                if let Some(error) = snapshot.error {
                    return Err(ServiceError::message(error));
                }
                if snapshot.idle {
                    break;
                }
                continue;
            }
            notified.await;
        }
        stop_depth.active = false;
        self.stop_gate.end_stop(key);
        Ok(chelix_service_traits::StopSessionOutcome { cancelled, run_id })
    }
}

impl Drop for SendGuard {
    fn drop(&mut self) {
        if self.active {
            self.gate.abandon(self.id);
        }
    }
}

impl Drop for PermitRelease {
    fn drop(&mut self) {
        drop(self.permit.take());
        let mut state = lock(&self.state);
        if let Some(count) = state.permit_holders.get_mut(&self.session_key) {
            *count = count.saturating_sub(1);
            if *count == 0 {
                state.permit_holders.remove(&self.session_key);
            }
        }
        drop(state);
        self.notify.notify_waiters();
    }
}

async fn wait_run_done(slot: Arc<RunDone>) -> Result<(), String> {
    loop {
        let notified = slot.notify.notified();
        if let Some(result) = lock(&slot.result).clone() {
            return result;
        }
        notified.await;
    }
}

fn begin_stop_locked(state: &mut GateState, session_key: &str) {
    let depth = state.stop_depth.entry(session_key.to_string()).or_insert(0);
    *depth += 1;
    if *depth == 1 {
        state.suppressed.insert(session_key.to_string());
        for guard in state.guards.values() {
            if guard.session_key == session_key {
                guard.cancel.cancel();
            }
        }
    }
}

fn end_stop_locked(state: &mut GateState, session_key: &str) {
    let Some(depth) = state.stop_depth.get_mut(session_key) else {
        return;
    };
    *depth = depth.saturating_sub(1);
    if *depth == 0 {
        state.stop_depth.remove(session_key);
        state.suppressed.remove(session_key);
        state.finished_during_stop.remove(session_key);
    }
}

pub(in crate::service) struct StopSnapshot {
    pub(in crate::service) idle: bool,
    pub(in crate::service) error: Option<String>,
    pub(in crate::service) epoch: u64,
}

struct StopDepthGuard<'a> {
    gate: &'a StopGate,
    key: String,
    active: bool,
}

impl Drop for StopDepthGuard<'_> {
    fn drop(&mut self) {
        if self.active {
            self.gate.end_stop(&self.key);
        }
    }
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|error| error.into_inner())
}

const DROPPED_RUN: &str = "run dropped before completion";

pub(in crate::service) struct RunFinishGuard {
    pub(in crate::service) armed: bool,
    gate: Arc<StopGate>,
    session_gates: Arc<super::session_gate::SessionGateRegistry>,
    active_runs: Arc<tokio::sync::RwLock<HashMap<String, CancellationToken>>>,
    active_runs_by_session: Arc<tokio::sync::RwLock<HashMap<String, String>>>,
    session_key: String,
    run_id: String,
}

impl RunFinishGuard {
    pub(in crate::service) fn arm(
        gate: Arc<StopGate>,
        session_gates: Arc<super::session_gate::SessionGateRegistry>,
        active_runs: Arc<tokio::sync::RwLock<HashMap<String, CancellationToken>>>,
        active_runs_by_session: Arc<tokio::sync::RwLock<HashMap<String, String>>>,
        session_key: String,
        run_id: String,
    ) -> Self {
        Self {
            armed: true,
            gate,
            session_gates,
            active_runs,
            active_runs_by_session,
            session_key,
            run_id,
        }
    }

    pub(in crate::service) fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for RunFinishGuard {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        self.armed = false;
        let gate = Arc::clone(&self.gate);
        let session_gates = Arc::clone(&self.session_gates);
        let active_runs = Arc::clone(&self.active_runs);
        let active_runs_by_session = Arc::clone(&self.active_runs_by_session);
        let session_key = self.session_key.clone();
        let run_id = self.run_id.clone();
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            handle.spawn(async move {
                finalize_dropped_run(
                    gate,
                    session_gates,
                    active_runs,
                    active_runs_by_session,
                    session_key,
                    run_id,
                )
                .await;
            });
            return;
        }
        std::thread::spawn(move || {
            finalize_dropped_run_blocking(
                gate,
                session_gates,
                active_runs,
                active_runs_by_session,
                session_key,
                run_id,
            );
        });
    }
}

async fn finalize_dropped_run(
    gate: Arc<StopGate>,
    session_gates: Arc<super::session_gate::SessionGateRegistry>,
    active_runs: Arc<tokio::sync::RwLock<HashMap<String, CancellationToken>>>,
    active_runs_by_session: Arc<tokio::sync::RwLock<HashMap<String, String>>>,
    session_key: String,
    run_id: String,
) {
    {
        let mut active = active_runs.write().await;
        gate.finish_run(&mut active, &run_id, Err(DROPPED_RUN.to_string()));
    }
    let mut runs = active_runs_by_session.write().await;
    let still_current = runs.get(&session_key) == Some(&run_id);
    if still_current {
        runs.remove(&session_key);
        session_gates
            .finish_turn(
                &session_key,
                chelix_service_traits::SessionTerminal::Cancelled,
            )
            .await;
    }
    drop(runs);
    if !still_current {
        session_gates.notify();
    }
}

fn finalize_dropped_run_blocking(
    gate: Arc<StopGate>,
    session_gates: Arc<super::session_gate::SessionGateRegistry>,
    active_runs: Arc<tokio::sync::RwLock<HashMap<String, CancellationToken>>>,
    active_runs_by_session: Arc<tokio::sync::RwLock<HashMap<String, String>>>,
    session_key: String,
    run_id: String,
) {
    {
        let mut active = active_runs.blocking_write();
        gate.finish_run(&mut active, &run_id, Err(DROPPED_RUN.to_string()));
    }
    let mut runs = active_runs_by_session.blocking_write();
    let still_current = runs.get(&session_key) == Some(&run_id);
    if still_current {
        runs.remove(&session_key);
        session_gates.finish_turn_blocking(
            &session_key,
            chelix_service_traits::SessionTerminal::Cancelled,
        );
    }
    drop(runs);
    if !still_current {
        session_gates.notify();
    }
}
