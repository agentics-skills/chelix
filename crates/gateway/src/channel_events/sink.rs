use super::*;

/// Broadcasts channel events over the gateway WebSocket.
///
/// Uses a deferred `OnceCell` reference so the sink can be created before
/// `GatewayState` exists (same pattern as cron callbacks).
pub struct GatewayChannelEventSink {
    pub(super) state: Arc<tokio::sync::OnceCell<Arc<GatewayState>>>,
}

impl GatewayChannelEventSink {
    pub fn new(state: Arc<tokio::sync::OnceCell<Arc<GatewayState>>>) -> Self {
        Self { state }
    }
}

pub(super) async fn emit(
    state: &Arc<tokio::sync::OnceCell<Arc<GatewayState>>>,
    event: ChannelEvent,
) {
    if let Some(state) = state.get() {
        let payload = match serde_json::to_value(&event) {
            Ok(v) => v,
            Err(e) => {
                warn!("failed to serialize channel event: {e}");
                return;
            },
        };

        broadcast(state, "channel", payload, BroadcastOpts::default()).await;
    }
}
