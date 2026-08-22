//! `LiveChatService` struct, constructors, and helper methods.

mod chat_impl;
mod types;

pub(crate) use types::{
    ActiveAssistantDraft, EventForwarderResult, build_persisted_tool_call,
    finalize_aborted_tool_segment, finalize_persisted_assistant_message, latest_tool_segment_index,
    persist_active_assistant_draft, persist_final_assistant_segment,
};
pub use {chelix_common::ActiveToolInvocation, types::LiveChatService};
