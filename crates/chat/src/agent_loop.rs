//! Agent loop support: model flagging, channel streaming, and compaction.

use std::{
    collections::HashSet,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};

use {
    chelix_config::schema::ToolMode,
    serde_json::Value,
    tokio::sync::{Mutex, Notify, RwLock, mpsc},
    tracing::{debug, info, warn},
};

use chelix_agents::runner::RunnerEvent;

use crate::{models::DisabledModelsStore, runtime::ChatRuntime, types::*};

pub(crate) async fn mark_unsupported_model(
    state: &Arc<dyn ChatRuntime>,
    model_store: &Arc<RwLock<DisabledModelsStore>>,
    model_id: &str,
    provider_name: &str,
    error_obj: &Value,
) {
    if error_obj.get("type").and_then(|v| v.as_str()) != Some("unsupported_model") {
        return;
    }

    let detail = error_obj
        .get("detail")
        .and_then(|v| v.as_str())
        .unwrap_or("Model is not supported for this account/provider");
    let provider = error_obj
        .get("provider")
        .and_then(|v| v.as_str())
        .unwrap_or(provider_name);

    let mut store = model_store.write().await;
    if store.mark_unsupported(model_id, detail, Some(provider)) {
        let unsupported = store.unsupported_info(model_id).cloned();
        if let Err(err) = store.save() {
            warn!(
                model = model_id,
                provider = provider,
                error = %err,
                "failed to persist unsupported model flag"
            );
        } else {
            info!(
                model = model_id,
                provider = provider,
                "flagged model as unsupported"
            );
        }
        drop(store);
        broadcast(
            state,
            "models.updated",
            serde_json::json!({
                "modelId": model_id,
                "unsupported": true,
                "unsupportedReason": unsupported.as_ref().map(|u| u.detail.as_str()).unwrap_or(detail),
                "unsupportedProvider": unsupported
                    .as_ref()
                    .and_then(|u| u.provider.as_deref())
                    .unwrap_or(provider),
                "unsupportedUpdatedAt": unsupported.map(|u| u.updated_at_ms).unwrap_or_else(now_ms),
            }),
            BroadcastOpts::default(),
        )
        .await;
    }
}

pub(crate) async fn clear_unsupported_model(
    state: &Arc<dyn ChatRuntime>,
    model_store: &Arc<RwLock<DisabledModelsStore>>,
    model_id: &str,
) {
    let mut store = model_store.write().await;
    if store.clear_unsupported(model_id) {
        if let Err(err) = store.save() {
            warn!(
                model = model_id,
                error = %err,
                "failed to persist unsupported model clear"
            );
        } else {
            info!(model = model_id, "cleared unsupported model flag");
        }
        drop(store);
        broadcast(
            state,
            "models.updated",
            serde_json::json!({
                "modelId": model_id,
                "unsupported": false,
            }),
            BroadcastOpts::default(),
        )
        .await;
    }
}

#[derive(Clone, Default)]
pub(crate) struct RunnerEventBarrier {
    sent: Arc<AtomicU64>,
    processed: Arc<AtomicU64>,
    notify: Arc<Notify>,
}

impl RunnerEventBarrier {
    #[must_use]
    pub(crate) fn snapshot(&self) -> u64 {
        self.sent.load(Ordering::Acquire)
    }

    pub(crate) fn mark_processed(&self) {
        self.processed.fetch_add(1, Ordering::Release);
        self.notify.notify_waiters();
    }

    pub(crate) fn processed_guard(&self) -> RunnerEventProcessedGuard {
        RunnerEventProcessedGuard(self.clone())
    }

    pub(crate) async fn wait_for(&self, target: u64) {
        loop {
            let notified = self.notify.notified();
            if self.processed.load(Ordering::Acquire) >= target {
                return;
            }
            notified.await;
        }
    }
}

pub(crate) struct RunnerEventProcessedGuard(RunnerEventBarrier);

