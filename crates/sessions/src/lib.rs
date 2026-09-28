//! Session storage and management.
//!
//! The canonical journal is `session_records` in `ui-history.sqlite`, next to
//! the UI history tables. Media files stay on disk under the sessions directory.

pub mod backing;
pub mod error;
mod journal;
pub mod key;
pub mod message;
pub mod metadata;
mod owner_lock;
pub mod prompt_queue;
mod provider_redaction;
pub mod session_events;
pub mod state_store;
pub mod store;
pub mod tool_results;
mod ui_history_database;

pub use owner_lock::ProcessOwnerLock;
pub mod ui_history_engine;
mod ui_history_fork;
mod ui_history_migrations;
mod ui_history_projection;
mod ui_history_serialization;
pub mod ui_history_types;

pub use {
    error::{Error, Result},
    journal::{ActiveEvent, JournalImportConflict, JournalPointers, TokenTotals},
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
