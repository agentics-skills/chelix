mod active_context;
mod agent_loop;
mod channels;
mod compaction;
mod compaction_reminder;
mod memory_tools;
mod message;
mod model_document;
mod models;
mod prompt;
mod prompt_queue;
mod run_with_tools;
mod service;
mod skill_tools;
mod stream_journal;
mod streaming;
mod tool_permission;
mod tools_config_source;
mod types;
mod ui_history_ingress;

#[cfg(test)]
pub(crate) static DATA_DIR_TEST_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

pub mod chat_error;
pub mod config_validation;
pub mod error;
pub mod runtime;

pub use tools_config_source::ToolsConfigSource;

pub use {
    memory_tools::{AgentScopedMemoryWriter, MemoryForgetTool},
    models::{DisabledModelsStore, LiveModelService},
    runtime::{ChatRuntime, TtsOverride},
    service::{ActiveToolInvocation, LiveChatService},
    types::{
        BroadcastOpts, memory_write_mode_allows_save, model_matches_allowlist, normalize_model_key,
    },
};
