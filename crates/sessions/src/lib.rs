//! Session storage and management.
//!
//! Sessions are stored as JSONL files (one message per line) at
//! `<data_dir>/agents/<agentId>/sessions/<sessionKey>.jsonl`
//! with file locking for concurrent access.

pub mod backing;
pub mod error;
pub mod key;
pub mod message;
pub mod metadata;
pub mod prompt_queue;
mod provider_redaction;
pub mod session_events;
pub mod state_store;
pub mod store;
mod tail_cursor;
pub mod tool_results;
mod ui_history_database;
pub mod ui_history_engine;
mod ui_history_fork;
mod ui_history_migrations;
mod ui_history_projection;
mod ui_history_serialization;
pub mod ui_history_types;

pub use {
    error::{Error, Result},
    key::SessionKey,
    message::{ContentBlock, MessageContent, PersistedMessage, UserDocument},
    prompt_queue::{
        QueuedPrompt, QueuedPromptChannelMetadata, QueuedPromptContent, QueuedPromptContentBlock,
        QueuedPromptDocument, QueuedPromptImageUrl, QueuedPromptMessageContent, QueuedPrompts,
        QueuedPromptsDrain, QueuedPromptsStatus,
    },
    provider_redaction::redact_backend_only_provider_state,
    store::SearchResult,
    tool_results::{PersistedToolResult, ToolResultStore},
    ui_history_migrations::run_migrations as run_ui_history_migrations,
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
