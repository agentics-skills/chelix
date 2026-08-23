use std::{
    fs::File,
    io::{BufRead, BufReader, Seek},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::{Instant, SystemTime},
};

#[cfg(unix)]
use std::os::unix::fs::MetadataExt;
#[cfg(windows)]
use std::os::windows::fs::MetadataExt;

use crate::{Error, Result};

const REGISTRY_CAPACITY: usize = 1_000_000;
const REGISTRY_IDLE_RETENTION: std::time::Duration = time::Duration::days(36_525).unsigned_abs();

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SessionFileStamp {
    len: u64,
    modified: SystemTime,
    identity: Option<SessionFileIdentity>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SessionFileIdentity {
    device: u64,
    inode: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SessionTailCursor {
    pub(crate) next_index: usize,
    pub(crate) file_stamp: SessionFileStamp,
}

#[derive(Debug, Default)]
pub(crate) struct SessionTailState {
    pub(crate) cursor: Option<SessionTailCursor>,
}

struct RegistryEntry {
    state: Arc<Mutex<SessionTailState>>,
    last_accessed: Instant,
}

pub(crate) struct SessionTailRegistry {
    state: Mutex<lru::LruCache<PathBuf, RegistryEntry>>,
    capacity: usize,
    idle_retention: std::time::Duration,
    #[cfg(test)]
    full_scans: std::sync::atomic::AtomicUsize,
    #[cfg(test)]
    scanned_bytes: std::sync::atomic::AtomicUsize,
    #[cfg(test)]
    fail_next_write: std::sync::atomic::AtomicBool,
    #[cfg(test)]
    fail_next_file_lock: std::sync::atomic::AtomicBool,
    #[cfg(test)]
    fail_next_metadata: std::sync::atomic::AtomicBool,
}

impl SessionFileStamp {
    pub(crate) fn read(file: &File) -> Result<Self> {
        let metadata = file.metadata()?;
        Ok(Self {
            len: metadata.len(),
            modified: metadata.modified()?,
            identity: file_identity(&metadata),
        })
    }
}

impl SessionTailState {
    pub(crate) fn invalidate(&mut self) {
        self.cursor = None;
    }

    pub(crate) fn set(&mut self, next_index: usize, file_stamp: SessionFileStamp) {
        self.cursor = Some(SessionTailCursor {
            next_index,
            file_stamp,
        });
    }
}

impl SessionTailRegistry {
    pub(crate) fn new() -> Self {
        Self {
            state: Mutex::new(lru::LruCache::unbounded()),
            capacity: REGISTRY_CAPACITY,
            idle_retention: REGISTRY_IDLE_RETENTION,
            #[cfg(test)]
            full_scans: std::sync::atomic::AtomicUsize::new(0),
            #[cfg(test)]
            scanned_bytes: std::sync::atomic::AtomicUsize::new(0),
            #[cfg(test)]
            fail_next_write: std::sync::atomic::AtomicBool::new(false),
            #[cfg(test)]
            fail_next_file_lock: std::sync::atomic::AtomicBool::new(false),
            #[cfg(test)]
            fail_next_metadata: std::sync::atomic::AtomicBool::new(false),
        }
    }

    #[cfg(test)]
    fn with_limits(capacity: usize, idle_retention: std::time::Duration) -> Self {
        assert!(capacity > 0);
        Self {
            state: Mutex::new(lru::LruCache::unbounded()),
            capacity,
            idle_retention,
            full_scans: std::sync::atomic::AtomicUsize::new(0),
            scanned_bytes: std::sync::atomic::AtomicUsize::new(0),
            fail_next_write: std::sync::atomic::AtomicBool::new(false),
            fail_next_file_lock: std::sync::atomic::AtomicBool::new(false),
            fail_next_metadata: std::sync::atomic::AtomicBool::new(false),
        }
    }

    pub(crate) fn session_state(&self, path: &Path) -> Result<Arc<Mutex<SessionTailState>>> {
        let now = Instant::now();
        let mut registry = self
            .state
            .lock()
            .map_err(|error| Error::lock_failed(error.to_string()))?;
        while registry
            .peek_lru()
            .is_some_and(|(_, entry)| now.duration_since(entry.last_accessed) > self.idle_retention)
        {
            registry.pop_lru();
        }

        if let Some(entry) = registry.get_mut(path) {
            entry.last_accessed = now;
            return Ok(Arc::clone(&entry.state));
        }

        if registry.len() >= self.capacity {
            registry.pop_lru().ok_or_else(|| {
                Error::message("session tail registry has no LRU entry at capacity")
            })?;
        }

        let state = Arc::new(Mutex::new(SessionTailState::default()));
        registry.put(path.to_path_buf(), RegistryEntry {
            state: Arc::clone(&state),
            last_accessed: now,
        });
        Ok(state)
    }

    pub(crate) fn remove(&self, path: &Path, state: &Arc<Mutex<SessionTailState>>) -> Result<()> {
        let mut registry = self
            .state
            .lock()
            .map_err(|error| Error::lock_failed(error.to_string()))?;
        if registry
            .peek(path)
            .is_some_and(|entry| Arc::ptr_eq(&entry.state, state))
        {
            registry.pop(path);
        }
        Ok(())
    }

    pub(crate) fn record_scan(&self, bytes_read: usize) {
        #[cfg(test)]
        {
            use std::sync::atomic::Ordering;

            self.full_scans.fetch_add(1, Ordering::Relaxed);
            self.scanned_bytes.fetch_add(bytes_read, Ordering::Relaxed);
        }
        #[cfg(not(test))]
        let _ = bytes_read;
    }

    #[cfg(test)]
    pub(crate) fn scan_metrics(&self) -> (usize, usize) {
        use std::sync::atomic::Ordering;

        (
            self.full_scans.load(Ordering::Relaxed),
            self.scanned_bytes.load(Ordering::Relaxed),
        )
    }

    #[cfg(test)]
    pub(crate) fn fail_next_write(&self) {
        use std::sync::atomic::Ordering;

        self.fail_next_write.store(true, Ordering::Relaxed);
    }

    #[cfg(test)]
    pub(crate) fn take_write_failure(&self) -> bool {
        use std::sync::atomic::Ordering;

        self.fail_next_write.swap(false, Ordering::Relaxed)
    }

    #[cfg(test)]
    pub(crate) fn fail_next_file_lock(&self) {
        use std::sync::atomic::Ordering;

        self.fail_next_file_lock.store(true, Ordering::Relaxed);
    }

    #[cfg(test)]
    pub(crate) fn take_file_lock_failure(&self) -> bool {
        use std::sync::atomic::Ordering;

        self.fail_next_file_lock.swap(false, Ordering::Relaxed)
    }

    #[cfg(test)]
    pub(crate) fn fail_next_metadata(&self) {
        use std::sync::atomic::Ordering;

        self.fail_next_metadata.store(true, Ordering::Relaxed);
    }

    #[cfg(test)]
    pub(crate) fn take_metadata_failure(&self) -> bool {
        use std::sync::atomic::Ordering;

        self.fail_next_metadata.swap(false, Ordering::Relaxed)
    }

    #[cfg(test)]
    pub(crate) fn contains(&self, path: &Path) -> bool {
        self.state
            .lock()
            .map(|registry| registry.contains(path))
            .unwrap_or(false)
    }
}

pub(crate) fn scan_tail(file: &mut File, registry: &SessionTailRegistry) -> Result<usize> {
    file.rewind()?;
    let mut reader = BufReader::new(file);
    let mut line = String::new();
    let mut next_index = 0_usize;
    let mut bytes_read = 0_usize;

    loop {
        line.clear();
        let line_bytes = reader.read_line(&mut line)?;
        if line_bytes == 0 {
            break;
        }
        bytes_read = bytes_read
            .checked_add(line_bytes)
            .ok_or_else(|| Error::message("session tail scan byte counter overflow"))?;
        if !line.ends_with('\n') {
            registry.record_scan(bytes_read);
            return Err(Error::message(
                "session JSONL ends with an incomplete record",
            ));
        }
        if !line.trim().is_empty() {
            next_index = next_index
                .checked_add(1)
                .ok_or_else(|| Error::message("session message index overflow"))?;
        }
    }

    registry.record_scan(bytes_read);
    Ok(next_index)
}

#[cfg(unix)]
fn file_identity(metadata: &std::fs::Metadata) -> Option<SessionFileIdentity> {
    Some(SessionFileIdentity {
        device: metadata.dev(),
        inode: metadata.ino(),
    })
}

#[cfg(not(unix))]
#[cfg(not(windows))]
fn file_identity(_metadata: &std::fs::Metadata) -> Option<SessionFileIdentity> {
    None
}

#[cfg(windows)]
fn file_identity(metadata: &std::fs::Metadata) -> Option<SessionFileIdentity> {
    Some(SessionFileIdentity {
        device: u64::from(metadata.volume_serial_number()?),
        inode: metadata.file_index()?,
    })
}

#[allow(clippy::unwrap_used, clippy::expect_used)]
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_evicts_the_least_recently_used_session_at_capacity() {
        let registry = SessionTailRegistry::with_limits(2, REGISTRY_IDLE_RETENTION);
        let first = PathBuf::from("first.jsonl");
        let second = PathBuf::from("second.jsonl");
        let third = PathBuf::from("third.jsonl");

        registry.session_state(&first).unwrap();
        registry.session_state(&second).unwrap();
        registry.session_state(&first).unwrap();
        registry.session_state(&third).unwrap();

        assert!(registry.contains(&first));
        assert!(!registry.contains(&second));
        assert!(registry.contains(&third));
    }
}
