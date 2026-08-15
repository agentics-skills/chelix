//! Session storage and management.
//!
//! Sessions are stored as JSONL files (one message per line) at
//! `<data_dir>/agents/<agentId>/sessions/<sessionKey>.jsonl`
//! with file locking for concurrent access.

pub mod error;
pub mod key;
pub mod message;
pub mod metadata;
pub mod prompt_queue;
pub mod session_events;
pub mod state_store;
pub mod store;
pub mod tool_results;
pub mod ui_history;

pub use {
    error::{Error, Result},
    key::SessionKey,
    message::{ContentBlock, MessageContent, PersistedMessage, UserDocument},
    prompt_queue::{QueuedPrompt, SessionPromptQueueStore},
    store::SearchResult,
    tool_results::{PersistedToolResult, ToolResultStore},
    ui_history::{filter_ui_history, redact_backend_only_provider_state},
};

/// Run database migrations for the sessions crate.
///
/// This creates the `sessions`, `channel_sessions`, `session_state`, and
/// `session_prompt_queue` tables. Should be called
/// at application startup after [`chelix_projects::run_migrations`] (sessions
/// has a foreign key to projects).
pub async fn run_migrations(pool: &sqlx::SqlitePool) -> Result<()> {
    sqlx::migrate!("./migrations")
        .set_ignore_missing(true)
        .run(pool)
        .await?;
    Ok(())
}
