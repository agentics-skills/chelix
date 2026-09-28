//! Process-wide owner lock for the session history database.

use std::{
    fs::{File, OpenOptions},
    path::Path,
};

use crate::{Error, Result};

/// Exclusive owner of `sessions/ui-history.owner.lock`.
///
/// The lock object is leaked so the write guard can be `'static`. Dropping
/// this value unlocks the file. The leaked box stays until the process exits.
pub struct ProcessOwnerLock {
    _guard: fd_lock::RwLockWriteGuard<'static, File>,
}

impl ProcessOwnerLock {
    /// Take the lock without waiting. Fails while another process holds it.
    pub fn try_acquire(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(path)?;
        let lock = Box::leak(Box::new(fd_lock::RwLock::new(file)));
        let guard = lock.try_write().map_err(|error| {
            Error::lock_failed(format!("session history owner lock is held: {error}"))
        })?;
        Ok(Self { _guard: guard })
    }
}
