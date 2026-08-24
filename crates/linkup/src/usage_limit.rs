//! Shared Linkup API usage-limit coordination.

use std::{collections::VecDeque, sync::Mutex, time::Duration};

use tokio::time::{Instant, sleep_until};

/// Linkup Search API request-start budget per one-second window.
pub(crate) const LINKUP_REQUESTS_PER_SECOND: usize = 10;
const LINKUP_USAGE_WINDOW: Duration = Duration::from_secs(1);

#[derive(Debug, Default)]
struct State {
    started_at: VecDeque<Instant>,
}

/// Shared sliding-window gate for every request made by one Linkup client.
#[derive(Debug, Default)]
pub(crate) struct UsageLimitCoordinator {
    state: Mutex<State>,
}

impl UsageLimitCoordinator {
    /// Wait until another request start fits inside the documented API budget.
    pub(crate) async fn acquire(&self) {
        loop {
            let wait_until = {
                let now = Instant::now();
                let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
                while state
                    .started_at
                    .front()
                    .is_some_and(|started| now.duration_since(*started) >= LINKUP_USAGE_WINDOW)
                {
                    state.started_at.pop_front();
                }
                if state.started_at.len() < LINKUP_REQUESTS_PER_SECOND {
                    state.started_at.push_back(now);
                    return;
                }
                state
                    .started_at
                    .front()
                    .and_then(|started| started.checked_add(LINKUP_USAGE_WINDOW))
            };

            let Some(wait_until) = wait_until else {
                tokio::task::yield_now().await;
                continue;
            };
            #[cfg(feature = "tracing")]
            tracing::debug!(
                remaining = ?wait_until.saturating_duration_since(Instant::now()),
                "Linkup request waiting for the shared API usage limit"
            );
            sleep_until(wait_until).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(start_paused = true)]
    async fn eleventh_request_waits_for_the_next_usage_window() {
        let coordinator = UsageLimitCoordinator::default();
        let started = Instant::now();
        for _ in 0..LINKUP_REQUESTS_PER_SECOND {
            coordinator.acquire().await;
        }

        coordinator.acquire().await;

        assert_eq!(Instant::now() - started, LINKUP_USAGE_WINDOW);
    }

    #[tokio::test(start_paused = true)]
    async fn expired_starts_release_only_the_available_budget() {
        let coordinator = UsageLimitCoordinator::default();
        let started = Instant::now();
        for _ in 0..LINKUP_REQUESTS_PER_SECOND {
            coordinator.acquire().await;
        }
        tokio::time::advance(Duration::from_millis(500)).await;

        let pending = coordinator.acquire();
        tokio::pin!(pending);
        tokio::select! {
            () = &mut pending => panic!("request started before the usage window expired"),
            () = tokio::time::sleep(Duration::from_millis(499)) => {},
        }
        assert_eq!(Instant::now() - started, Duration::from_millis(999));
        pending.await;

        assert_eq!(Instant::now() - started, LINKUP_USAGE_WINDOW);
    }
}
