//! Single-worker priority queue for embedding jobs.

use std::{cmp::Ordering, collections::BinaryHeap, sync::Arc};

use {
    anyhow::{Result, anyhow},
    async_trait::async_trait,
    chelix_protocol::{EMBEDDING_MAX_BODY_BYTES, EmbeddingModelMetadata},
    tokio::sync::{Mutex, Notify, oneshot},
};

use crate::EmbeddingEngine;

pub const MAX_WAITING_JOBS: usize = 16;
pub const MAX_HTTP_IN_FLIGHT: usize = MAX_WAITING_JOBS + 1;
pub const MAX_EMBED_BODY_BYTES: usize = EMBEDDING_MAX_BODY_BYTES;

#[derive(Debug)]
pub struct QueueFull;

impl std::fmt::Display for QueueFull {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("embedding queue is full")
    }
}

impl std::error::Error for QueueFull {}

struct Job {
    priority: u32,
    seq: u64,
    text: String,
    tx: oneshot::Sender<Result<Vec<f32>>>,
}

impl PartialEq for Job {
    fn eq(&self, other: &Self) -> bool {
        self.priority == other.priority && self.seq == other.seq
    }
}

impl Eq for Job {}

impl PartialOrd for Job {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Job {
    fn cmp(&self, other: &Self) -> Ordering {
        self.priority
            .cmp(&other.priority)
            .then_with(|| other.seq.cmp(&self.seq))
    }
}

struct QueueState {
    heap: BinaryHeap<Job>,
    next_seq: u64,
}

impl QueueState {
    fn prune_closed(&mut self) {
        let open: Vec<Job> = self
            .heap
            .drain()
            .filter(|job| !job.tx.is_closed())
            .collect();
        self.heap = open.into();
    }
}

struct EmbedQueue {
    state: Mutex<QueueState>,
    notify: Notify,
    max_waiting: usize,
}

pub struct QueuedEngine {
    queue: Arc<EmbedQueue>,
    metadata: EmbeddingModelMetadata,
}

impl QueuedEngine {
    pub fn start(inner: Arc<dyn EmbeddingEngine>, max_waiting: usize) -> Arc<Self> {
        let metadata = inner.metadata().clone();
        let queue = Arc::new(EmbedQueue {
            state: Mutex::new(QueueState {
                heap: BinaryHeap::new(),
                next_seq: 0,
            }),
            notify: Notify::new(),
            max_waiting,
        });
        let worker_queue = Arc::clone(&queue);
        tokio::spawn(async move {
            worker_loop(inner, worker_queue).await;
        });
        Arc::new(Self { queue, metadata })
    }

    #[cfg(test)]
    async fn waiting_len(&self) -> usize {
        self.queue.state.lock().await.heap.len()
    }
}

async fn worker_loop(inner: Arc<dyn EmbeddingEngine>, queue: Arc<EmbedQueue>) {
    loop {
        let notified = queue.notify.notified();
        let job = {
            let mut state = queue.state.lock().await;
            state.heap.pop()
        };
        let Some(job) = job else {
            notified.await;
            continue;
        };
        if job.tx.is_closed() {
            continue;
        }
        let result = inner.embed(&job.text, 0).await;
        let _ = job.tx.send(result);
    }
}

impl EmbedQueue {
    async fn submit(&self, text: String, priority: u32) -> Result<Vec<f32>> {
        let (tx, rx) = oneshot::channel();
        {
            let mut state = self.state.lock().await;
            if state.heap.len() >= self.max_waiting {
                state.prune_closed();
            }
            if state.heap.len() >= self.max_waiting {
                return Err(anyhow!(QueueFull));
            }
            let seq = state.next_seq;
            state.next_seq = seq + 1;
            state.heap.push(Job {
                priority,
                seq,
                text,
                tx,
            });
        }
        self.notify.notify_one();
        rx.await
            .map_err(|_| anyhow!("embedding worker dropped the job"))?
    }
}

#[async_trait]
impl EmbeddingEngine for QueuedEngine {
    async fn embed(&self, text: &str, priority: u32) -> Result<Vec<f32>> {
        self.queue.submit(text.to_owned(), priority).await
    }

    fn metadata(&self) -> &EmbeddingModelMetadata {
        &self.metadata
    }
}

#[cfg(test)]
mod tests {
    use {
        super::{Job, MAX_WAITING_JOBS, QueuedEngine},
        crate::EmbeddingEngine,
        anyhow::Result,
        async_trait::async_trait,
        chelix_protocol::EmbeddingModelMetadata,
        std::{
            collections::BinaryHeap,
            sync::{
                Arc,
                atomic::{AtomicUsize, Ordering},
            },
        },
        tokio::sync::{Mutex, oneshot},
    };

