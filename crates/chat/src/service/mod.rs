//! `LiveChatService` struct, constructors, and helper methods.

mod chat_impl;
mod types;

use types::QueuedMessage;
pub(crate) use types::{
    ActiveAssistantDraft, EventForwarderResult, append_final_assistant_segment,
    build_persisted_assistant_message, build_persisted_tool_call,
    build_tool_call_assistant_message, finalize_persisted_assistant_message,
    persist_tool_history_pair,
};
pub use types::{ActiveToolCall, LiveChatService};
