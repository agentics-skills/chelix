use std::{sync::Arc, time::Duration};

use {
    chelix_config::ApprovalMode,
    serde::{Deserialize, Serialize},
    tokio::sync::{RwLock, oneshot},
    tracing::{debug, warn},
};

use crate::Result;

/// Broadcaster that notifies connected clients about pending approval requests.
#[async_trait::async_trait]
pub trait ApprovalBroadcaster: Send + Sync {
    async fn broadcast_request(
        &self,
        request_id: &str,
        command: &str,
        session_key: Option<&str>,
    ) -> Result<()>;
}

/// Outcome of an approval request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ApprovalDecision {
    Approved,
    Denied,
    Timeout,
}

/// Pending approval request waiting for gateway resolution.
struct PendingApproval {
    command: String,
    session_key: Option<String>,
    tx: oneshot::Sender<ApprovalDecision>,
}

/// Serializable summary of a pending approval request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingApprovalView {
    pub id: String,
    pub command: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_key: Option<String>,
}

/// The approval manager handles the operator approval flow for shell commands.
pub struct ApprovalManager {
    pub mode: ApprovalMode,
    pub timeout: Duration,
    pending: Arc<RwLock<std::collections::HashMap<String, PendingApproval>>>,
}

impl ApprovalManager {
    #[must_use]
    pub fn new(mode: ApprovalMode) -> Self {
        Self {
            mode,
            timeout: Duration::from_secs(120),
            pending: Arc::new(RwLock::new(std::collections::HashMap::new())),
        }
    }

    /// Whether a command must be approved by an operator before it runs.
    #[must_use]
    pub fn needs_approval(&self) -> bool {
        match self.mode {
            ApprovalMode::Always => true,
            ApprovalMode::Never => false,
        }
    }

    /// Register a pending approval request. Returns an ID and a receiver for the decision.
    pub async fn create_request(
        &self,
        command: &str,
        session_key: Option<&str>,
    ) -> (String, oneshot::Receiver<ApprovalDecision>) {
        let id = uuid::Uuid::new_v4().to_string();
        let (tx, rx) = oneshot::channel();
        self.pending
            .write()
            .await
            .insert(id.clone(), PendingApproval {
                command: command.to_string(),
                session_key: session_key.map(str::to_string),
                tx,
            });
        debug!(id = %id, command, session_key, "approval request created");
        (id, rx)
    }

    /// Resolve a pending approval request.
    pub async fn resolve(&self, id: &str, decision: ApprovalDecision) {
        if let Some(pending) = self.pending.write().await.remove(id) {
            let _ = pending.tx.send(decision);
            debug!(id, "approval resolved");
        } else {
            warn!(id, "approval resolve: no pending request");
        }
    }

    /// Return the IDs of all pending approval requests.
    pub async fn pending_ids(&self) -> Vec<String> {
        let mut ids: Vec<_> = self.pending.read().await.keys().cloned().collect();
        ids.sort();
        ids
    }

    /// Return summaries of all pending approval requests.
    pub async fn pending_requests(&self) -> Vec<PendingApprovalView> {
        let mut requests: Vec<_> = self
            .pending
            .read()
            .await
            .iter()
            .map(|(id, pending)| PendingApprovalView {
                id: id.clone(),
                command: pending.command.clone(),
                session_key: pending.session_key.clone(),
            })
            .collect();
        requests.sort_by(|left, right| left.id.cmp(&right.id));
        requests
    }

    /// Return summaries of pending approval requests scoped to a session.
    pub async fn pending_requests_for_session(
        &self,
        session_key: &str,
    ) -> Vec<PendingApprovalView> {
        self.pending_requests()
            .await
            .into_iter()
            .filter(|request| request.session_key.as_deref() == Some(session_key))
            .collect()
    }

    /// Wait for an approval decision with timeout.
    pub async fn wait_for_decision(
        &self,
        rx: oneshot::Receiver<ApprovalDecision>,
    ) -> ApprovalDecision {
        match tokio::time::timeout(self.timeout, rx).await {
            Ok(Ok(decision)) => decision,
            Ok(Err(_)) => {
                warn!("approval channel closed");
                ApprovalDecision::Denied
            },
            Err(_) => {
                warn!("approval timed out");
                ApprovalDecision::Timeout
            },
        }
    }
}

#[allow(clippy::unwrap_used, clippy::expect_used)]
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn never_mode_does_not_require_approval() {
        let manager = ApprovalManager::new(ApprovalMode::Never);
        assert!(!manager.needs_approval());
    }

    #[test]
    fn always_mode_requires_approval() {
        let manager = ApprovalManager::new(ApprovalMode::Always);
        assert!(manager.needs_approval());
    }

    #[tokio::test]
    async fn resolve_delivers_decision_to_waiter() {
        let manager = ApprovalManager::new(ApprovalMode::Always);
        let (id, rx) = manager.create_request("echo hi", Some("session:a")).await;

        manager.resolve(&id, ApprovalDecision::Approved).await;

        assert_eq!(
            manager.wait_for_decision(rx).await,
            ApprovalDecision::Approved
        );
        assert!(manager.pending_ids().await.is_empty());
    }

    #[tokio::test]
    async fn resolve_unknown_request_is_a_noop() {
        let manager = ApprovalManager::new(ApprovalMode::Always);
        manager.resolve("missing", ApprovalDecision::Approved).await;
        assert!(manager.pending_requests().await.is_empty());
    }

    #[tokio::test]
    async fn wait_for_decision_times_out() {
        let mut manager = ApprovalManager::new(ApprovalMode::Always);
        manager.timeout = Duration::from_millis(10);
        let (_id, rx) = manager.create_request("echo hi", None).await;

        assert_eq!(
            manager.wait_for_decision(rx).await,
            ApprovalDecision::Timeout
        );
    }

    #[tokio::test]
    async fn wait_for_decision_denies_when_channel_closes() {
        let manager = ApprovalManager::new(ApprovalMode::Always);
        let (tx, rx) = oneshot::channel();
        drop(tx);

        assert_eq!(
            manager.wait_for_decision(rx).await,
            ApprovalDecision::Denied
        );
    }

    #[tokio::test]
    async fn pending_ids_are_sorted() {
        let manager = ApprovalManager::new(ApprovalMode::Always);
        let (first, _rx1) = manager.create_request("echo one", None).await;
        let (second, _rx2) = manager.create_request("echo two", None).await;

        let mut expected = vec![first, second];
        expected.sort();
        assert_eq!(manager.pending_ids().await, expected);
    }

    #[tokio::test]
    async fn pending_requests_for_session_filters_other_sessions() {
        let manager = ApprovalManager::new(ApprovalMode::Always);
        let _ = manager.create_request("echo one", Some("session:a")).await;
        let _ = manager.create_request("echo two", Some("session:b")).await;
        let _ = manager
            .create_request("echo three", Some("session:a"))
            .await;

        let pending = manager.pending_requests_for_session("session:a").await;
        assert_eq!(pending.len(), 2);
        assert!(
            pending
                .iter()
                .all(|request| request.session_key.as_deref() == Some("session:a"))
        );
    }
}
