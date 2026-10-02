//! Priority list of Context7 API tokens.

use std::sync::atomic::{AtomicUsize, Ordering};

use secrecy::Secret;

/// Tokens in config order. Index `0` is the highest priority.
pub(crate) struct Context7Tokens {
    tokens: Vec<Secret<String>>,
    index: AtomicUsize,
}

impl Context7Tokens {
    pub(crate) fn new(tokens: Vec<Secret<String>>) -> Self {
        Self {
            tokens,
            index: AtomicUsize::new(0),
        }
    }

    /// Highest-priority token and the number of configured tokens.
    pub(crate) fn current(&self) -> (Option<&Secret<String>>, usize) {
        let count = self.tokens.len();
        if count == 0 {
            return (None, 0);
        }
        let index = self.index.load(Ordering::Relaxed) % count;
        (self.tokens.get(index), count)
    }

    /// Move priority to the next token and return it.
    pub(crate) fn rotate(&self) -> Option<&Secret<String>> {
        let count = self.tokens.len();
        if count == 0 {
            return None;
        }
        let next = self.index.load(Ordering::Relaxed).wrapping_add(1) % count;
        self.index.store(next, Ordering::Relaxed);
        self.tokens.get(next)
    }
}
