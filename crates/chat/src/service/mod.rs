//! `LiveChatService` struct, constructors, and helper methods.

mod chat_impl;
mod types;

use types::QueuedMessage;
pub(crate) use types::{
    ActiveAssistantDraft, EventForwarderResult, append_assistant_delta,
    build_persisted_assistant_message, build_persisted_tool_call,
    build_tool_call_assistant_message, finalize_persisted_assistant_message,
    persist_active_assistant_draft, persist_final_assistant_segment, persist_tool_history_pair,
    reserve_assistant_message_index,
};
pub use types::{ActiveToolCall, LiveChatService};
