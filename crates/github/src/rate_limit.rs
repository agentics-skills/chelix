//! Process-wide GitHub rate-limit coordination for the shared client.

use std::{sync::Mutex, time::Duration};

use tokio::{
    sync::watch,
    time::{Instant, sleep_until},
};

#[derive(Debug, Clone, Copy)]
enum State {
    Open,
    CoolingDown { until: Instant },
    ProbeInFlight,
}

/// Shared gate that coordinates one GitHub rate limit across concurrent calls.
pub(crate) struct RateLimitCoordinator {
    state: Mutex<State>,
    changed: watch::Sender<u64>,
}

impl Default for RateLimitCoordinator {
    fn default() -> Self {
        let (changed, receiver) = watch::channel(0);
        drop(receiver);
        Self {
            state: Mutex::new(State::Open),
            changed,
        }
    }
}

impl RateLimitCoordinator {
    /// Wait until requests may proceed. After a cooldown, exactly one caller is
    /// admitted as the probe while every other caller waits for its outcome.
    pub(crate) async fn acquire(&self) -> RateLimitPermit<'_> {
        let mut changed = self.changed.subscribe();
        loop {
            let action = {
                let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
                match *state {
                    State::Open => AcquireAction::Ready { probe: false },
                    State::CoolingDown { until } if Instant::now() >= until => {
                        *state = State::ProbeInFlight;
                        AcquireAction::Ready { probe: true }
                    },
                    State::CoolingDown { until } => AcquireAction::SleepUntil(until),
                    State::ProbeInFlight => AcquireAction::WaitForChange,
                }
            };

            match action {
                AcquireAction::Ready { probe } => {
                    #[cfg(feature = "tracing")]
                    if probe {
                        tracing::debug!("GitHub shared rate-limit probe admitted");
                    }
                    return RateLimitPermit {
                        coordinator: self,
                        probe,
                        completed: false,
                    };
                },
                AcquireAction::SleepUntil(until) => {
                    #[cfg(feature = "tracing")]
                    tracing::debug!(
                        remaining = ?until.saturating_duration_since(Instant::now()),
                        "GitHub request waiting for shared rate-limit cooldown"
                    );
                    tokio::select! {
                        () = sleep_until(until) => {},
                        result = changed.changed() => {
                            let _ = result;
                        },
                    }
                },
                AcquireAction::WaitForChange => {
                    #[cfg(feature = "tracing")]
                    tracing::debug!("GitHub request waiting for shared rate-limit probe");
                    let _ = changed.changed().await;
                },
            }
        }
    }

    fn complete(&self, probe: bool, rate_limited: bool, cooldown_ms: Option<u64>) {
        if rate_limited {
            if let Some(cooldown_ms) = cooldown_ms {
                self.start_cooldown(cooldown_ms);
            } else if probe {
                self.release_probe();
            }
            return;
        }
        if probe {
            self.release_probe();
        }
    }

    fn start_cooldown(&self, cooldown_ms: u64) {
        let until = Instant::now() + Duration::from_millis(cooldown_ms);
        let changed = {
            let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
            let next_until = match *state {
                State::CoolingDown { until: current } => current.max(until),
                State::Open | State::ProbeInFlight => until,
            };
            let changed = !matches!(
                *state,
                State::CoolingDown { until: current } if current == next_until
            );
            *state = State::CoolingDown { until: next_until };
            changed
        };
        if changed {
            #[cfg(feature = "tracing")]
            tracing::warn!(
                cooldown_ms,
                "GitHub shared rate-limit cooldown started or extended"
            );
            self.signal_change();
        }
    }

    fn release_probe(&self) {
        let changed = {
            let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
            if matches!(*state, State::ProbeInFlight) {
                *state = State::Open;
                true
            } else {
                false
            }
        };
        if changed {
            #[cfg(feature = "tracing")]
            tracing::debug!("GitHub shared rate-limit probe released");
            self.signal_change();
        }
    }

    fn signal_change(&self) {
        self.changed
            .send_modify(|generation| *generation = generation.wrapping_add(1));
    }
}

enum AcquireAction {
    Ready { probe: bool },
    SleepUntil(Instant),
    WaitForChange,
}

/// Cancellation-safe lease for a request admitted through the shared gate.
pub(crate) struct RateLimitPermit<'a> {
    coordinator: &'a RateLimitCoordinator,
    probe: bool,
    completed: bool,
}

impl RateLimitPermit<'_> {
    /// Record the response and update the shared gate before releasing the lease.
    pub(crate) fn complete(mut self, rate_limited: bool, cooldown_ms: Option<u64>) {
        self.coordinator
            .complete(self.probe, rate_limited, cooldown_ms);
        self.completed = true;
    }

    #[cfg(test)]
    fn is_probe(&self) -> bool {
        self.probe
    }
}

impl Drop for RateLimitPermit<'_> {
    fn drop(&mut self) {
        if self.probe && !self.completed {
            #[cfg(feature = "tracing")]
            tracing::warn!(
                "GitHub shared rate-limit probe ended without a response; reopening the gate"
            );
            self.coordinator.release_probe();
        }
    }
}

#[cfg(test)]
mod tests {
    use {super::*, std::sync::Arc, tokio::sync::oneshot};

    #[tokio::test(start_paused = true)]
    async fn only_one_probe_runs_after_a_shared_cooldown() {
        let coordinator = Arc::new(RateLimitCoordinator::default());
        coordinator.acquire().await.complete(true, Some(1_000));

        let probe = coordinator.acquire().await;
        assert!(probe.is_probe());

        let waiting_coordinator = Arc::clone(&coordinator);
        let (acquired_tx, mut acquired_rx) = oneshot::channel();
        let waiter = tokio::spawn(async move {
            let permit = waiting_coordinator.acquire().await;
            let _ = acquired_tx.send(permit.is_probe());
            permit.complete(false, None);
        });
        tokio::task::yield_now().await;
        assert!(matches!(
            acquired_rx.try_recv(),
            Err(oneshot::error::TryRecvError::Empty)
        ));

        probe.complete(false, None);
        assert_eq!(acquired_rx.await, Ok(false));
        waiter
            .await
            .unwrap_or_else(|error| panic!("waiter task failed: {error}"));
    }

    #[tokio::test(start_paused = true)]
    async fn concurrent_rate_limits_extend_the_shared_deadline() {
        let coordinator = RateLimitCoordinator::default();
        let first = coordinator.acquire().await;
        let second = coordinator.acquire().await;
        let started = Instant::now();

        first.complete(true, Some(1_000));
        second.complete(true, Some(2_000));
        let probe = coordinator.acquire().await;

        assert!(probe.is_probe());
        assert!(Instant::now() >= started + Duration::from_millis(2_000));
        probe.complete(false, None);
    }

    #[tokio::test(start_paused = true)]
    async fn dropping_a_cancelled_probe_reopens_the_gate() {
        let coordinator = RateLimitCoordinator::default();
        coordinator.acquire().await.complete(true, Some(1_000));

        let probe = coordinator.acquire().await;
        assert!(probe.is_probe());
        drop(probe);

        let next = coordinator.acquire().await;
        assert!(!next.is_probe());
        next.complete(false, None);
    }
}
