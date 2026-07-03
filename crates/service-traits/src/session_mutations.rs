use std::{collections::HashMap, sync::Arc};

use tokio::sync::{OwnedRwLockWriteGuard, OwnedSemaphorePermit, RwLock, Semaphore};

/// Coordinates per-session mutations that must not run concurrently with a chat turn.
#[derive(Debug, Default)]
pub struct SessionMutationCoordinator {
    locks: RwLock<HashMap<String, Arc<SessionMutationLock>>>,
}

#[derive(Debug)]
struct SessionMutationLock {
    acquisition_gate: Arc<RwLock<()>>,
    turn: Arc<Semaphore>,
}

impl Default for SessionMutationLock {
    fn default() -> Self {
        Self {
            acquisition_gate: Arc::new(RwLock::new(())),
            turn: Arc::new(Semaphore::new(1)),
        }
    }
}

/// Permit held while a chat turn owns a session.
#[derive(Debug)]
pub struct SessionTurnPermit {
    _turn: OwnedSemaphorePermit,
}

/// Reservation that blocks new chat turns before an exclusive mutation starts.
#[derive(Debug)]
pub struct SessionMutationReservation {
    lock: Arc<SessionMutationLock>,
    gate: OwnedRwLockWriteGuard<()>,
}

/// Permit held while a session history mutation is in progress.
#[derive(Debug)]
pub struct SessionMutationPermit {
    _gate: OwnedRwLockWriteGuard<()>,
    _turn: OwnedSemaphorePermit,
}

/// Error returned when a chat turn cannot start immediately.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionBusyReason {
    /// Another chat turn owns the session.
    ActiveTurn,
    /// A maintenance mutation reserved the session and new chat turns must not queue.
    ReservedMutation,
}

impl std::fmt::Display for SessionBusyReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ActiveTurn => f.write_str("active turn"),
            Self::ReservedMutation => f.write_str("reserved mutation"),
        }
    }
}

#[derive(Debug, thiserror::Error)]
#[error("session is busy: {reason}")]
pub struct SessionBusyError {
    reason: SessionBusyReason,
}

impl SessionBusyError {
    #[must_use]
    pub fn reason(&self) -> SessionBusyReason {
        self.reason
    }

    fn active_turn() -> Self {
        Self {
            reason: SessionBusyReason::ActiveTurn,
        }
    }

    fn reserved_mutation() -> Self {
        Self {
            reason: SessionBusyReason::ReservedMutation,
        }
    }
}

impl SessionMutationCoordinator {
    /// Try to acquire exclusive turn access for a session.
    pub async fn try_acquire_turn(&self, key: &str) -> Result<SessionTurnPermit, SessionBusyError> {
        let lock = self.lock(key).await;
        let Ok(gate) = Arc::clone(&lock.acquisition_gate).try_read_owned() else {
            return Err(SessionBusyError::reserved_mutation());
        };
        let Ok(turn) = Arc::clone(&lock.turn).try_acquire_owned() else {
            return Err(SessionBusyError::active_turn());
        };
        drop(gate);

        Ok(SessionTurnPermit { _turn: turn })
    }

    /// Reserve a mutation slot, preventing new chat turns from starting.
    pub async fn reserve_mutation(&self, key: &str) -> SessionMutationReservation {
        let lock = self.lock(key).await;
        let gate = Arc::clone(&lock.acquisition_gate).write_owned().await;
        SessionMutationReservation { lock, gate }
    }

    async fn lock(&self, key: &str) -> Arc<SessionMutationLock> {
        {
            let locks = self.locks.read().await;
            if let Some(lock) = locks.get(key) {
                return Arc::clone(lock);
            }
        }

        let mut locks = self.locks.write().await;
        Arc::clone(
            locks
                .entry(key.to_string())
                .or_insert_with(|| Arc::new(SessionMutationLock::default())),
        )
    }
}

impl SessionMutationReservation {
    /// Wait until the active chat turn releases the session, then hold it for mutation.
    pub async fn acquire(self) -> Result<SessionMutationPermit, tokio::sync::AcquireError> {
        let turn = Arc::clone(&self.lock.turn).acquire_owned().await?;
        Ok(SessionMutationPermit {
            _gate: self.gate,
            _turn: turn,
        })
    }
}
