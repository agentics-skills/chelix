use std::{
    any::{Any, TypeId},
    collections::{HashMap, HashSet},
    future::Future,
    pin::Pin,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
};

use tokio::sync::{Notify, OwnedSemaphorePermit, Semaphore, broadcast, mpsc};

use crate::{
    error::{CallError, PublishError, RegError},
    procedure::{Delivery, Event, Procedure},
};

/// Bound of one subscriber queue and of one `Detach` broadcast buffer.
pub const QUEUE_CAPACITY: usize = 16;

type BoxFuture<T> = Pin<Box<dyn Future<Output = T> + Send>>;
type ErasedError = Box<dyn std::error::Error + Send + Sync>;

type HandlerFn<P> = Arc<
    dyn Fn(P) -> BoxFuture<Result<<P as Procedure>::Output, <P as Procedure>::Error>> + Send + Sync,
>;

type SubscriberFn<E> = Arc<dyn Fn(E) -> BoxFuture<Result<(), ErasedError>> + Send + Sync>;

struct QueuedItem<E> {
    event: E,
    _permit: OwnedSemaphorePermit,
}

struct QueuePort<E> {
    tx: mpsc::Sender<QueuedItem<E>>,
    sem: Arc<Semaphore>,
}

struct EventHub<E> {
    subscribers: Mutex<Vec<SubscriberFn<E>>>,
    queues: Mutex<Vec<QueuePort<E>>>,
    detach: Mutex<Option<broadcast::Sender<E>>>,
}

struct Shared {
    queue_inflight: AtomicUsize,
    wait_inflight: AtomicUsize,
    notify: Notify,
}

struct BusInner {
    sealed: bool,
    closed: bool,
    handlers: HashMap<TypeId, Box<dyn Any + Send + Sync>>,
    required: HashSet<TypeId>,
    hubs: HashMap<TypeId, Arc<dyn Any + Send + Sync>>,
}

/// Process-local registry of typed procedures and events.
pub struct CallBus {
    inner: Mutex<BusInner>,
    shared: Arc<Shared>,
}

/// How many subscribers were addressed by one `publish`.
pub struct PublishOutcome {
    pub subscribers: usize,
}

