//! Per-session wait signal for the current agent-loop final gate.
//!
//! The registry stores only a wait wakeup and terminal kind. Answer text stays
//! in session history.

use std::{collections::HashMap, future::Future, sync::Arc};

use {
    chelix_service_traits::SessionTerminal,
    tokio::sync::{RwLock, watch},
};

use crate::types::ChatRunOutcome;

pub(crate) struct SessionGateRegistry {
    version: watch::Sender<u64>,
    last_terminal: RwLock<HashMap<String, SessionTerminal>>,
}

impl SessionGateRegistry {
    pub(crate) fn new() -> Arc<Self> {
        Arc::new(Self {
            version: watch::Sender::new(0),
            last_terminal: RwLock::new(HashMap::new()),
        })
    }

    pub(crate) async fn forget(&self, session_key: &str) {
        self.last_terminal.write().await.remove(session_key);
    }

    pub(crate) async fn begin_turn(&self, session_key: &str) {
        self.forget(session_key).await;
    }

    pub(crate) async fn finish_turn(&self, session_key: &str, terminal: SessionTerminal) {
        self.last_terminal
            .write()
            .await
            .insert(session_key.to_string(), terminal);
        self.version.send_modify(|version| *version += 1);
    }

    pub(crate) async fn last_terminal(&self, session_key: &str) -> Option<SessionTerminal> {
        self.last_terminal.read().await.get(session_key).copied()
    }

    pub(crate) async fn wait_for_gate<F, Fut>(
        &self,
        session_key: &str,
        mut is_active: F,
    ) -> Option<SessionTerminal>
    where
        F: FnMut() -> Fut,
        Fut: Future<Output = bool>,
    {
        let mut version = self.version.subscribe();
        loop {
            let terminal = self.last_terminal(session_key).await;
            if terminal.is_some() || !is_active().await {
                return terminal;
            }
            if version.changed().await.is_err() {
                return self.last_terminal(session_key).await;
            }
        }
    }
}

pub(crate) fn terminal_from_outcome(outcome: &ChatRunOutcome) -> SessionTerminal {
    match outcome {
        ChatRunOutcome::Completed(_) => SessionTerminal::Completed,
        ChatRunOutcome::Cancelled => SessionTerminal::Cancelled,
        ChatRunOutcome::Failed => SessionTerminal::Failed,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn wait_returns_immediately_when_session_is_inactive() {
        let registry = SessionGateRegistry::new();
        let terminal = registry
            .wait_for_gate("session:child", || async { false })
            .await;
        assert_eq!(terminal, None);
    }

    #[tokio::test]
    async fn wait_wakes_on_finish_without_exposing_a_run_id() {
        let registry = Arc::clone(&SessionGateRegistry::new());
        let waiter_registry = Arc::clone(&registry);
        let wait = tokio::spawn(async move {
            waiter_registry
                .wait_for_gate("session:child", || async { true })
                .await
        });
        tokio::task::yield_now().await;
        registry
            .finish_turn("session:child", SessionTerminal::Cancelled)
            .await;
        assert_eq!(
            wait.await
                .unwrap_or_else(|error| panic!("wait task: {error}")),
            Some(SessionTerminal::Cancelled)
        );
        assert_eq!(
            registry.last_terminal("session:child").await,
            Some(SessionTerminal::Cancelled)
        );
    }

    #[tokio::test]
    async fn forget_clears_last_terminal() {
        let registry = SessionGateRegistry::new();
        registry
            .finish_turn("session:child", SessionTerminal::Completed)
            .await;
        registry.forget("session:child").await;
        assert_eq!(registry.last_terminal("session:child").await, None);
    }

    #[tokio::test]
    async fn begin_turn_clears_stale_terminal() {
        let registry = SessionGateRegistry::new();
        registry
            .finish_turn("session:child", SessionTerminal::Cancelled)
            .await;
        registry.begin_turn("session:child").await;
        assert_eq!(registry.last_terminal("session:child").await, None);
        let terminal = registry
            .wait_for_gate("session:child", || async { false })
            .await;
        assert_eq!(terminal, None);
    }

    #[tokio::test]
    async fn wait_stays_pending_after_begin_turn_clears_terminal_while_active() {
        let registry = Arc::clone(&SessionGateRegistry::new());
        registry
            .finish_turn("session:child", SessionTerminal::Cancelled)
            .await;
        registry.begin_turn("session:child").await;
        let waiter_registry = Arc::clone(&registry);
        let mut wait = tokio::spawn(async move {
            waiter_registry
                .wait_for_gate("session:child", || async { true })
                .await
        });
        tokio::select! {
            biased;
            result = &mut wait => {
                panic!("wait returned before the next finish_turn: {result:?}");
            },
            () = tokio::task::yield_now() => {}
        }
        registry
            .finish_turn("session:child", SessionTerminal::Completed)
            .await;
        assert_eq!(
            wait.await
                .unwrap_or_else(|error| panic!("wait task: {error}")),
            Some(SessionTerminal::Completed)
        );
    }
}
