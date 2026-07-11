use std::{collections::HashMap, sync::Arc, time::Duration};

use {
    chelix_protocol::{EventFrame, StateVersion, scopes},
    futures::future::join_all,
    tracing::{trace, warn},
};

use crate::state::{ConnectedClient, GatewayState};

const MANDATORY_EVENT_DELIVERY_TIMEOUT: Duration = Duration::from_secs(5);

// ── Broadcaster ──────────────────────────────────────────────────────────────

/// Lock-free broadcast state: sequence counter and GraphQL subscription channel.
///
/// Phase 1 of broadcaster decoupling — owns only fields that never participate
/// in the `GatewayInner` RwLock. Client registry remains in `GatewayInner`
/// to preserve atomic compound operations.
pub struct Broadcaster {
    /// Monotonically increasing sequence counter for broadcast events.
    seq: std::sync::atomic::AtomicU64,
    /// Broadcast channel for GraphQL subscriptions. Events are `(event_name, payload)`.
    #[cfg(feature = "graphql")]
    pub graphql_broadcast: tokio::sync::broadcast::Sender<(String, serde_json::Value)>,
}

impl Default for Broadcaster {
    fn default() -> Self {
        Self::new()
    }
}

impl Broadcaster {
    /// Create a new Broadcaster with sequence starting at 0.
    pub fn new() -> Self {
        Self {
            seq: std::sync::atomic::AtomicU64::new(0),
            #[cfg(feature = "graphql")]
            graphql_broadcast: {
                let (tx, _) = tokio::sync::broadcast::channel(256);
                tx
            },
        }
    }

    /// Return the next sequence number.
    #[must_use]
    pub fn next_seq(&self) -> u64 {
        self.seq.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1
    }
}

// ── Scope guards ─────────────────────────────────────────────────────────────

/// Events that require specific scopes to receive.
fn event_scope_guards() -> HashMap<&'static str, &'static [&'static str]> {
    let mut m = HashMap::new();
    m.insert("command.approval.requested", [scopes::APPROVALS].as_slice());
    m.insert("command.approval.resolved", [scopes::APPROVALS].as_slice());
    m.insert("device.pair.requested", [scopes::PAIRING].as_slice());
    m.insert("device.pair.resolved", [scopes::PAIRING].as_slice());
    m.insert("node.pair.requested", [scopes::PAIRING].as_slice());
    m.insert("node.pair.resolved", [scopes::PAIRING].as_slice());
    m
}

// ── Broadcast options ────────────────────────────────────────────────────────

#[derive(Default)]
pub struct BroadcastOpts {
    pub drop_if_slow: bool,
    pub state_version: Option<StateVersion>,
    /// Stream group ID for chunked delivery (v4).
    pub stream: Option<String>,
    /// End-of-stream marker (v4).
    pub done: bool,
    /// Logical channel for multiplexing (v4).
    pub channel: Option<String>,
}

// ── Broadcast ───────────────────────────────────────────────────────────────