impl CallBus {
    /// Create a private bus. Tests and the process each hold their own `Arc`.
    #[must_use]
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            inner: Mutex::new(BusInner {
                sealed: false,
                closed: false,
                handlers: HashMap::new(),
                required: HashSet::new(),
                hubs: HashMap::new(),
            }),
            shared: Arc::new(Shared {
                queue_inflight: AtomicUsize::new(0),
                wait_inflight: AtomicUsize::new(0),
                notify: Notify::new(),
            }),
        })
    }

    /// Register the only handler for procedure `P`.
    pub fn register<P, F, Fut>(&self, handler: F) -> Result<(), RegError>
    where
        P: Procedure,
        F: Fn(P) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<P::Output, P::Error>> + Send + 'static,
    {
        let mut inner = lock(&self.inner);
        if inner.sealed {
            return Err(RegError::Sealed);
        }
        let key = TypeId::of::<P>();
        if inner.handlers.contains_key(&key) {
            return Err(RegError::Duplicate);
        }
        let handler: HandlerFn<P> = Arc::new(move |request| Box::pin(handler(request)));
        inner.handlers.insert(key, Box::new(handler));
        Ok(())
    }

    /// Mark `P` as required for a successful `seal`.
    pub fn require<P: Procedure>(&self) -> Result<(), RegError> {
        let mut inner = lock(&self.inner);
        if inner.sealed {
            return Err(RegError::Sealed);
        }
        inner.required.insert(TypeId::of::<P>());
        Ok(())
    }

    /// Add a subscriber for event `E`. Several subscribers are allowed.
    pub fn subscribe<E, F, Fut>(&self, subscriber: F) -> Result<(), RegError>
    where
        E: Event,
        F: Fn(E) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<(), ErasedError>> + Send + 'static,
    {
        let hub = {
            let mut inner = lock(&self.inner);
            if inner.sealed {
                return Err(RegError::Sealed);
            }
            let key = TypeId::of::<E>();
            if let Some(existing) = inner.hubs.get(&key) {
                existing
                    .clone()
                    .downcast::<EventHub<E>>()
                    .unwrap_or_else(|_| mismatched())
            } else {
                let hub = Arc::new(EventHub::<E> {
                    subscribers: Mutex::new(Vec::new()),
                    queues: Mutex::new(Vec::new()),
                    detach: Mutex::new(None),
                });
                inner
                    .hubs
                    .insert(key, Arc::clone(&hub) as Arc<dyn Any + Send + Sync>);
                hub
            }
        };
        let subscriber: SubscriberFn<E> = Arc::new(move |event| Box::pin(subscriber(event)));
        let _sealed = lock(&self.inner);
        if _sealed.sealed {
            return Err(RegError::Sealed);
        }
        hub.install(subscriber, Arc::clone(&self.shared));
        Ok(())
    }

    /// Close registration. Missing required handlers leave the bus unsealed.
    pub fn seal(&self) -> Result<(), RegError> {
        let mut inner = lock(&self.inner);
        if inner.sealed {
            return Err(RegError::Sealed);
        }
        if inner
            .required
            .iter()
            .any(|key| !inner.handlers.contains_key(key))
        {
            return Err(RegError::Missing);
        }
        inner.sealed = true;
        Ok(())
    }

    /// Invoke the handler for `P`.
    pub async fn call<P: Procedure>(&self, request: P) -> Result<P::Output, CallError<P::Error>> {
        #[cfg(feature = "tracing")]
        {
            use tracing::Instrument;
            self.call_inner(request)
                .instrument(tracing::info_span!(
                    "call_bus.call",
                    r#type = std::any::type_name::<P>()
                ))
                .await
        }
        #[cfg(not(feature = "tracing"))]
        {
            self.call_inner(request).await
        }
    }

    async fn call_inner<P: Procedure>(&self, request: P) -> Result<P::Output, CallError<P::Error>> {
        let started = std::time::Instant::now();
        let result = self.dispatch_call(request).await;
        record_metric("call", started, result.is_ok());
        result
    }

    async fn dispatch_call<P: Procedure>(
        &self,
        request: P,
    ) -> Result<P::Output, CallError<P::Error>> {
        let handler = {
            let inner = lock(&self.inner);
            if !inner.sealed {
                return Err(CallError::NotSealed);
            }
            let Some(stored) = inner.handlers.get(&TypeId::of::<P>()) else {
                return Err(CallError::Missing);
            };
            stored
                .downcast_ref::<HandlerFn<P>>()
                .unwrap_or_else(mismatched)
                .clone()
        };
        handler(request).await.map_err(CallError::Failed)
    }

    /// Deliver `event` with the selected mode.
    pub async fn publish<E: Event>(
        &self,
        event: &E,
        delivery: Delivery,
    ) -> Result<PublishOutcome, PublishError> {
        #[cfg(feature = "tracing")]
        {
            use tracing::Instrument;
            self.publish_inner(event, delivery)
                .instrument(tracing::info_span!(
                    "call_bus.publish",
                    r#type = std::any::type_name::<E>()
                ))
                .await
        }
        #[cfg(not(feature = "tracing"))]
        {
            self.publish_inner(event, delivery).await
        }
    }

    async fn publish_inner<E: Event>(
        &self,
        event: &E,
        delivery: Delivery,
    ) -> Result<PublishOutcome, PublishError> {
        let started = std::time::Instant::now();
        let result = self.dispatch_publish(event, delivery).await;
        record_metric("publish", started, result.is_ok());
        result
    }

    async fn dispatch_publish<E: Event>(
        &self,
        event: &E,
        delivery: Delivery,
    ) -> Result<PublishOutcome, PublishError> {
        let hub = {
            let inner = lock(&self.inner);
            if !inner.sealed {
                return Err(PublishError::NotSealed);
            }
            if inner.closed {
                return Err(PublishError::Closed);
            }
            inner.hubs.get(&TypeId::of::<E>()).cloned().map(|hub| {
                hub.downcast::<EventHub<E>>()
                    .unwrap_or_else(|_| mismatched())
            })
        };
        let Some(hub) = hub else {
            return Ok(PublishOutcome { subscribers: 0 });
        };
        match delivery {
            Delivery::Wait => self.publish_wait(&hub, event).await,
            Delivery::Queue => self.publish_queue(&hub, event),
            Delivery::Detach => self.publish_detach(&hub, event),
        }
    }

    async fn publish_wait<E: Event>(
        &self,
        hub: &EventHub<E>,
        event: &E,
    ) -> Result<PublishOutcome, PublishError> {
        let subscribers = {
            let inner = lock(&self.inner);
            if inner.closed {
                return Err(PublishError::Closed);
            }
            let subscribers = lock(&hub.subscribers).clone();
            if subscribers.is_empty() {
                return Ok(PublishOutcome { subscribers: 0 });
            }
            self.shared.wait_inflight.fetch_add(1, Ordering::AcqRel);
            subscribers
        };
        let _wait_guard = WaitInflightGuard {
            shared: Arc::clone(&self.shared),
        };
        let mut errors = Vec::new();
        for subscriber in &subscribers {
            if let Err(error) = subscriber(event.clone()).await {
                errors.push(error);
            }
        }
        drop(_wait_guard);
        if errors.is_empty() {
            Ok(PublishOutcome {
                subscribers: subscribers.len(),
            })
        } else {
            Err(PublishError::Subscriber(errors))
        }
    }

    fn publish_queue<E: Event>(
        &self,
        hub: &EventHub<E>,
        event: &E,
    ) -> Result<PublishOutcome, PublishError> {
        let inner = lock(&self.inner);
        if inner.closed {
            return Err(PublishError::Closed);
        }
        let queues = lock(&hub.queues);
        if queues.is_empty() {
            return Ok(PublishOutcome { subscribers: 0 });
        }
        if queues.iter().any(|queue| queue.tx.capacity() == 0) {
            return Err(PublishError::Full);
        }
        let mut permits = Vec::with_capacity(queues.len());
        for queue in queues.iter() {
            match queue.sem.clone().try_acquire_owned() {
                Ok(permit) => permits.push(permit),
                Err(_) => return Err(PublishError::Full),
            }
        }
        self.shared
            .queue_inflight
            .fetch_add(queues.len(), Ordering::AcqRel);
        for (queue, permit) in queues.iter().zip(permits) {
            queue
                .tx
                .try_send(QueuedItem {
                    event: event.clone(),
                    _permit: permit,
                })
                .unwrap_or_else(|_| mismatched());
        }
        Ok(PublishOutcome {
            subscribers: queues.len(),
        })
    }

    fn publish_detach<E: Event>(
        &self,
        hub: &EventHub<E>,
        event: &E,
    ) -> Result<PublishOutcome, PublishError> {
        let inner = lock(&self.inner);
        if inner.closed {
            return Err(PublishError::Closed);
        }
        let detach = lock(&hub.detach);
        let Some(sender) = detach.as_ref() else {
            return Ok(PublishOutcome { subscribers: 0 });
        };
        let subscribers = sender.receiver_count();
        if subscribers == 0 {
            return Ok(PublishOutcome { subscribers: 0 });
        }
        match sender.send(event.clone()) {
            Ok(_) => Ok(PublishOutcome { subscribers }),
            Err(_) => Ok(PublishOutcome { subscribers: 0 }),
        }
    }

    /// Stop accepting events and wait for accepted `Queue` and started `Wait` handlers.
    pub async fn close(&self) {
        lock(&self.inner).closed = true;
        loop {
            let notified = self.shared.notify.notified();
            if self.shared.queue_inflight.load(Ordering::Acquire) == 0
                && self.shared.wait_inflight.load(Ordering::Acquire) == 0
            {
                break;
            }
            notified.await;
        }
    }
}

