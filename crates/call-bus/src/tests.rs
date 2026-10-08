use std::{
    error::Error,
    fmt::Debug,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::Duration,
};

use tokio::sync::Notify;

use crate::{
    CallBus, CallError, Delivery, Event, Procedure, PublishError, QUEUE_CAPACITY, RegError,
};

#[derive(Debug, thiserror::Error)]
#[error("{0}")]
struct TestError(String);

struct Echo(String);

impl Procedure for Echo {
    type Error = TestError;
    type Output = String;
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Notice(u64);

impl Event for Notice {}

struct Latch {
    open: AtomicBool,
    notify: Notify,
}

impl Latch {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            open: AtomicBool::new(false),
            notify: Notify::new(),
        })
    }

    async fn wait(&self) {
        loop {
            let notified = self.notify.notified();
            if self.open.load(Ordering::SeqCst) {
                return;
            }
            notified.await;
        }
    }

    fn open(&self) {
        self.open.store(true, Ordering::SeqCst);
        self.notify.notify_waiters();
    }
}

fn ready<T, E: Debug>(result: Result<T, E>) -> T {
    match result {
        Ok(value) => value,
        Err(error) => panic!("unexpected error: {error:?}"),
    }
}

fn lock_vec(values: &Mutex<Vec<u64>>) -> std::sync::MutexGuard<'_, Vec<u64>> {
    values.lock().unwrap_or_else(|error| error.into_inner())
}

#[tokio::test]
async fn call_returns_handler_output() {
    let bus = CallBus::new();
    ready(bus.register(|request: Echo| async move { Ok(request.0) }));
    ready(bus.seal());
    let output = ready(bus.call(Echo("ready".to_string())).await);
    assert_eq!(output, "ready");
}

#[tokio::test]
async fn second_register_is_duplicate() {
    let bus = CallBus::new();
    ready(bus.register(|request: Echo| async move { Ok(request.0) }));
    let error = bus
        .register(|request: Echo| async move { Ok(request.0) })
        .err()
        .unwrap_or_else(|| panic!("second register succeeded"));
    assert_eq!(error, RegError::Duplicate);
}

#[tokio::test]
async fn seal_without_required_handler_is_missing_and_not_sealed() {
    let bus = CallBus::new();
    ready(bus.require::<Echo>());
    let error = bus.seal().err().unwrap_or_else(|| panic!("seal succeeded"));
    assert_eq!(error, RegError::Missing);
    let error = bus
        .call(Echo("x".to_string()))
        .await
        .err()
        .unwrap_or_else(|| panic!("call succeeded"));
    assert!(matches!(error, CallError::NotSealed));
}

#[tokio::test]
async fn call_without_handler_is_missing() {
    let bus = CallBus::new();
    ready(bus.seal());
    let error = bus
        .call(Echo("x".to_string()))
        .await
        .err()
        .unwrap_or_else(|| panic!("call succeeded"));
    assert!(matches!(error, CallError::Missing));
}

#[tokio::test]
async fn two_buses_do_not_share_registration() {
    let left = CallBus::new();
    let right = CallBus::new();
    ready(left.register(|request: Echo| async move { Ok(request.0) }));
    ready(left.seal());
    ready(right.seal());
    assert!(left.call(Echo("a".to_string())).await.is_ok());
    let error = right
        .call(Echo("b".to_string()))
        .await
        .err()
        .unwrap_or_else(|| panic!("call succeeded"));
    assert!(matches!(error, CallError::Missing));
}