/// Broadcast events to all connected WebSocket clients, respecting scope
/// guards. Droppable events use best-effort delivery; mandatory events wait
/// for bounded queue capacity and disconnect an unresponsive client instead
/// of silently losing protocol state.
pub async fn broadcast(
    state: &Arc<GatewayState>,
    event: &str,
    payload: serde_json::Value,
    opts: BroadcastOpts,
) {
    let seq = state.broadcaster.next_seq();
    let stream = opts.stream.clone();
    let done = opts.done.then_some(true);
    let channel = opts.channel.clone();
    let frame = EventFrame {
        r#type: "event".into(),
        event: event.into(),
        payload: Some(payload),
        seq: Some(seq),
        state_version: opts.state_version,
        stream,
        done,
        channel,
    };
    let json = match serde_json::to_string(&frame) {
        Ok(j) => j,
        Err(e) => {
            warn!("failed to serialize broadcast event: {e}");
            return;
        },
    };

    // Forward to GraphQL subscription broadcast channel.
    #[cfg(feature = "graphql")]
    if let Some(ref payload) = frame.payload {
        let _ = state
            .broadcaster
            .graphql_broadcast
            .send((event.to_string(), payload.clone()));
    }

    let guards = event_scope_guards();
    let required_scopes = guards.get(event);

    let recipients = {
        let registry = state.client_registry.read().await;
        trace!(
            event,
            seq,
            clients = registry.clients.len(),
            "broadcasting event"
        );
        let mut recipients = Vec::new();
        for client in registry.clients.values() {
            // Check scope guard: if the event requires a scope, verify the client has it.
            if let Some(required) = required_scopes {
                let client_scopes = client.scopes();
                let has = client_scopes.contains(&scopes::ADMIN)
                    || required.iter().any(|s| client_scopes.contains(s));
                if !has {
                    continue;
                }
            }

            // Subscription filter (v4): skip clients not subscribed to this event.
            if !client.is_subscribed_to(event) {
                continue;
            }

            // Channel filter (v4): if event is scoped to a channel, skip clients
            // that haven't joined it.
            if let Some(ref ch) = opts.channel
                && !client.is_in_channel(ch)
            {
                continue;
            }

            recipients.push((client.conn_id.clone(), client.sender.clone()));
        }
        recipients
    };

    if opts.drop_if_slow {
        for (_, sender) in recipients {
            let _ = ConnectedClient::try_send_frame(&sender, json.clone());
        }
        return;
    }

    let send_results = join_all(recipients.into_iter().map(|(conn_id, sender)| {
        let frame = json.clone();
        async move {
            let delivered = matches!(
                tokio::time::timeout(MANDATORY_EVENT_DELIVERY_TIMEOUT, sender.send(frame)).await,
                Ok(Ok(()))
            );
            (conn_id, delivered)
        }
    }))
    .await;
    for (conn_id, delivered) in send_results {
        if delivered {
            continue;
        }
        warn!(
            event,
            seq, conn_id, "closing slow client after mandatory event delivery timeout"
        );
        state.close_client(&conn_id).await;
    }
}

/// Broadcast a tick event with the current timestamp and memory stats.
fn tick_mem_payload(
    process_memory_bytes: u64,
    system_available_bytes: u64,
    system_total_bytes: u64,
) -> serde_json::Value {
    let mut mem = serde_json::Map::new();
    mem.insert(
        "process".to_string(),
        serde_json::json!(process_memory_bytes),
    );
    mem.insert(
        "available".to_string(),
        serde_json::json!(system_available_bytes),
    );
    mem.insert("total".to_string(), serde_json::json!(system_total_bytes));
    serde_json::Value::Object(mem)
}

pub async fn broadcast_tick(
    state: &Arc<GatewayState>,
    process_memory_bytes: u64,
    system_available_bytes: u64,
    system_total_bytes: u64,
) {
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;
    let mem = tick_mem_payload(
        process_memory_bytes,
        system_available_bytes,
        system_total_bytes,
    );

    broadcast(
        state,
        "tick",
        serde_json::json!({
            "ts": ts,
            "mem": mem
        }),
        BroadcastOpts {
            drop_if_slow: true,
            ..Default::default()
        },
    )
    .await;
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::tick_mem_payload;

    #[test]
    fn tick_mem_payload_includes_memory_fields() {
        let payload = tick_mem_payload(1, 2, 3);
        assert_eq!(payload.get("process").and_then(|v| v.as_u64()), Some(1));
        assert_eq!(payload.get("available").and_then(|v| v.as_u64()), Some(2));
        assert_eq!(payload.get("total").and_then(|v| v.as_u64()), Some(3));
    }
}

#[cfg(test)]
mod broadcaster_tests {
    use super::Broadcaster;

    #[test]
    fn new_starts_at_zero() {
        let b = Broadcaster::new();
        assert_eq!(b.next_seq(), 1);
    }

    #[test]
    fn next_seq_is_strictly_monotonic() {
        let b = Broadcaster::new();
        let mut prev = 0;
        for _ in 0..100 {
            let seq = b.next_seq();
            assert!(seq > prev, "seq {seq} is not > prev {prev}");
            prev = seq;
        }
        assert_eq!(prev, 100);
    }
}
