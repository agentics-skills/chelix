//! Agent loop support: channel streaming and compaction.

use std::{
    collections::HashSet,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};

use {
    tokio::sync::{Mutex, Notify, mpsc, oneshot},
    tracing::{debug, warn},
};

use chelix_agents::runner::{OnEvent, OnToolLifecycle, RunnerEvent, RunnerToolLifecycleEvent};

use crate::runtime::ChatRuntime;

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

pub(crate) enum OrderedRunnerEvent {
    Event(RunnerEvent),
    ToolLifecycle {
        event: Box<RunnerToolLifecycleEvent>,
        receipt: Option<oneshot::Sender<Result<(), String>>>,
    },
}

pub(crate) fn ordered_runner_event_callbacks(
    ui_run: Option<chelix_sessions::ui_history_engine::UiHistoryRun>,
    cancellation: tokio_util::sync::CancellationToken,
    sandbox_enabled: bool,
) -> (
    OnEvent,
    OnToolLifecycle,
    mpsc::UnboundedReceiver<OrderedRunnerEvent>,
    RunnerEventBarrier,
) {
    let (tx, rx) = mpsc::unbounded_channel::<OrderedRunnerEvent>();
    let barrier = RunnerEventBarrier::default();

    let event_tx = tx.clone();
    let event_barrier = barrier.clone();
    let event_ui = ui_run.clone();
    let event_cancellation = cancellation.clone();
    let on_event: OnEvent = Box::new(move |event| {
        if let Some(run) = &event_ui
            && let Err(error) = crate::ui_history_ingress::copy_event(run, &event)
        {
            tracing::error!(%error, "UI provider copy failed");
            event_cancellation.cancel();
        }
        if event_tx.send(OrderedRunnerEvent::Event(event)).is_ok() {
            event_barrier.sent.fetch_add(1, Ordering::Release);
        } else {
            debug!("runner event dropped because event processor is closed");
        }
    });

    let lifecycle_barrier = barrier.clone();
    let on_tool_lifecycle: OnToolLifecycle = Arc::new(move |event| {
        if let Some(run) = &ui_run
            && let Err(error) =
                crate::ui_history_ingress::copy_lifecycle(run, &event, sandbox_enabled)
        {
            tracing::error!(%error, "UI lifecycle copy failed");
            cancellation.cancel();
            return Box::pin(async move { Err(anyhow::Error::new(error)) });
        }
        let await_receipt = event.lifecycle.stage()
            != chelix_common::tool_lifecycle::ToolLifecycleStage::InputStreaming;
        let (receipt, receipt_rx) = if await_receipt {
            let (receipt_tx, receipt_rx) = oneshot::channel();
            (Some(receipt_tx), Some(receipt_rx))
        } else {
            (None, None)
        };
        let queued = tx.send(OrderedRunnerEvent::ToolLifecycle {
            event: Box::new(event),
            receipt,
        });
        if queued.is_ok() {
            lifecycle_barrier.sent.fetch_add(1, Ordering::Release);
        }
        Box::pin(async move {
            queued.map_err(|_| anyhow::anyhow!("tool lifecycle processor is closed"))?;
            let Some(receipt_rx) = receipt_rx else {
                return Ok(());
            };
            let result = receipt_rx
                .await
                .map_err(|_| anyhow::anyhow!("tool lifecycle receipt was dropped"))?;
            result.map_err(anyhow::Error::msg)
        })
    });

    (on_event, on_tool_lifecycle, rx, barrier)
}

#[cfg(test)]
mod lifecycle_receipt_tests {
    use std::{future::Future, task::Poll};

    use super::*;

    #[tokio::test]
    async fn lifecycle_callback_waits_for_forwarder_receipt() {
        let (_on_event, on_tool_lifecycle, mut receiver, _barrier) =
            ordered_runner_event_callbacks(None, tokio_util::sync::CancellationToken::new(), false);
        let event =
            RunnerToolLifecycleEvent::new(chelix_common::tool_lifecycle::ToolLifecycleEvent {
                tool_call_id: "call-1".to_owned(),
                tool_name: "read_file".to_owned(),
                sequence: 3,
                emitted_at_ms: 1,
                run_id: None,
                context_budget: None,
                update: chelix_common::tool_lifecycle::ToolLifecycleUpdate::WaitingForExecution {
                    arguments: serde_json::json!({"path": "/tmp/input"}),
                },
            });
        let delivery = on_tool_lifecycle(event);
        tokio::pin!(delivery);
        let queued = receiver
            .recv()
            .await
            .unwrap_or_else(|| panic!("lifecycle event must be queued"));
        let OrderedRunnerEvent::ToolLifecycle { receipt, .. } = queued else {
            panic!("expected a lifecycle event");
        };
        let receipt = receipt.unwrap_or_else(|| panic!("stage boundary must carry a receipt"));

        let was_pending =
            std::future::poll_fn(|cx| Poll::Ready(delivery.as_mut().poll(cx).is_pending())).await;
        assert!(was_pending);
        receipt
            .send(Ok(()))
            .unwrap_or_else(|_| panic!("lifecycle callback must still await the receipt"));
        assert!(delivery.await.is_ok());
    }

    #[tokio::test]
    async fn input_streaming_callback_completes_after_ordered_enqueue() {
        let (_on_event, on_tool_lifecycle, mut receiver, _barrier) =
            ordered_runner_event_callbacks(None, tokio_util::sync::CancellationToken::new(), false);
        let event =
            RunnerToolLifecycleEvent::new(chelix_common::tool_lifecycle::ToolLifecycleEvent {
                tool_call_id: "call-1".to_owned(),
                tool_name: "overwrite_file".to_owned(),
                sequence: 1,
                emitted_at_ms: 1,
                run_id: None,
                context_budget: None,
                update: chelix_common::tool_lifecycle::ToolLifecycleUpdate::InputStreaming {
                    arguments_delta: "fragment".to_owned(),
                },
            });

        assert!(on_tool_lifecycle(event).await.is_ok());
        let queued = receiver
            .recv()
            .await
            .unwrap_or_else(|| panic!("input streaming event must be queued"));
        let OrderedRunnerEvent::ToolLifecycle { receipt, .. } = queued else {
            panic!("expected a lifecycle event");
        };
        assert!(receipt.is_none());
    }

    #[tokio::test]
    async fn event_receiver_closes_after_both_callbacks_are_dropped() {
        let (on_event, on_tool_lifecycle, mut receiver, _barrier) =
            ordered_runner_event_callbacks(None, tokio_util::sync::CancellationToken::new(), false);

        drop(on_event);
        drop(on_tool_lifecycle);

        assert!(receiver.recv().await.is_none());
    }
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

/// Fan out model deltas to channel stream workers (Telegram edit-in-place).
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
