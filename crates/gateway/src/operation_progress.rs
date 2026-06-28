use std::{future::Future, sync::Arc, time::Duration};

use {moltis_protocol::EventFrame, serde_json::json, tracing::warn};

use crate::state::GatewayState;

/// Emits reusable progress updates for long-running RPC operations.
#[derive(Clone)]
pub(crate) struct OperationProgressEmitter {
    state: Arc<GatewayState>,
    client_conn_id: String,
    operation_id: String,
    method: String,
    kind: String,
    session_key: Option<String>,
}

impl OperationProgressEmitter {
    pub(crate) fn new(
        state: Arc<GatewayState>,
        client_conn_id: impl Into<String>,
        operation_id: impl Into<String>,
        method: impl Into<String>,
        kind: impl Into<String>,
        session_key: Option<String>,
    ) -> Self {
        Self {
            state,
            client_conn_id: client_conn_id.into(),
            operation_id: operation_id.into(),
            method: method.into(),
            kind: kind.into(),
            session_key,
        }
    }

    pub(crate) async fn emit(
        &self,
        phase: &str,
        message: &str,
        current: Option<u64>,
        total: Option<u64>,
        done: bool,
    ) {
        let event = "operation.progress";
        let frame = EventFrame {
            r#type: "event".into(),
            event: event.into(),
            payload: Some(json!({
                "operationId": &self.operation_id,
                "method": &self.method,
                "kind": &self.kind,
                "sessionKey": self.session_key.as_deref(),
                "phase": phase,
                "message": message,
                "current": current,
                "total": total,
                "done": done,
            })),
            seq: Some(self.state.broadcaster.next_seq()),
            state_version: None,
            stream: None,
            done: None,
            channel: None,
        };
        let json = match serde_json::to_string(&frame) {
            Ok(value) => value,
            Err(error) => {
                warn!(%error, "failed to serialize operation progress event");
                return;
            },
        };
        let registry = self.state.client_registry.read().await;
        for (conn_id, client) in &registry.clients {
            if !client.is_subscribed_to(event) {
                continue;
            }
            let is_origin = conn_id == &self.client_conn_id;
            let is_same_session = self.session_key.as_ref().is_some_and(|session_key| {
                registry
                    .active_sessions
                    .get(conn_id)
                    .is_some_and(|active_session| active_session == session_key)
            });
            if is_origin || is_same_session {
                let _ = client.send(&json);
            }
        }
    }

    pub(crate) async fn run_with_heartbeat<T, F>(
        &self,
        phase: &str,
        message: &str,
        current: Option<u64>,
        total: Option<u64>,
        future: F,
    ) -> T
    where
        F: Future<Output = T>,
    {
        self.emit(phase, message, current, total, false).await;
        tokio::pin!(future);
        loop {
            let heartbeat = tokio::time::sleep(Duration::from_secs(10));
            tokio::pin!(heartbeat);
            tokio::select! {
                result = &mut future => return result,
                () = &mut heartbeat => {
                    self.emit(phase, message, current, total, false).await;
                },
            }
        }
    }
}