    fn metadata() -> EmbeddingModelMetadata {
        EmbeddingModelMetadata {
            model_name: "test".into(),
            dimensions: 3,
            provider_key: "test".into(),
        }
    }

    #[test]
    fn heap_serves_higher_priority_before_lower_then_fifo() {
        let (tx_a, _rx_a) = oneshot::channel();
        let (tx_b, _rx_b) = oneshot::channel();
        let (tx_c, _rx_c) = oneshot::channel();
        let mut heap = BinaryHeap::new();
        heap.push(Job {
            priority: 1,
            seq: 0,
            text: "low-first".into(),
            tx: tx_a,
        });
        heap.push(Job {
            priority: 5,
            seq: 1,
            text: "high".into(),
            tx: tx_b,
        });
        heap.push(Job {
            priority: 5,
            seq: 2,
            text: "high-later".into(),
            tx: tx_c,
        });
        let first = heap.pop().unwrap_or_else(|| panic!("missing first job"));
        let second = heap.pop().unwrap_or_else(|| panic!("missing second job"));
        let third = heap.pop().unwrap_or_else(|| panic!("missing third job"));
        assert_eq!(first.text, "high");
        assert_eq!(second.text, "high-later");
        assert_eq!(third.text, "low-first");
    }

    struct RecordingEngine {
        metadata: EmbeddingModelMetadata,
        order: Mutex<Vec<String>>,
        started: AtomicUsize,
        gate: Mutex<Option<oneshot::Receiver<()>>>,
    }

    #[async_trait]
    impl EmbeddingEngine for RecordingEngine {
        async fn embed(&self, text: &str, _priority: u32) -> Result<Vec<f32>> {
            self.started.fetch_add(1, Ordering::SeqCst);
            if let Some(gate) = self.gate.lock().await.take() {
                let _ = gate.await;
            }
            self.order.lock().await.push(text.to_owned());
            Ok(vec![1.0, 2.0, 3.0])
        }

        fn metadata(&self) -> &EmbeddingModelMetadata {
            &self.metadata
        }
    }

    #[tokio::test]
    async fn higher_priority_runs_before_lower_after_in_flight_job() {
        let (release_tx, release_rx) = oneshot::channel();
        let inner = Arc::new(RecordingEngine {
            metadata: metadata(),
            order: Mutex::new(Vec::new()),
            started: AtomicUsize::new(0),
            gate: Mutex::new(Some(release_rx)),
        });
        let queued = QueuedEngine::start(inner.clone(), MAX_WAITING_JOBS);

        let first = {
            let queued = Arc::clone(&queued);
            tokio::spawn(async move { queued.embed("first", 0).await })
        };
        while inner.started.load(Ordering::SeqCst) == 0 {
            tokio::task::yield_now().await;
        }

        let low = {
            let queued = Arc::clone(&queued);
            tokio::spawn(async move { queued.embed("low", 1).await })
        };
        let high = {
            let queued = Arc::clone(&queued);
            tokio::spawn(async move { queued.embed("high", 10).await })
        };
        while queued.waiting_len().await < 2 {
            tokio::task::yield_now().await;
        }

        release_tx
            .send(())
            .unwrap_or_else(|_| panic!("release first job"));
        first
            .await
            .unwrap_or_else(|error| panic!("join first: {error}"))
            .unwrap_or_else(|error| panic!("first embed: {error}"));
        high.await
            .unwrap_or_else(|error| panic!("join high: {error}"))
            .unwrap_or_else(|error| panic!("high embed: {error}"));
        low.await
            .unwrap_or_else(|error| panic!("join low: {error}"))
            .unwrap_or_else(|error| panic!("low embed: {error}"));

        let order = inner.order.lock().await.clone();
        assert_eq!(order, vec!["first", "high", "low"]);
    }