#[tokio::test]
async fn publish_before_seal_delivers_nothing() {
    let bus = CallBus::new();
    let calls = Arc::new(AtomicUsize::new(0));
    let seen = Arc::clone(&calls);
    ready(bus.subscribe(move |_event: Notice| {
        let seen = Arc::clone(&seen);
        async move {
            seen.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
    }));
    for delivery in [Delivery::Wait, Delivery::Queue, Delivery::Detach] {
        let error = bus
            .publish(&Notice(1), delivery)
            .await
            .err()
            .unwrap_or_else(|| panic!("publish succeeded"));
        assert!(matches!(error, PublishError::NotSealed));
    }
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn publish_without_subscribers_keeps_nothing() {
    let bus = CallBus::new();
    ready(bus.seal());
    for delivery in [Delivery::Wait, Delivery::Queue] {
        let outcome = ready(bus.publish(&Notice(7), delivery).await);
        assert_eq!(outcome.subscribers, 0);
    }
}

#[tokio::test]
async fn queue_full_rejects_without_delivery() {
    let bus = CallBus::new();
    let entered = Arc::new(Notify::new());
    let release = Latch::new();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let entered_flag = Arc::clone(&entered);
    let release_flag = Arc::clone(&release);
    let seen_values = Arc::clone(&seen);
    ready(bus.subscribe(move |event: Notice| {
        let entered_flag = Arc::clone(&entered_flag);
        let release_flag = Arc::clone(&release_flag);
        let seen_values = Arc::clone(&seen_values);
        async move {
            lock_vec(&seen_values).push(event.0);
            entered_flag.notify_waiters();
            release_flag.wait().await;
            Ok(())
        }
    }));
    ready(bus.seal());
    let entered_wait = entered.notified();
    ready(bus.publish(&Notice(0), Delivery::Queue).await);
    ready(tokio::time::timeout(Duration::from_secs(1), entered_wait).await);
    for value in 1..QUEUE_CAPACITY {
        ready(bus.publish(&Notice(value as u64), Delivery::Queue).await);
    }
    let rejected = Notice(QUEUE_CAPACITY as u64);
    let error = bus
        .publish(&rejected, Delivery::Queue)
        .await
        .err()
        .unwrap_or_else(|| panic!("full publish succeeded"));
    assert!(matches!(error, PublishError::Full));
    assert_eq!(rejected.0, QUEUE_CAPACITY as u64);
    release.open();
    bus.close().await;
    assert!(!lock_vec(&seen).contains(&(QUEUE_CAPACITY as u64)));
}

#[tokio::test]
async fn close_waits_for_a_running_queue_handler() {
    let bus = CallBus::new();
    let entered = Arc::new(Notify::new());
    let release = Latch::new();
    let finished = Arc::new(AtomicUsize::new(0));
    let entered_flag = Arc::clone(&entered);
    let release_flag = Arc::clone(&release);
    let finished_flag = Arc::clone(&finished);
    ready(bus.subscribe(move |_event: Notice| {
        let entered_flag = Arc::clone(&entered_flag);
        let release_flag = Arc::clone(&release_flag);
        let finished_flag = Arc::clone(&finished_flag);
        async move {
            entered_flag.notify_waiters();
            release_flag.wait().await;
            finished_flag.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
    }));
    ready(bus.seal());
    let entered_wait = entered.notified();
    ready(bus.publish(&Notice(1), Delivery::Queue).await);
    ready(tokio::time::timeout(Duration::from_secs(1), entered_wait).await);
    let bus_for_close = Arc::clone(&bus);
    let closing = tokio::spawn(async move { bus_for_close.close().await });
    tokio::task::yield_now().await;
    assert!(!closing.is_finished());
    release.open();
    ready(ready(
        tokio::time::timeout(Duration::from_secs(1), closing).await,
    ));
    assert_eq!(finished.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn publish_after_close_returns_closed_without_delivery() {
    let bus = CallBus::new();
    let calls = Arc::new(AtomicUsize::new(0));
    let seen = Arc::clone(&calls);
    ready(bus.subscribe(move |_event: Notice| {
        let seen = Arc::clone(&seen);
        async move {
            seen.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
    }));
    ready(bus.seal());
    bus.close().await;
    let event = Notice(4);
    for delivery in [Delivery::Wait, Delivery::Queue, Delivery::Detach] {
        let error = bus
            .publish(&event, delivery)
            .await
            .err()
            .unwrap_or_else(|| panic!("publish succeeded"));
        assert!(matches!(error, PublishError::Closed));
    }
    assert_eq!(event.0, 4);
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn wait_invokes_both_subscribers_and_returns_the_failure_once() {
    let bus = CallBus::new();
    let calls = Arc::new(AtomicUsize::new(0));
    let ok_calls = Arc::clone(&calls);
    let err_calls = Arc::clone(&calls);
    ready(bus.subscribe(move |_event: Notice| {
        let ok_calls = Arc::clone(&ok_calls);
        async move {
            ok_calls.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
    }));
    ready(bus.subscribe(move |_event: Notice| {
        let err_calls = Arc::clone(&err_calls);
        async move {
            err_calls.fetch_add(1, Ordering::SeqCst);
            Err(Box::new(TestError("boom".to_string())) as Box<dyn Error + Send + Sync>)
        }
    }));
    ready(bus.seal());
    let error = bus
        .publish(&Notice(1), Delivery::Wait)
        .await
        .err()
        .unwrap_or_else(|| panic!("wait succeeded"));
    assert!(matches!(error, PublishError::Subscriber(errors) if errors.len() == 1));
    assert_eq!(calls.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn detach_is_the_only_mode_that_drops_before_close() {
    let bus = CallBus::new();
    let handled = Arc::new(AtomicUsize::new(0));
    let release = Latch::new();
    let handled_flag = Arc::clone(&handled);
    let release_flag = Arc::clone(&release);
    ready(bus.subscribe(move |_event: Notice| {
        let handled_flag = Arc::clone(&handled_flag);
        let release_flag = Arc::clone(&release_flag);
        async move {
            handled_flag.fetch_add(1, Ordering::SeqCst);
            release_flag.wait().await;
            Ok(())
        }
    }));
    ready(bus.seal());
    ready(bus.publish(&Notice(0), Delivery::Detach).await);
    ready(
        tokio::time::timeout(Duration::from_secs(1), async {
            while handled.load(Ordering::SeqCst) == 0 {
                tokio::task::yield_now().await;
            }
        })
        .await,
    );
    for value in 1..QUEUE_CAPACITY + 4 {
        ready(bus.publish(&Notice(value as u64), Delivery::Detach).await);
    }
    assert_eq!(handled.load(Ordering::SeqCst), 1);
    release.open();
    ready(
        tokio::time::timeout(Duration::from_secs(1), async {
            while handled.load(Ordering::SeqCst) <= 1 {
                tokio::task::yield_now().await;
            }
        })
        .await,
    );
    let delivered = handled.load(Ordering::SeqCst);
    assert!(delivered < QUEUE_CAPACITY + 4);
    bus.close().await;
    let error = bus
        .publish(&Notice(99), Delivery::Detach)
        .await
        .err()
        .unwrap_or_else(|| panic!("detach succeeded"));
    assert!(matches!(error, PublishError::Closed));
}
