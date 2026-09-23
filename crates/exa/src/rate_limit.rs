//! Shared Exa cooldown across concurrent agent sessions.

use std::{sync::Mutex, time::Duration};

use tokio::{
    sync::watch,
    time::{Instant, sleep_until},
};

const MAX_COOLDOWN: Duration = Duration::from_secs(60);
const BUFFER: Duration = Duration::from_secs(5);

#[derive(Clone, Copy)]
enum State {
    Open,
    Cooling(Instant),
    Probing,
}

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
    pub(crate) async fn acquire(&self) -> Permit<'_> {
        let mut changed = self.changed.subscribe();
        loop {
            let (state, probe) = {
                let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
                match *state {
                    State::Cooling(until) if Instant::now() >= until => {
                        *state = State::Probing;
                        (State::Probing, true)
                    },
                    other => (other, false),
                }
            };
            if probe || matches!(state, State::Open) {
                return Permit {
                    gate: self,
                    probe,
                    completed: false,
                };
            }
            match state {
                State::Cooling(until) => {
                    tokio::select! {
                        () = sleep_until(until) => {},
                        result = changed.changed() => { let _ = result; },
                    }
                },
                State::Probing => {
                    let _ = changed.changed().await;
                },
                State::Open => {},
            }
        }
    }

    fn update(&self, probe: bool, retry_after: Option<&str>) -> bool {
        let Some(seconds) = retry_after.and_then(|value| value.parse::<u64>().ok()) else {
            if probe {
                self.release_probe();
            }
            return false;
        };
        let delay = Duration::from_secs(seconds)
            .saturating_add(BUFFER)
            .min(MAX_COOLDOWN);
        let until = Instant::now() + delay;
        {
            let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
            *state = State::Cooling(match *state {
                State::Cooling(current) => current.max(until),
                _ => until,
            });
        }
        self.signal();
        true
    }

    fn release_probe(&self) {
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        if matches!(*state, State::Probing) {
            *state = State::Open;
            drop(state);
            self.signal();
        }
    }

    fn signal(&self) {
        self.changed
            .send_modify(|version| *version = version.wrapping_add(1));
    }
}

/// A cancelled probe reopens the gate.
pub(crate) struct Permit<'a> {
    gate: &'a RateLimitCoordinator,
    probe: bool,
    completed: bool,
}

impl Permit<'_> {
    pub(crate) fn complete(mut self, limited: bool, retry_after: Option<&str>) -> bool {
        let cooldown = if limited {
            self.gate.update(self.probe, retry_after)
        } else {
            if self.probe {
                self.gate.release_probe();
            }
            false
        };
        self.completed = true;
        cooldown
    }
}

impl Drop for Permit<'_> {
    fn drop(&mut self) {
        if self.probe && !self.completed {
            self.gate.release_probe();
        }
    }
}

#[cfg(test)]
mod tests {
    use {super::*, std::sync::Arc};

    #[tokio::test(start_paused = true)]
    async fn only_one_probe_runs_after_cooldown() {
        let gate = Arc::new(RateLimitCoordinator::default());
        assert!(gate.acquire().await.complete(true, Some("0")));
        tokio::time::advance(BUFFER).await;
        let first = gate.acquire().await;
        assert!(first.probe);
        let other = Arc::clone(&gate);
        let waiter = tokio::spawn(async move { other.acquire().await.probe });
        tokio::task::yield_now().await;
        assert!(!waiter.is_finished());
        first.complete(false, None);
        assert!(
            !waiter
                .await
                .unwrap_or_else(|error| panic!("probe task failed: {error}"))
        );
    }

    #[tokio::test(start_paused = true)]
    async fn another_rate_limit_extends_cooldown() {
        let gate = Arc::new(RateLimitCoordinator::default());
        let first = gate.acquire().await;
        let concurrent = gate.acquire().await;
        assert!(first.complete(true, Some("1")));
        assert!(concurrent.complete(true, Some("2")));
        tokio::time::advance(Duration::from_secs(6)).await;
        let other = Arc::clone(&gate);
        let waiter = tokio::spawn(async move { other.acquire().await.probe });
        tokio::task::yield_now().await;
        assert!(!waiter.is_finished());
        tokio::time::advance(Duration::from_secs(1)).await;
        assert!(
            waiter
                .await
                .unwrap_or_else(|error| panic!("probe task failed: {error}"))
        );
    }

    #[tokio::test(start_paused = true)]
    async fn dropped_probe_reopens_gate() {
        let gate = RateLimitCoordinator::default();
        assert!(gate.acquire().await.complete(true, Some("0")));
        tokio::time::advance(BUFFER).await;
        let probe = gate.acquire().await;
        assert!(probe.probe);
        drop(probe);
        assert!(!gate.acquire().await.probe);
    }
}
