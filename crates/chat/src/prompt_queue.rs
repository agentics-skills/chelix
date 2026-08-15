//! Session prompt queue used by `chat.send`.
//!
//! A prompt is queued when the user submits it while an agent run already owns
//! the session. The queue is durable (`chelix-sessions`), so it survives page
//! reloads and gateway restarts, and every mutation is broadcast so all
//! connected clients render the same pending prompts.
//!
//! The queue is claimed exactly once, after the owning run reaches its final
//! gate and releases the session permit. The whole queue is then replayed as a
//! single agent run whose leading user messages are the queued prompts in
//! submission order. A replay that cannot be started hands the prompts back,
//! so user input is never dropped silently.

use std::sync::Arc;

use {serde_json::Value, tracing::info};

use chelix_sessions::{QueuedPrompt, SessionPromptQueueStore};

use crate::{
    error::{Error, Result},
    runtime::ChatRuntime,
    types::{BroadcastOpts, broadcast},
};

/// Broadcast state name carrying a full queue snapshot for one session.
const QUEUE_EVENT_STATE: &str = "prompt_queue";

/// Result field reporting how many claimed prompts reached session history.
///
/// `chat.send` succeeds in ways that never persist anything — a `MessageReceived`
/// hook can reject the message, and a busy session defers the replay. Reporting
/// the persisted count makes the claim verifiable: the replay is only complete
/// when the prompts are in history, so anything else returns them to the queue.
pub(crate) const QUEUED_PROMPTS_PERSISTED_KEY: &str = "queuedPromptsPersisted";

/// Whether a `chat.send` result confirms the claimed prompts became history.
#[must_use]
pub(crate) fn replay_persisted_prompts(result: &Value) -> bool {
    result
        .get(QUEUED_PROMPTS_PERSISTED_KEY)
        .and_then(Value::as_u64)
        .is_some_and(|persisted| persisted > 0)
}

/// Durable prompt queue with client synchronization.
pub(crate) struct PromptQueue {
    store: Arc<SessionPromptQueueStore>,
    state: Arc<dyn ChatRuntime>,
}

impl PromptQueue {
    pub(crate) fn new(store: Arc<SessionPromptQueueStore>, state: Arc<dyn ChatRuntime>) -> Self {
        Self { store, state }
    }

    /// Append a prompt and broadcast the resulting snapshot.
    pub(crate) async fn push(
        &self,
        session_key: &str,
        params: &Value,
    ) -> Result<Vec<QueuedPrompt>> {
        self.store.push(session_key, params).await?;
        let prompts = self.store.list(session_key).await?;
        info!(
            session = %session_key,
            queued = prompts.len(),
            "prompt queue: queued because session is active"
        );
        self.broadcast_snapshot(session_key, &prompts).await;
        Ok(prompts)
    }

    /// Current queue snapshot of a session.
    pub(crate) async fn list(&self, session_key: &str) -> Result<Vec<QueuedPrompt>> {
        Ok(self.store.list(session_key).await?)
    }

    /// Remove one prompt and broadcast the resulting snapshot.
    ///
    /// Fails when the prompt is unknown so a stale client cannot silently
    /// believe it cancelled something.
    pub(crate) async fn cancel_one(
        &self,
        session_key: &str,
        prompt_id: &str,
    ) -> Result<Vec<QueuedPrompt>> {
        if !self.store.remove(session_key, prompt_id).await? {
            return Err(Error::message(format!(
                "queued prompt '{prompt_id}' not found in session '{session_key}'"
            )));
        }
        let prompts = self.store.list(session_key).await?;
        info!(
            session = %session_key,
            prompt_id,
            remaining = prompts.len(),
            "prompt queue: cancelled one prompt"
        );
        self.broadcast_snapshot(session_key, &prompts).await;
        Ok(prompts)
    }

    /// Remove every prompt of a session and broadcast the empty snapshot.
    pub(crate) async fn cancel_all(&self, session_key: &str) -> Result<u64> {
        let cleared = self.store.clear(session_key).await?;
        info!(
            session = %session_key,
            cleared,
            "prompt queue: cancelled all prompts"
        );
        self.broadcast_snapshot(session_key, &[]).await;
        Ok(cleared)
    }

