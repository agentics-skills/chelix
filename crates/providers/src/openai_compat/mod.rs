//! Shared helpers for OpenAI-compatible streaming with tools.
//!
//! This module provides reusable functions for parsing OpenAI-style SSE streams
//! that include tool calls. Used by the OpenAI-compatible transports.

mod provider;
mod schema_normalization;

#[cfg(test)]
mod tests;

pub use provider::{
    ChatCompletionsFunction, ChatCompletionsTool, ResponsesApiTool, ResponsesEventResult,
    ResponsesStreamState, SseLineResult, StreamingToolState, finalize_responses_stream,
    finalize_stream, parse_openai_compat_usage, parse_openai_compat_usage_from_payload,
    process_openai_sse_line, process_responses_event, process_responses_sse_line,
    split_responses_instructions_and_input, to_openai_tools, to_responses_api_tools,
    to_responses_input,
};