impl<E: Event> EventHub<E> {
    fn install(self: &Arc<Self>, subscriber: SubscriberFn<E>, shared: Arc<Shared>) {
        let (queue_tx, queue_rx) = mpsc::channel(QUEUE_CAPACITY);
        let sem = Arc::new(Semaphore::new(QUEUE_CAPACITY));
        let detach_rx = {
            let mut detach = lock(&self.detach);
            if let Some(sender) = detach.as_ref() {
                sender.subscribe()
            } else {
                let (sender, receiver) = broadcast::channel(QUEUE_CAPACITY);
                *detach = Some(sender);
                receiver
            }
        };
        lock(&self.queues).push(QueuePort { tx: queue_tx, sem });
        lock(&self.subscribers).push(Arc::clone(&subscriber));
        spawn_queue_worker(queue_rx, Arc::clone(&subscriber), shared);
        spawn_detach_worker(detach_rx, subscriber);
    }
}

fn spawn_queue_worker<E: Event>(
    mut rx: mpsc::Receiver<QueuedItem<E>>,
    subscriber: SubscriberFn<E>,
    shared: Arc<Shared>,
) {
    tokio::spawn(async move {
        while let Some(item) = rx.recv().await {
            if let Err(error) = subscriber(item.event).await {
                record_subscriber_error(&error);
            }
            drop(item._permit);
            shared.queue_inflight.fetch_sub(1, Ordering::AcqRel);
            shared.notify.notify_waiters();
        }
    });
}