impl Drop for RunnerEventProcessedGuard {
    fn drop(&mut self) {
        self.0.mark_processed();
    }
}

pub(crate) fn ordered_runner_event_callback() -> (
    Box<dyn Fn(RunnerEvent) + Send + Sync>,
    mpsc::UnboundedReceiver<RunnerEvent>,
    RunnerEventBarrier,
) {
    let (tx, rx) = mpsc::unbounded_channel::<RunnerEvent>();
    let barrier = RunnerEventBarrier::default();
    let callback_barrier = barrier.clone();
    let callback: Box<dyn Fn(RunnerEvent) + Send + Sync> = Box::new(move |event| {
        if tx.send(event).is_ok() {
            callback_barrier.sent.fetch_add(1, Ordering::Release);
        } else {
            debug!("runner event dropped because event processor is closed");
        }
    });
    (callback, rx, barrier)
}

const CHANNEL_STREAM_BUFFER_SIZE: usize = 64;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct ChannelReplyTargetKey {
    channel_type: chelix_channels::ChannelType,
    account_id: String,
    chat_id: String,
    message_id: Option<String>,
    thread_id: Option<String>,
}

impl From<&chelix_channels::ChannelReplyTarget> for ChannelReplyTargetKey {
    fn from(target: &chelix_channels::ChannelReplyTarget) -> Self {
        Self {
            channel_type: target.channel_type,
            account_id: target.account_id.clone(),
            chat_id: target.chat_id.clone(),
            message_id: target.message_id.clone(),
            thread_id: target.thread_id.clone(),
        }
    }
}

struct ChannelStreamWorker {
    sender: chelix_channels::StreamSender,
    receives_progress_deltas: bool,
}

/// Fan out model deltas to channel stream workers (Telegram/Discord edit-in-place).
///
/// Workers are started eagerly so channel typing indicators remain active
/// during long-running tool execution before the first text delta arrives.
/// Stream-dedup only applies after at least one delta has been sent.
pub(crate) struct ChannelStreamDispatcher {
    outbound: Arc<dyn chelix_channels::plugin::ChannelStreamOutbound>,
    targets: Vec<chelix_channels::ChannelReplyTarget>,
    workers: Vec<ChannelStreamWorker>,
    tasks: Vec<tokio::task::JoinHandle<()>>,
    completed: Arc<Mutex<HashSet<ChannelReplyTargetKey>>>,
    started: bool,
    sent_final_delta: bool,
}

impl ChannelStreamDispatcher {
    pub(crate) async fn for_session(
        state: &Arc<dyn ChatRuntime>,
        session_key: &str,
    ) -> Option<Self> {
        let outbound = state.channel_stream_outbound()?;
        let targets: Vec<chelix_channels::ChannelReplyTarget> = state
            .peek_channel_replies(session_key)
            .await
            .into_iter()
            .collect();
        if targets.is_empty() {
            return None;
        }
        let mut dispatcher = Self {
            outbound,
            targets,
            workers: Vec::new(),
            tasks: Vec::new(),
            completed: Arc::new(Mutex::new(HashSet::new())),
            started: false,
            sent_final_delta: false,
        };
        dispatcher.ensure_started().await;
        Some(dispatcher)
    }