    /// Claim the whole queue for replay and broadcast the resulting snapshot.
    ///
    /// The prompts leave the queue in a single statement, which makes the
    /// claim exclusive: from this point a prompt can no longer be cancelled,
    /// because it is already on its way into session history. A replay that
    /// never starts must hand the prompts back with
    /// [`PromptQueue::restore_claimed`].
    pub(crate) async fn claim_all(&self, session_key: &str) -> Result<Vec<QueuedPrompt>> {
        let prompts = self.store.claim_all(session_key).await?;
        if prompts.is_empty() {
            return Ok(prompts);
        }
        info!(
            session = %session_key,
            count = prompts.len(),
            "prompt queue: claimed after final gate"
        );
        self.broadcast_snapshot(session_key, &[]).await;
        Ok(prompts)
    }

    /// Return claimed prompts to the queue after a replay could not start.
    ///
    /// Prompts queued in the meantime keep their place, so the batch stays in
    /// submission order and nothing the user typed is dropped.
    pub(crate) async fn restore_claimed(&self, session_key: &str, claimed: &[QueuedPrompt]) {
        if let Err(error) = self.store.restore(session_key, claimed).await {
            tracing::error!(
                session = %session_key,
                %error,
                count = claimed.len(),
                "prompt queue: failed to restore claimed prompts"
            );
        }
        match self.store.list(session_key).await {
            Ok(prompts) => self.broadcast_snapshot(session_key, &prompts).await,
            Err(error) => tracing::error!(
                session = %session_key,
                %error,
                "prompt queue: failed to resync after a rejected replay"
            ),
        }
    }

    async fn broadcast_snapshot(&self, session_key: &str, prompts: &[QueuedPrompt]) {
        broadcast(
            &self.state,
            "chat",
            queue_snapshot_payload(session_key, prompts),
            BroadcastOpts::default(),
        )
        .await;
    }
}

/// Build the `chat` event payload carrying a queue snapshot.
#[must_use]
pub(crate) fn queue_snapshot_payload(session_key: &str, prompts: &[QueuedPrompt]) -> Value {
    serde_json::json!({
        "state": QUEUE_EVENT_STATE,
        "sessionKey": session_key,
        "prompts": prompts,
    })
}

/// Merge a drained queue into the replay request for a single agent run.
///
/// The last prompt supplies the request parameters, so the run uses the model
/// and reasoning effort the user selected most recently. Every preceding prompt
/// is carried in `_queued_prompts` and enters the run as its own leading user
/// message, so the agent answers the whole batch once instead of once per
/// prompt.
///
/// The replay is pinned to the session that owns the queue: `_session_key` is
/// set and the originating `_conn_id` is dropped. A queued prompt is session
/// state, so it must run in its own session even when the submitting client
/// switched to another session or disconnected in the meantime — resolving the
/// key from the connection would otherwise send the batch to whatever session
/// that client now looks at, or to `main` once it is gone.
#[must_use]
pub(crate) fn build_replay_params(
    session_key: &str,
    mut prompts: Vec<QueuedPrompt>,
) -> Option<Value> {
    let mut params = prompts.pop()?.params;
    params["_queued_replay"] = Value::Bool(true);
    params["_session_key"] = Value::String(session_key.to_string());
    if let Some(object) = params.as_object_mut() {
        object.remove("_conn_id");
    }
    if !prompts.is_empty() {
        params["_queued_prompts"] =
            Value::Array(prompts.into_iter().map(|prompt| prompt.params).collect());
    }
    Some(params)
}

/// Extract the preceding queued prompts attached to a replay request.
#[must_use]
pub(crate) fn take_queued_prompts(params: &mut Value) -> Vec<Value> {
    params
        .as_object_mut()
        .and_then(|object| object.remove("_queued_prompts"))
        .and_then(|value| match value {
            Value::Array(items) => Some(items),
            _ => None,
        })
        .unwrap_or_default()
}

#[allow(clippy::unwrap_used, clippy::expect_used)]
#[cfg(test)]
mod tests {
    use super::*;

    fn prompt(id: &str, params: Value) -> QueuedPrompt {
        QueuedPrompt {
            id: id.to_string(),
            session_key: "s1".to_string(),
            position: 1,
            preview: String::new(),
            params,
            created_at: 0,
        }
    }