fn spawn_detach_worker<E: Event>(mut rx: broadcast::Receiver<E>, subscriber: SubscriberFn<E>) {
    tokio::spawn(async move {
        loop {
            match rx.recv().await {
                Ok(event) => {
                    let _ = subscriber(event).await;
                },
                Err(broadcast::error::RecvError::Lagged(_)) => {},
                Err(broadcast::error::RecvError::Closed) => break,
            }
        }
    });
}

struct WaitInflightGuard {
    shared: Arc<Shared>,
}

impl Drop for WaitInflightGuard {
    fn drop(&mut self) {
        self.shared.wait_inflight.fetch_sub(1, Ordering::AcqRel);
        self.shared.notify.notify_waiters();
    }
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|error| error.into_inner())
}

fn record_metric(op: &'static str, started: std::time::Instant, ok: bool) {
    #[cfg(feature = "metrics")]
    {
        let result = if ok {
            "ok"
        } else {
            "error"
        };
        metrics::counter!(
            chelix_metrics::call_bus::CALLS_TOTAL,
            "op" => op,
            "result" => result
        )
        .increment(1);
        metrics::histogram!(chelix_metrics::call_bus::CALL_DURATION_SECONDS, "op" => op)
            .record(started.elapsed().as_secs_f64());
    }
    #[cfg(not(feature = "metrics"))]
    {
        let _ = (op, started, ok);
    }
}

fn record_subscriber_error(error: &ErasedError) {
    #[cfg(feature = "tracing")]
    tracing::error!(%error, "call bus queue subscriber failed");
    #[cfg(feature = "metrics")]
    metrics::counter!(
        chelix_metrics::call_bus::CALLS_TOTAL,
        "op" => "publish",
        "result" => "error"
    )
    .increment(1);
    #[cfg(not(any(feature = "tracing", feature = "metrics")))]
    {
        let _ = error;
    }
}

fn mismatched<T>() -> T {
    panic!("call bus type mismatch");
}