    async fn ensure_started(&mut self) {
        if self.started {
            return;
        }
        self.started = true;

        for target in self.targets.iter().cloned() {
            if !self.outbound.is_stream_enabled(&target.account_id).await {
                debug!(
                    account_id = target.account_id.as_str(),
                    chat_id = target.chat_id.as_str(),
                    "channel streaming disabled for target account"
                );
                continue;
            }

            let key = ChannelReplyTargetKey::from(&target);
            let streams_final_replies = self
                .outbound
                .streams_final_replies(&target.account_id)
                .await;
            let receives_progress_deltas = self
                .outbound
                .receives_progress_deltas(&target.account_id)
                .await;
            let (tx, rx) = mpsc::channel(CHANNEL_STREAM_BUFFER_SIZE);
            let outbound = Arc::clone(&self.outbound);
            let completed = Arc::clone(&self.completed);
            let account_id = target.account_id.clone();
            let to = target.outbound_to().into_owned();
            let reply_to = target.message_id.clone();
            let key_for_insert = key.clone();
            let account_for_log = account_id.clone();
            let chat_for_log = target.chat_id.clone();
            let thread_for_log = target.thread_id.clone();

            self.workers.push(ChannelStreamWorker {
                sender: tx,
                receives_progress_deltas,
            });
            self.tasks.push(tokio::spawn(async move {
                match outbound
                    .send_stream(&account_id, &to, reply_to.as_deref(), rx)
                    .await
                {
                    Ok(()) => {
                        if streams_final_replies {
                            completed.lock().await.insert(key_for_insert);
                        }
                    },
                    Err(e) => {
                        warn!(
                            account_id = account_for_log,
                            chat_id = chat_for_log,
                            thread_id = thread_for_log.as_deref().unwrap_or("-"),
                            "channel stream outbound failed: {e}"
                        );
                    },
                }
            }));
        }
    }

    pub(crate) async fn send_delta(&mut self, delta: &str) {
        if delta.is_empty() {
            return;
        }
        self.sent_final_delta = true;
        self.ensure_started().await;
        self.send_to_workers(
            chelix_channels::StreamEvent::Delta(delta.to_string()),
            "delta",
        )
        .await;
    }

    pub(crate) async fn send_progress_delta(&mut self, delta: &str) {
        if delta.is_empty() {
            return;
        }
        self.ensure_started().await;
        let event = chelix_channels::StreamEvent::ProgressDelta(delta.to_string());
        for worker in &self.workers {
            if worker.receives_progress_deltas && worker.sender.send(event.clone()).await.is_err() {
                debug!("channel stream progress delta dropped: worker closed");
            }
        }
    }

    async fn send_to_workers(&mut self, event: chelix_channels::StreamEvent, label: &str) {
        for worker in &self.workers {
            if worker.sender.send(event.clone()).await.is_err() {
                debug!("channel stream {label} dropped: worker closed");
            }
        }
    }

    pub(crate) async fn finish(&mut self) {
        self.send_terminal(chelix_channels::StreamEvent::Done).await;
        self.join_workers().await;
    }

    async fn send_terminal(&mut self, event: chelix_channels::StreamEvent) {
        if self.workers.is_empty() {
            return;
        }
        let workers = std::mem::take(&mut self.workers);
        for worker in &workers {
            if worker.sender.send(event.clone()).await.is_err() {
                debug!("channel stream terminal event dropped: worker closed");
            }
        }
    }

    async fn join_workers(&mut self) {
        let tasks = std::mem::take(&mut self.tasks);
        for task in tasks {
            if let Err(e) = task.await {
                warn!(error = %e, "channel stream worker task join failed");
            }
        }
    }

    pub(crate) async fn completed_target_keys(&self) -> HashSet<ChannelReplyTargetKey> {
        if !self.sent_final_delta {
            return HashSet::new();
        }
        self.completed.lock().await.clone()
    }
}

/// Resolve the effective tool mode for a provider.
///
/// Combines the provider's `tool_mode()` override with its `supports_tools()`
/// capability to determine how tools should be dispatched:
/// - `Native` — provider handles tool schemas via API (OpenAI function calling, etc.)
/// - `Text` — tools are described in the prompt; the runner parses tool calls from text
/// - `Off` — no tools at all
pub(crate) fn effective_tool_mode(provider: &dyn chelix_agents::model::LlmProvider) -> ToolMode {
    match provider.tool_mode() {
        Some(ToolMode::Native) => ToolMode::Native,
        Some(ToolMode::Text) => ToolMode::Text,
        Some(ToolMode::Off) => ToolMode::Off,
        Some(ToolMode::Auto) | None => {
            if provider.supports_tools() {
                ToolMode::Native
            } else {
                ToolMode::Text
            }
        },
    }
}
