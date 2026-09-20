//! Code-index crate for Chelix — workspace codebase intelligence.
//!
//! Supports two modes:
//! - **Builtin** (optional, `builtin` feature): SQLite + FTS5 with embeddings.
//! - **Config-only**: File discovery and filtering without search.

// Core modules (always available).
pub mod chunker;
pub mod config;
pub mod delta;
pub mod discover;
pub mod error;
pub mod filter;
pub mod index;
#[cfg(feature = "tracing")]
pub mod log;
pub mod snapshot_store;
pub mod store;
pub mod types;

// Optional backend, gated behind a feature flag.
#[cfg(feature = "builtin")]
pub mod store_sqlite;

#[cfg(feature = "file-watcher")]
pub mod watcher;

// Agent tools (only relevant with the builtin search backend).
#[cfg(feature = "builtin")]
pub mod tools;

// Re-exports for convenience.
pub use {
    config::CodeIndexConfig,
    delta::{FileMeta, HashSnapshot},
    error::{Error, Result},
    index::CodeIndex,
    types::{IndexStatus, SearchResult},
};