    #[test]
    fn replay_params_are_none_for_an_empty_queue() {
        assert!(build_replay_params("s1", Vec::new()).is_none());
    }

    #[test]
    fn replay_params_use_a_single_prompt_verbatim() {
        let params = build_replay_params("s1", vec![prompt(
            "a",
            serde_json::json!({ "text": "one", "model": "gpt-5" }),
        )]);

        let params = params.expect("replay params");
        assert_eq!(params["text"], "one");
        assert_eq!(params["model"], "gpt-5");
        assert_eq!(params["_queued_replay"], true);
        assert!(params.get("_queued_prompts").is_none());
    }

    #[test]
    fn replay_params_use_the_last_prompt_as_the_request() {
        let params = build_replay_params("s1", vec![
            prompt("a", serde_json::json!({ "text": "one", "model": "old" })),
            prompt("b", serde_json::json!({ "text": "two", "model": "new" })),
        ])
        .expect("replay params");

        assert_eq!(params["text"], "two");
        assert_eq!(params["model"], "new");
    }

    /// A queued prompt belongs to its session, not to the client that sent it.
    #[test]
    fn replay_params_pin_the_owning_session() {
        let params = build_replay_params("work", vec![prompt(
            "a",
            serde_json::json!({ "text": "one", "_conn_id": "conn-1" }),
        )])
        .expect("replay params");

        assert_eq!(params["_session_key"], "work");
        assert!(
            params.get("_conn_id").is_none(),
            "the replay must not resolve its session from a connection"
        );
    }

    #[test]
    fn replay_params_carry_preceding_prompts_in_order() {
        let params = build_replay_params("s1", vec![
            prompt("a", serde_json::json!({ "text": "one" })),
            prompt("b", serde_json::json!({ "text": "two" })),
            prompt("c", serde_json::json!({ "text": "three" })),
        ])
        .expect("replay params");

        let preceding = params["_queued_prompts"]
            .as_array()
            .expect("queued prompts array");
        assert_eq!(preceding.len(), 2);
        assert_eq!(preceding[0]["text"], "one");
        assert_eq!(preceding[1]["text"], "two");
    }

    #[test]
    fn a_persisted_replay_consumes_the_claim() {
        let result = serde_json::json!({
            "ok": true,
            "runId": "r1",
            QUEUED_PROMPTS_PERSISTED_KEY: 2,
        });

        assert!(replay_persisted_prompts(&result));
    }

    /// A hook that blocks the message returns `Ok`, but nothing was stored.
    #[test]
    fn a_rejected_replay_does_not_consume_the_claim() {
        let result = serde_json::json!({
            "ok": false,
            "rejected": true,
            "reason": "blocked",
        });

        assert!(!replay_persisted_prompts(&result));
    }

    #[test]
    fn a_deferred_replay_does_not_consume_the_claim() {
        let result = serde_json::json!({
            "ok": false,
            "queued": true,
            QUEUED_PROMPTS_PERSISTED_KEY: 0,
        });

        assert!(!replay_persisted_prompts(&result));
    }

    #[test]
    fn take_queued_prompts_removes_the_field() {
        let mut params = serde_json::json!({
            "text": "one",
            "_queued_prompts": [{ "text": "two" }],
        });

        let followups = take_queued_prompts(&mut params);

        assert_eq!(followups.len(), 1);
        assert!(params.get("_queued_prompts").is_none());
    }

    #[test]
    fn take_queued_prompts_is_empty_without_the_field() {
        let mut params = serde_json::json!({ "text": "one" });

        assert!(take_queued_prompts(&mut params).is_empty());
    }

    #[test]
    fn snapshot_payload_carries_every_prompt() {
        let payload =
            queue_snapshot_payload("s1", &[prompt("a", serde_json::json!({ "text": "one" }))]);

        assert_eq!(payload["state"], QUEUE_EVENT_STATE);
        assert_eq!(payload["sessionKey"], "s1");
        assert_eq!(payload["prompts"].as_array().map(Vec::len), Some(1));
    }

    #[test]
    fn snapshot_payload_reports_an_empty_queue() {
        let payload = queue_snapshot_payload("s1", &[]);

        assert_eq!(payload["prompts"].as_array().map(Vec::len), Some(0));
    }
}
