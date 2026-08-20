//! Shared request queue and DuckDuckGo start spacing.

use std::{
    sync::{
        Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use tokio::{
    sync::{Semaphore, SemaphorePermit},
    time::{Instant, sleep_until},
};

use crate::error::{Error, Result};

pub(crate) const BASE_INTERVAL: Duration = Duration::from_secs(5);

#[derive(Debug, Default)]
struct State {
    last_started_at: Option<Instant>,
}

/// Shared FIFO gate for all calls registered against one client.
#[derive(Debug)]
pub(crate) struct RequestCoordinator {
    serial: Semaphore,
    waiting: AtomicUsize,
    state: Mutex<State>,
}

impl Default for RequestCoordinator {
    fn default() -> Self {
        Self {
            serial: Semaphore::new(1),
            waiting: AtomicUsize::new(0),
            state: Mutex::new(State::default()),
        }
    }
}

impl RequestCoordinator {
    pub(crate) async fn acquire(&self) -> Result<RequestPermit<'_>> {
        let waiting = WaitingPosition::new(&self.waiting);
        let permit =
            self.serial.acquire().await.map_err(|error| {
                Error::message(format!("DuckDuckGo request queue closed: {error}"))
            })?;
        let position = waiting.start();
        Ok(RequestPermit {
            coordinator: self,
            _permit: permit,
            position,
        })
    }

    async fn wait_before_start(&self, required_interval: Duration) {
        let deadline = self
            .state
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .last_started_at
            .map(|started| started + required_interval);
        if let Some(deadline) = deadline
            && deadline > Instant::now()
        {
            #[cfg(feature = "tracing")]
            tracing::debug!(
                remaining = ?deadline.saturating_duration_since(Instant::now()),
                "DuckDuckGo request waiting in the shared queue"
            );
            sleep_until(deadline).await;
        }
        self.state
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .last_started_at = Some(Instant::now());
    }
}

fn required_interval(position: usize) -> Duration {
    if position <= 3 {
        return BASE_INTERVAL;
    }
    BASE_INTERVAL.saturating_mul(u32::try_from(position - 2).unwrap_or(u32::MAX))
}

struct WaitingPosition<'a> {
    waiting: &'a AtomicUsize,
    position: usize,
    started: bool,
}

impl<'a> WaitingPosition<'a> {
    fn new(waiting: &'a AtomicUsize) -> Self {
        Self {
            waiting,
            position: waiting.fetch_add(1, Ordering::SeqCst) + 1,
            started: false,
        }
    }

    fn start(mut self) -> usize {
        self.started = true;
        self.waiting.fetch_sub(1, Ordering::SeqCst);
        self.position
    }
}

impl Drop for WaitingPosition<'_> {
    fn drop(&mut self) {
        if !self.started {
            self.waiting.fetch_sub(1, Ordering::SeqCst);
        }
    }
}

/// Lease that serializes one complete search including its retry delay.
pub(crate) struct RequestPermit<'a> {
    coordinator: &'a RequestCoordinator,
    _permit: SemaphorePermit<'a>,
    position: usize,
}

impl RequestPermit<'_> {
    pub(crate) async fn wait_for_initial_request(&self) {
        self.coordinator
            .wait_before_start(required_interval(self.position))
            .await;
    }

    pub(crate) async fn wait_for_retry(&self, backoff: Duration) {
        self.coordinator
            .wait_before_start(backoff.max(BASE_INTERVAL))
            .await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn queue_position_matches_reference_intervals() {
        assert_eq!(required_interval(1), Duration::from_secs(5));
        assert_eq!(required_interval(3), Duration::from_secs(5));
        assert_eq!(required_interval(4), Duration::from_secs(10));
        assert_eq!(required_interval(5), Duration::from_secs(15));
    }

    #[tokio::test(start_paused = true)]
    async fn requests_are_spaced_by_the_shared_base_interval() {
        let coordinator = RequestCoordinator::default();
        let started = Instant::now();
        {
            let permit = coordinator
                .acquire()
                .await
                .unwrap_or_else(|error| panic!("first acquire failed: {error}"));
            permit.wait_for_initial_request().await;
        }
        {
            let permit = coordinator
                .acquire()
                .await
                .unwrap_or_else(|error| panic!("second acquire failed: {error}"));
            permit.wait_for_initial_request().await;
        }
        assert_eq!(Instant::now() - started, BASE_INTERVAL);
    }

    #[tokio::test(start_paused = true)]
    async fn retry_backoff_delays_every_queued_call() {
        let coordinator = RequestCoordinator::default();
        let started = Instant::now();
        {
            let permit = coordinator
                .acquire()
                .await
                .unwrap_or_else(|error| panic!("first acquire failed: {error}"));
            permit.wait_for_initial_request().await;
            permit.wait_for_retry(Duration::from_secs(12)).await;
        }
        {
            let permit = coordinator
                .acquire()
                .await
                .unwrap_or_else(|error| panic!("second acquire failed: {error}"));
            permit.wait_for_initial_request().await;
        }
        assert_eq!(Instant::now() - started, Duration::from_secs(17));
    }
}