    #[tokio::test]
    async fn submit_rejects_when_waiting_capacity_is_exhausted() {
        let (release_tx, release_rx) = oneshot::channel();
        let inner = Arc::new(RecordingEngine {
            metadata: metadata(),
            order: Mutex::new(Vec::new()),
            started: AtomicUsize::new(0),
            gate: Mutex::new(Some(release_rx)),
        });
        let queued = QueuedEngine::start(inner.clone(), 1);

        let first = {
            let queued = Arc::clone(&queued);
            tokio::spawn(async move { queued.embed("first", 0).await })
        };
        while inner.started.load(Ordering::SeqCst) == 0 {
            tokio::task::yield_now().await;
        }

        let waiting = {
            let queued = Arc::clone(&queued);
            tokio::spawn(async move { queued.embed("waiting", 0).await })
        };
        while queued.waiting_len().await == 0 {
            tokio::task::yield_now().await;
        }

        let overflow = queued.embed("overflow", 0).await;
        assert!(
            overflow
                .as_ref()
                .err()
                .is_some_and(|error| error.downcast_ref::<super::QueueFull>().is_some()),
            "expected queue full, got {overflow:?}"
        );

        release_tx
            .send(())
            .unwrap_or_else(|_| panic!("release first job"));
        first
            .await
            .unwrap_or_else(|error| panic!("join first: {error}"))
            .unwrap_or_else(|error| panic!("first embed: {error}"));
        waiting
            .await
            .unwrap_or_else(|error| panic!("join waiting: {error}"))
            .unwrap_or_else(|error| panic!("waiting embed: {error}"));
    }

    #[tokio::test]
    async fn cancelled_waiting_job_frees_capacity_for_submit() {
        let (release_tx, release_rx) = oneshot::channel();
        let inner = Arc::new(RecordingEngine {
            metadata: metadata(),
            order: Mutex::new(Vec::new()),
            started: AtomicUsize::new(0),
            gate: Mutex::new(Some(release_rx)),
        });
        let queued = QueuedEngine::start(inner.clone(), 1);

        let first = {
            let queued = Arc::clone(&queued);
            tokio::spawn(async move { queued.embed("first", 0).await })
        };
        while inner.started.load(Ordering::SeqCst) == 0 {
            tokio::task::yield_now().await;
        }

        let waiting = {
            let queued = Arc::clone(&queued);
            tokio::spawn(async move { queued.embed("waiting", 0).await })
        };
        while queued.waiting_len().await == 0 {
            tokio::task::yield_now().await;
        }
        waiting.abort();
        let _ = waiting.await;

        let next = {
            let queued = Arc::clone(&queued);
            tokio::spawn(async move { queued.embed("next", 0).await })
        };
        while queued.waiting_len().await == 0 {
            tokio::task::yield_now().await;
        }

        release_tx
            .send(())
            .unwrap_or_else(|_| panic!("release first job"));
        first
            .await
            .unwrap_or_else(|error| panic!("join first: {error}"))
            .unwrap_or_else(|error| panic!("first embed: {error}"));
        next.await
            .unwrap_or_else(|error| panic!("join next: {error}"))
            .unwrap_or_else(|error| panic!("next embed: {error}"));

        let order = inner.order.lock().await.clone();
        assert_eq!(order, vec!["first", "next"]);
    }

    #[tokio::test]
    async fn cancelled_waiting_job_is_skipped_without_new_submit() {
        let (release_tx, release_rx) = oneshot::channel();
        let inner = Arc::new(RecordingEngine {
            metadata: metadata(),
            order: Mutex::new(Vec::new()),
            started: AtomicUsize::new(0),
            gate: Mutex::new(Some(release_rx)),
        });
        let queued = QueuedEngine::start(inner.clone(), 8);

        let first = {
            let queued = Arc::clone(&queued);
            tokio::spawn(async move { queued.embed("first", 0).await })
        };
        while inner.started.load(Ordering::SeqCst) == 0 {
            tokio::task::yield_now().await;
        }

        let waiting = {
            let queued = Arc::clone(&queued);
            tokio::spawn(async move { queued.embed("waiting", 0).await })
        };
        while queued.waiting_len().await == 0 {
            tokio::task::yield_now().await;
        }
        waiting.abort();
        let _ = waiting.await;

        release_tx
            .send(())
            .unwrap_or_else(|_| panic!("release first job"));
        first
            .await
            .unwrap_or_else(|error| panic!("join first: {error}"))
            .unwrap_or_else(|error| panic!("first embed: {error}"));

        while queued.waiting_len().await != 0 {
            tokio::task::yield_now().await;
        }

        queued
            .embed("after", 0)
            .await
            .unwrap_or_else(|error| panic!("after embed: {error}"));

        let order = inner.order.lock().await.clone();
        assert_eq!(order, vec!["first", "after"]);
    }
}
