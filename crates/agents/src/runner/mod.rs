//! Agent runner: LLM call loop with tool execution, retry, and streaming support.

mod helpers;
mod non_streaming;
pub mod retry;
mod streaming;
pub mod tool_result;

#[cfg(test)]
mod tests;

// ── Re-exports (preserve public API) ────────────────────────────────────

pub use {
    helpers::{
        AgentLoopLimits, AgentRunError, AgentRunResult, ContextCompactionRequest, FinalTextSource,
        OnEvent, RunnerEvent, RunnerToolCall,
    },
    non_streaming::{
        run_agent, run_agent_loop, run_agent_loop_with_context,
        run_agent_loop_with_context_and_limits,
    },
    streaming::{run_agent_loop_streaming, run_agent_loop_streaming_with_limits},
    tool_result::{persist_and_truncate, sanitize_tool_result},
};

/// Shared inbox for mid-flight steering text (populated by `/steer` command).
///
/// The agent loop drains this between iterations and injects the text as a
/// system notice so the LLM sees the guidance on its next call.
pub type SteerInbox = std::sync::Arc<tokio::sync::Mutex<Vec<String>>>;

// Re-export helpers at the module level so that sibling submodules
// (`non_streaming`, `streaming`) can continue to import via `super::item_name`.
pub(crate) use helpers::{
    AUTO_CONTINUE_NUDGE, MALFORMED_TOOL_RETRY_PROMPT, UsageAccumulator,
    apply_before_llm_call_modify_payload, apply_loop_detector_intervention,
    channel_binding_from_tool_context, dispatch_after_llm_call_hook,
    dispatch_before_agent_start_hook, empty_tool_name_retry_prompt, evaluate_context_budget,
    explicit_shell_command_from_user_content, fallback_final_text_source,
    find_empty_tool_name_call, finish_agent_run, has_named_tool_call, is_substantive_answer_text,
    log_tool_argument_diagnostic, public_tool_arguments, record_answer_text, resolve_tool_lookup,
    sanitize_tool_name, should_trigger_automatic_checkpoint, split_context_for_compaction,
    streaming_tool_call_message_content,
};

// Items only consumed by runner tests.
#[cfg(test)]
pub(crate) use helpers::{AUTO_COMPACTION_RATIO, estimate_prompt_tokens, legacy_public_tool_alias};
