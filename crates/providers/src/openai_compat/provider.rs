//! Shared helpers for OpenAI-compatible streaming with tools.
//!
//! This module provides reusable functions for parsing OpenAI-style SSE streams
//! that include tool calls. Used by the OpenAI-compatible transports.

use std::collections::{HashMap, HashSet};

use {
    anyhow::{Context, Result},
    chelix_agents::model::{
        ChatMessage, CompletionResponse, StreamEvent, ToolCall, Usage, UserContent,
        decode_tool_call_arguments_with_diagnostic,
    },
    chelix_common::{
        ItemPositionAllocator, ProviderItemId, ProviderItemPosition, ProviderItemUpdate,
        ProviderItemUpdatePayload, ProviderSegmentId, ProviderSegmentOutcome,
    },
    serde::Serialize,
    tracing::trace,
};

use super::schema_normalization::{normalize_tool_parameters, tool_identity};

/// Identity of the reasoning item synthesized for Chat Completions streams.
///
/// Chat Completions has no output items, so the adapter assigns this identity
/// once per segment. All reasoning text of the segment belongs to it.
const CHAT_REASONING_ITEM_ID: &str = "rs_0";

/// Identity of the visible message item synthesized for Chat Completions streams.
const CHAT_MESSAGE_ITEM_ID: &str = "msg_0";

// ============================================================================
// OpenAI Tool Schema Types
// ============================================================================
// These types enforce the correct structure for OpenAI-compatible APIs.
// Using typed structs instead of manual JSON prevents missing fields at compile time.
//
// References:
// - Chat Completions: https://platform.openai.com/docs/guides/function-calling
// - Responses API: https://learn.microsoft.com/en-us/azure/ai-foundry/openai/how-to/responses
// ============================================================================

/// Chat Completions API tool format (nested under "function").
///
/// ```json
/// { "type": "function", "function": { "name": "...", ... } }
/// ```
#[derive(Debug, Serialize)]
pub struct ChatCompletionsTool {
    #[serde(rename = "type")]
    pub tool_type: &'static str,
    pub function: ChatCompletionsFunction,
}

/// The function definition nested inside ChatCompletionsTool.
#[derive(Debug, Serialize)]
pub struct ChatCompletionsFunction {
    pub name: String,
    pub description: String,
    pub parameters: serde_json::Value,
    pub strict: bool,
}

/// Responses API tool format (flat, name at top level).
///
/// ```json
/// { "type": "function", "name": "...", "parameters": {...}, "strict": true }
/// ```
#[derive(Debug, Serialize)]
pub struct ResponsesApiTool {
    #[serde(rename = "type")]
    pub tool_type: &'static str,
    pub name: String,
    pub description: String,
    pub parameters: serde_json::Value,
    pub strict: bool,
}

/// Convert tool schemas to OpenAI Chat Completions function-calling format.
///
/// Uses the nested `function` object format required by Chat Completions API:
/// ```json
/// { "type": "function", "function": { "name": "...", ... } }
/// ```
///
/// Tool schemas are always sent with `strict: false`. Their declared
/// `required` fields remain authoritative, so optional properties are never
/// rewritten into required nullable properties.
///
/// Fails when any tool's schema cannot be expressed in the OpenAI dialect. A
/// tool the model can see but not call correctly is worse than a refused
/// request, so the error is propagated instead of dropping the tool.
///
/// See: <https://platform.openai.com/docs/guides/function-calling>
pub fn to_openai_tools(tools: &[serde_json::Value]) -> Result<Vec<serde_json::Value>> {
    let result = tools
        .iter()
        .map(|tool| {
            let (name, description) = tool_identity(tool)?;
            let mut parameters = tool["parameters"].clone();
            normalize_tool_parameters(&mut parameters)
                .with_context(|| format!("tool `{name}` has an unusable parameter schema"))?;

            trace!(tool_name = %name, "converted tool to Chat Completions format");

            Ok(serde_json::to_value(ChatCompletionsTool {
                tool_type: "function",
                function: ChatCompletionsFunction {
                    name,
                    description,
                    parameters,
                    strict: false,
                },
            })?)
        })
        .collect::<Result<Vec<serde_json::Value>>>()?;

    trace!(tools_count = result.len(), "to_openai_tools complete");
    Ok(result)
}

/// Convert tool schemas to OpenAI Responses API function-calling format.
///
/// Uses the flat format required by the Responses API where `name` is at top level:
/// ```json
/// { "type": "function", "name": "...", "parameters": {...}, "strict": false }
/// ```
///
/// This is the format used by the Responses API.
///
/// Preserves the tool's declared `required` fields so optional properties
/// remain optional on the wire.
///
/// Fails on an unusable schema for the same reason as [`to_openai_tools`].
///
/// See: <https://learn.microsoft.com/en-us/azure/ai-foundry/openai/how-to/responses>
pub fn to_responses_api_tools(tools: &[serde_json::Value]) -> Result<Vec<serde_json::Value>> {
    let result = tools
        .iter()
        .map(|tool| {
            let (name, description) = tool_identity(tool)?;
            // Keep the tool's required/optional contract intact.
            let mut parameters = tool["parameters"].clone();
            normalize_tool_parameters(&mut parameters)
                .with_context(|| format!("tool `{name}` has an unusable parameter schema"))?;

            trace!(tool_name = %name, "converted tool to Responses API format");

            Ok(serde_json::to_value(ResponsesApiTool {
                tool_type: "function",
                name,
                description,
                parameters,
                strict: false,
            })?)
        })
        .collect::<Result<Vec<serde_json::Value>>>()?;

    trace!(
        tools_count = result.len(),
        "to_responses_api_tools complete"
    );
    Ok(result)
}

/// Convert typed chat messages to Responses API input items.
///
/// Responses API accepts a heterogeneous input array (messages, tool calls, and
/// tool outputs). This keeps one canonical conversion for providers that use
/// Responses transport (SSE or WebSocket).
#[must_use]
pub fn to_responses_input(messages: &[ChatMessage]) -> Vec<serde_json::Value> {
    messages
        .iter()
        .flat_map(|msg| match msg {
            ChatMessage::System { .. } => {
                // System messages are extracted into `instructions`.
                vec![]
            },
            ChatMessage::User { content, .. } => {
                let content_blocks = match content {
                    UserContent::Text(t) => {
                        vec![serde_json::json!({"type": "input_text", "text": t})]
                    },
                    UserContent::Multimodal(parts) => parts
                        .iter()
                        .map(|p| match p {
                            chelix_agents::model::ContentPart::Text(t) => {
                                serde_json::json!({"type": "input_text", "text": t})
                            },
                            chelix_agents::model::ContentPart::Image { media_type, data } => {
                                let data_uri = format!("data:{media_type};base64,{data}");
                                serde_json::json!({
                                    "type": "input_image",
                                    "image_url": data_uri,
                                })
                            },
                        })
                        .collect(),
                };
                vec![serde_json::json!({
                    "role": "user",
                    "content": content_blocks,
                })]
            },
            ChatMessage::Assistant {
                content,
                tool_calls,
                provider_items,
                ..
            } => {
                if !provider_items.is_empty() {
                    provider_items
                        .iter()
                        .map(|item| match &item.payload {
                            chelix_common::ProviderOutputPayload::Reasoning(reasoning) => {
                                serde_json::json!({
                                    "type": "reasoning",
                                    "id": reasoning.id.0,
                                    "summary": reasoning_summary(reasoning),
                                    "encrypted_content": reasoning.encrypted_content,
                                })
                            },
                            chelix_common::ProviderOutputPayload::Message { text } => {
                                serde_json::json!({
                                    "type": "message",
                                    "role": "assistant",
                                    "content": [{"type": "output_text", "text": text}]
                                })
                            },
                            chelix_common::ProviderOutputPayload::FunctionCall {
                                call_id,
                                name,
                                arguments,
                            } => {
                                serde_json::json!({
                                    "type": "function_call",
                                    "call_id": call_id,
                                    "name": name,
                                    "arguments": arguments,
                                })
                            },
                        })
                        .collect()
                } else {
                    let mut items = Vec::new();
                    if let Some(text) = content
                        && (!text.is_empty() || tool_calls.is_empty())
                    {
                        items.push(serde_json::json!({
                            "type": "message",
                            "role": "assistant",
                            "content": [{"type": "output_text", "text": text}]
                        }));
                    }

                    items.extend(tool_calls.iter().map(|tc| {
                        serde_json::json!({
                            "type": "function_call",
                            "call_id": tc.id,
                            "name": tc.name,
                            "arguments": tc.arguments.to_string(),
                        })
                    }));
                    items
                }
            },
            ChatMessage::Tool {
                tool_call_id,
                content,
            } => {
                vec![serde_json::json!({
                    "type": "function_call_output",
                    "call_id": tool_call_id,
                    "output": content,
                })]
            },
        })
        .collect()
}

/// Updates for the summary parts carried by a final reasoning item.
///
/// `output_item.done` repeats the whole summary of the item, including parts
/// that already arrived as deltas. Re-emitting those would append the same text
/// twice, so only the parts the stream never delivered are emitted here. A
/// provider that sends the summary only in the final item therefore keeps its
/// reasoning, and one that streams it keeps it exactly once.
fn reasoning_summary_events(
    item: &serde_json::Value,
    item_id: &ProviderItemId,
    position: ProviderItemPosition,
    state: &mut ResponsesStreamState,
) -> Vec<StreamEvent> {
    let Some(summary) = item["summary"].as_array() else {
        return Vec::new();
    };
    let mut events = Vec::new();
    for (summary_index, part) in summary.iter().enumerate() {
        let Some(text) = part["text"].as_str().filter(|text| !text.is_empty()) else {
            continue;
        };
        if !state
            .streamed_reasoning_summary_parts
            .insert((item_id.0.clone(), summary_index))
        {
            continue;
        }
        let seq = state.next_seq();
        events.push(StreamEvent::ProviderItemUpdate(ProviderItemUpdate {
            segment_id: state.segment_id.clone(),
            item_id: item_id.clone(),
            position,
            update_seq: seq,
            payload: ProviderItemUpdatePayload::ReasoningPartDone {
                part_index: summary_index,
                text: text.to_string(),
            },
        }));
    }
    events
}

/// Serialize the summary of a reasoning item for a Responses API request.
///
/// The parts a segment collected are what the model produced, so they are sent
/// back verbatim and in their canonical order. An item that carries no parts
/// serializes as an empty summary, which is what the API expects for reasoning
/// that only has opaque content.
fn reasoning_summary(reasoning: &chelix_common::ReasoningItem) -> Vec<serde_json::Value> {
    reasoning
        .summary_parts
        .iter()
        .map(|part| {
            serde_json::json!({
                "type": "summary_text",
                "text": part.text,
            })
        })
        .collect()
}

/// Parse tool_calls from an OpenAI response message (non-streaming).
pub fn parse_tool_calls(message: &serde_json::Value) -> Vec<ToolCall> {
    message["tool_calls"]
        .as_array()
        .map(|tcs| {
            tcs.iter()
                .filter_map(|tc| {
                    let id = tc["id"].as_str()?.to_string();
                    let name = tc["function"]["name"].as_str()?.to_string();
                    let decoded =
                        decode_tool_call_arguments_with_diagnostic(tc["function"].get("arguments"));
                    Some(ToolCall {
                        id,
                        name,
                        arguments: decoded.arguments,
                        argument_diagnostic: decoded.diagnostic,
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

fn is_null_sentinel(value: &serde_json::Value) -> bool {
    value.as_str().is_some_and(|raw| {
        let normalized = raw.trim().to_ascii_lowercase();
        matches!(normalized.as_str(), "none" | "null")
    })
}

fn schema_type_includes_null(schema: &serde_json::Value) -> bool {
    match schema.get("type") {
        Some(serde_json::Value::String(kind)) => kind == "null",
        Some(serde_json::Value::Array(kinds)) => {
            kinds.iter().any(|kind| kind.as_str() == Some("null"))
        },
        _ => false,
    }
}

fn schema_type_is_only_null(schema: &serde_json::Value) -> bool {
    match schema.get("type") {
        Some(serde_json::Value::String(kind)) => kind == "null",
        Some(serde_json::Value::Array(kinds)) => {
            !kinds.is_empty() && kinds.iter().all(|kind| kind.as_str() == Some("null"))
        },
        _ => false,
    }
}

fn schema_type_includes_array(schema: &serde_json::Value) -> bool {
    match schema.get("type") {
        Some(serde_json::Value::String(kind)) => kind == "array",
        Some(serde_json::Value::Array(kinds)) => {
            kinds.iter().any(|kind| kind.as_str() == Some("array"))
        },
        _ => false,
    }
}

fn schema_enum_includes(schema: &serde_json::Value, expected: &serde_json::Value) -> bool {
    schema
        .get("enum")
        .and_then(serde_json::Value::as_array)
        .is_some_and(|values| values.iter().any(|value| value == expected))
}

fn schema_allows_null(
    schema: &serde_json::Value,
    parent_required: Option<&[serde_json::Value]>,
    property_name: Option<&str>,
) -> bool {
    if schema_type_includes_null(schema) || schema_enum_includes(schema, &serde_json::Value::Null) {
        return true;
    }

    let is_optional = property_name.is_some_and(|name| {
        !parent_required.is_some_and(|required| {
            required
                .iter()
                .any(|entry| entry.as_str().is_some_and(|entry| entry == name))
        })
    });

    // Strict-mode providers make originally-optional enum fields nullable.
    // Some compatible backends return host-language sentinels like "None"
    // instead of JSON null; only coerce when the schema does not explicitly
    // allow that sentinel as an enum value.
    is_optional
        && schema
            .get("enum")
            .and_then(serde_json::Value::as_array)
            .is_some()
        && !schema_enum_includes(schema, &serde_json::Value::String("None".to_string()))
        && !schema_enum_includes(schema, &serde_json::Value::String("null".to_string()))
}

fn normalize_argument_value(
    value: &mut serde_json::Value,
    schema: &serde_json::Value,
    parent_required: Option<&[serde_json::Value]>,
    property_name: Option<&str>,
) {
    if value.as_str() == Some("") && schema_type_is_only_null(schema) {
        *value = serde_json::Value::Null;
        return;
    }

    if is_null_sentinel(value) && schema_allows_null(schema, parent_required, property_name) {
        *value = serde_json::Value::Null;
        return;
    }

    if schema_type_includes_array(schema)
        && let Some(raw) = value.as_str()
        && raw.trim_start().starts_with('[')
        && let Ok(parsed @ serde_json::Value::Array(_)) = serde_json::from_str(raw)
    {
        *value = parsed;
    }

    match value {
        serde_json::Value::Object(args) => {
            let required = schema
                .get("required")
                .and_then(serde_json::Value::as_array)
                .map(Vec::as_slice);
            let Some(properties) = schema
                .get("properties")
                .and_then(serde_json::Value::as_object)
            else {
                return;
            };

            for (name, nested_value) in args {
                if let Some(nested_schema) = properties.get(name) {
                    normalize_argument_value(nested_value, nested_schema, required, Some(name));
                }
            }
        },
        serde_json::Value::Array(values) => {
            if let Some(items) = schema.get("items") {
                for nested_value in values {
                    normalize_argument_value(nested_value, items, None, None);
                }
            }
        },
        _ => {},
    }
}

/// Normalize provider-specific null sentinels in tool-call arguments.
///
/// Some OpenAI-compatible providers translate JSON Schema nullability through a
/// host-language sentinel and return strings like `"None"` for nullable enum
/// fields. The schema still tells us those fields were intended to be JSON null,
/// so repair them at the provider boundary before tool execution.
pub fn normalize_tool_call_arguments_from_schemas(
    tool_calls: &mut [ToolCall],
    tools: &[serde_json::Value],
) {
    for tool_call in tool_calls {
        let Some(tool) = tools.iter().find(|tool| {
            tool.get("name")
                .and_then(serde_json::Value::as_str)
                .is_some_and(|name| name == tool_call.name)
        }) else {
            continue;
        };
        let Some(parameters) = tool.get("parameters") else {
            continue;
        };
        normalize_argument_value(&mut tool_call.arguments, parameters, None, None);
    }
}

fn usage_value_at_path(usage: &serde_json::Value, path: &[&str]) -> Option<u64> {
    let mut cursor = usage;
    for key in path {
        cursor = cursor.get(*key)?;
    }
    cursor
        .as_u64()
        .or_else(|| cursor.as_str().and_then(|raw| raw.parse::<u64>().ok()))
}

fn usage_field_u32(usage: &serde_json::Value, paths: &[&[&str]]) -> u32 {
    paths
        .iter()
        .find_map(|path| usage_value_at_path(usage, path))
        .unwrap_or(0) as u32
}

fn usage_object_from_payload(payload: &serde_json::Value) -> Option<&serde_json::Value> {
    if let Some(usage) = payload.get("usage").filter(|usage| usage.is_object()) {
        return Some(usage);
    }

    if let Some(usage) = payload
        .get("choices")
        .and_then(serde_json::Value::as_array)
        .and_then(|choices| choices.first())
        .and_then(|choice| choice.get("usage"))
        .filter(|usage| usage.is_object())
    {
        return Some(usage);
    }

    if let Some(usage) = payload
        .get("choices")
        .and_then(serde_json::Value::as_array)
        .and_then(|choices| choices.first())
        .and_then(|choice| choice.get("delta"))
        .and_then(|delta| delta.get("usage"))
        .filter(|usage| usage.is_object())
    {
        return Some(usage);
    }

    if let Some(usage) = payload
        .get("choices")
        .and_then(serde_json::Value::as_array)
        .and_then(|choices| choices.first())
        .and_then(|choice| choice.get("message"))
        .and_then(|message| message.get("usage"))
        .filter(|usage| usage.is_object())
    {
        return Some(usage);
    }

    payload
        .get("x_groq")
        .and_then(|x_groq| x_groq.get("usage"))
        .filter(|usage| usage.is_object())
}

/// Parse usage payloads from OpenAI-compatible backends.
///
/// Different providers use different field names:
/// - OpenAI-style: `prompt_tokens`, `completion_tokens`
/// - Anthropic/MiniMax-style: `input_tokens`, `output_tokens`
/// - Cache fields may be top-level or nested in `*_tokens_details`.
#[must_use]
pub fn parse_openai_compat_usage(usage: &serde_json::Value) -> Usage {
    Usage {
        input_tokens: usage_field_u32(usage, &[
            &["prompt_tokens"],
            &["promptTokens"],
            &["input_tokens"],
            &["inputTokens"],
        ]),
        output_tokens: usage_field_u32(usage, &[
            &["completion_tokens"],
            &["completionTokens"],
            &["output_tokens"],
            &["outputTokens"],
        ]),
        cache_read_tokens: usage_field_u32(usage, &[
            &["prompt_tokens_details", "cached_tokens"],
            &["promptTokensDetails", "cachedTokens"],
            &["input_tokens_details", "cached_tokens"],
            &["inputTokensDetails", "cachedTokens"],
            &["cache_read_input_tokens"],
            &["cacheReadInputTokens"],
            &["input_tokens_details", "cache_read_input_tokens"],
            &["inputTokensDetails", "cacheReadInputTokens"],
        ]),
        cache_write_tokens: usage_field_u32(usage, &[
            &["cache_creation_input_tokens"],
            &["cacheCreationInputTokens"],
            &["input_tokens_details", "cache_creation_input_tokens"],
            &["inputTokensDetails", "cacheCreationInputTokens"],
        ]),
    }
}

/// Parse usage from an OpenAI-compatible payload, checking common nesting variants.
///
/// Providers differ on where they place usage metadata:
/// - top-level `usage`
/// - `choices[0].usage`
/// - `choices[0].delta.usage`
/// - `choices[0].message.usage`
/// - provider extension blocks (for example `x_groq.usage`)
#[must_use]
pub fn parse_openai_compat_usage_from_payload(payload: &serde_json::Value) -> Option<Usage> {
    usage_object_from_payload(payload).map(parse_openai_compat_usage)
}

/// Strip reasoning tags from content, returning `(visible, thinking)`.
///
/// Models like DeepSeek R1, QwQ, and MiniMax embed chain-of-thought reasoning
/// inside tags like `<think>` or `<thought>` in the `content` field rather than using a separate
/// `reasoning_content` field.  This helper splits content into the visible
/// answer text and the thinking text so callers can handle them appropriately.
///
/// Edge cases handled:
/// - Multiple reasoning blocks interspersed with answer text
/// - Unclosed reasoning tag (remainder treated as reasoning)
/// - Empty reasoning blocks
/// - Nested angle brackets inside thinking text
pub fn strip_think_tags(content: &str) -> (String, String) {
    let mut visible = String::new();
    let mut thinking = String::new();
    let mut remaining = content;

    loop {
        match find_next_open_tag(remaining) {
            Some((start, tag)) => {
                // Text before <think> is visible
                visible.push_str(&remaining[..start]);
                let after_open = &remaining[start + tag.open().len()..];
                match after_open.find(tag.close()) {
                    Some(end) => {
                        thinking.push_str(&after_open[..end]);
                        remaining = &after_open[end + tag.close().len()..];
                    },
                    None => {
                        // Unclosed reasoning tag — treat rest as reasoning
                        thinking.push_str(after_open);
                        break;
                    },
                }
            },
            None => {
                visible.push_str(remaining);
                break;
            },
        }
    }

    (
        visible.trim_start().to_string(),
        thinking.trim_start().to_string(),
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ReasoningTag {
    Think,
    Thought,
}

impl ReasoningTag {
    const ALL: [Self; 2] = [Self::Think, Self::Thought];

    fn open(self) -> &'static str {
        match self {
            Self::Think => "<think>",
            Self::Thought => "<thought>",
        }
    }

    fn close(self) -> &'static str {
        match self {
            Self::Think => "</think>",
            Self::Thought => "</thought>",
        }
    }
}

fn find_next_open_tag(text: &str) -> Option<(usize, ReasoningTag)> {
    ReasoningTag::ALL
        .into_iter()
        .filter_map(|tag| text.find(tag.open()).map(|pos| (pos, tag)))
        .min_by_key(|(pos, _)| *pos)
}

fn longest_reasoning_open_tag_suffix(text: &str) -> usize {
    ReasoningTag::ALL
        .into_iter()
        .map(|tag| longest_tag_suffix(text, tag.open()))
        .max()
        .unwrap_or_default()
}

/// State for tracking streaming tool calls.
#[derive(Debug)]
pub struct StreamingToolState {
    pub segment_id: ProviderSegmentId,
    pub segment_started: bool,
    /// Set once the segment has been closed by a terminal outcome.
    pub segment_closed: bool,
    /// Set once the provider announced a terminal outcome for this response.
    ///
    /// Chat Completions marks the end of a response with a `finish_reason` or a
    /// `[DONE]` frame. Without one of them the connection simply dropped, and
    /// the segment must not be reported as completed.
    terminal_seen: bool,
    pub next_seq: u64,
    /// Map from index -> (id, name, arguments_buffer)
    pub tool_calls: HashMap<usize, (String, String, String)>,
    /// Canonical positions for this segment, assigned in first-appearance order.
    ///
    /// Chat Completions carries no output item index, so the adapter is the
    /// only place that can assign positions, and it does so exactly once per
    /// item identity.
    item_positions: ItemPositionAllocator,
    pub input_tokens: u32,
    pub output_tokens: u32,
    pub cache_read_tokens: u32,
    pub cache_write_tokens: u32,
    /// Whether we are currently inside a reasoning tag block in streamed content.
    in_think_block: bool,
    /// Which reasoning tag opened the current block.
    current_reasoning_tag: Option<ReasoningTag>,
    /// Whether we are still stripping leading whitespace at the start of a
    /// reasoning block. Set to `true` when entering a reasoning tag, cleared once
    /// non-whitespace reasoning content is emitted.
    think_strip_leading_ws: bool,
    /// Whether we are still stripping leading whitespace from visible content
    /// after exiting a reasoning tag block. Models often emit `\n\n` between
    /// reasoning and the actual answer.
    visible_strip_leading_ws: bool,
    /// Buffer for detecting reasoning tags that may be split
    /// across SSE chunk boundaries.
    tag_buffer: String,
}

impl Default for StreamingToolState {
    fn default() -> Self {
        Self {
            segment_id: ProviderSegmentId::new(format!(
                "seg_{}",
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_nanos()
            )),
            segment_started: false,
            segment_closed: false,
            terminal_seen: false,
            next_seq: 0,
            tool_calls: HashMap::new(),
            item_positions: ItemPositionAllocator::default(),
            input_tokens: 0,
            output_tokens: 0,
            cache_read_tokens: 0,
            cache_write_tokens: 0,
            in_think_block: false,
            current_reasoning_tag: None,
            think_strip_leading_ws: false,
            visible_strip_leading_ws: false,
            tag_buffer: String::new(),
        }
    }
}

impl StreamingToolState {
    pub fn next_seq(&mut self) -> u64 {
        self.next_seq += 1;
        self.next_seq
    }

    /// Record that the provider announced the end of this response.
    pub fn mark_terminal(&mut self) {
        self.terminal_seen = true;
    }

    /// Close this segment because the transport failed mid-response.
    ///
    /// The caller aborts the stream without finalizing it, so the close is
    /// emitted here. A segment left open would later be replayed as if the
    /// response were still in progress.
    pub fn close_on_transport_error(&mut self) -> Vec<StreamEvent> {
        if !self.segment_started || self.segment_closed {
            return Vec::new();
        }
        self.segment_closed = true;
        vec![StreamEvent::SegmentClose {
            segment_id: self.segment_id.clone(),
            outcome: ProviderSegmentOutcome::TransportError,
            usage: Some(Usage {
                input_tokens: self.input_tokens,
                output_tokens: self.output_tokens,
                cache_read_tokens: self.cache_read_tokens,
                cache_write_tokens: self.cache_write_tokens,
            }),
        }]
    }

    /// Canonical position of `item_id`, assigned on first sight and stable
    /// afterwards.
    fn position_for(&mut self, item_id: &ProviderItemId) -> ProviderItemPosition {
        self.item_positions.position_for(item_id)
    }
}

/// Result of processing a single SSE line.
#[derive(Debug)]
pub enum SseLineResult {
    /// No actionable event (empty line, non-data prefix)
    Skip,
    /// Stream is done
    Done,
    /// Events to yield
    Events(Vec<StreamEvent>),
}

/// Report a malformed Chat Completions event and fail the stream.
///
/// Substituting a value for a missing correlation field would merge unrelated
/// provider items, so the segment is closed as failed and the defect is
/// reported instead.
fn chat_protocol_error(
    state: &mut StreamingToolState,
    mut events: Vec<StreamEvent>,
    detail: String,
) -> SseLineResult {
    tracing::error!(
        segment_id = %state.segment_id,
        detail = %detail,
        "provider sent a malformed Chat Completions event"
    );
    if !state.segment_closed {
        state.segment_closed = true;
        events.push(StreamEvent::SegmentClose {
            segment_id: state.segment_id.clone(),
            outcome: ProviderSegmentOutcome::Failed,
            usage: None,
        });
    }
    events.push(StreamEvent::Error(detail));
    SseLineResult::Events(events)
}

/// Result of processing a single Responses API SSE line.
///
/// Unlike Chat Completions, the Responses API distinguishes successful and
/// unsuccessful terminal events. The caller must not finalize a failed stream
/// with [`StreamEvent::Done`].
#[derive(Debug)]
pub enum ResponsesEventResult {
    /// No actionable event (invalid JSON or an unrecognized event type).
    Skip,
    /// Non-terminal events to yield.
    Events(Vec<StreamEvent>),
    /// The stream completed successfully; yield events before finalizing.
    Completed(Vec<StreamEvent>),
    /// The stream failed or was incomplete; yield events and stop without finalizing.
    Failed(Vec<StreamEvent>),
}

/// Emit a `ReasoningDelta`, stripping leading whitespace at the start of a
/// think block so the UI doesn't show a blank prefix.
fn emit_reasoning(text: String, state: &mut StreamingToolState, events: &mut Vec<StreamEvent>) {
    if text.is_empty() {
        return;
    }
    let emitted = if state.think_strip_leading_ws {
        let trimmed = text.trim_start();
        if trimmed.is_empty() {
            // Entire chunk was whitespace — keep stripping
            return;
        }
        state.think_strip_leading_ws = false;
        trimmed.to_string()
    } else {
        text
    };
    let seq = state.next_seq();
    let item_id = ProviderItemId::new(CHAT_REASONING_ITEM_ID);
    let position = state.position_for(&item_id);
    events.push(StreamEvent::ProviderItemUpdate(ProviderItemUpdate {
        segment_id: state.segment_id.clone(),
        item_id,
        position,
        update_seq: seq,
        payload: ProviderItemUpdatePayload::ReasoningTextDelta { delta: emitted },
    }));
}

/// Emit a visible `Delta`, stripping leading whitespace after a `</think>`
/// block so the UI doesn't show blank lines before the answer.
fn emit_visible(text: String, state: &mut StreamingToolState, events: &mut Vec<StreamEvent>) {
    if text.is_empty() {
        return;
    }
    let emitted = if state.visible_strip_leading_ws {
        let trimmed = text.trim_start();
        if trimmed.is_empty() {
            // Entire chunk was whitespace — keep stripping
            return;
        }
        state.visible_strip_leading_ws = false;
        trimmed.to_string()
    } else {
        text
    };
    let seq = state.next_seq();
    let item_id = ProviderItemId::new(CHAT_MESSAGE_ITEM_ID);
    let position = state.position_for(&item_id);
    events.push(StreamEvent::ProviderItemUpdate(ProviderItemUpdate {
        segment_id: state.segment_id.clone(),
        item_id,
        position,
        update_seq: seq,
        payload: ProviderItemUpdatePayload::MessageDelta {
            delta: emitted.clone(),
        },
    }));
    events.push(StreamEvent::Delta(emitted));
}

/// Process streamed content through the reasoning-tag state machine.
///
/// Content arriving inside `<think>...</think>` or `<thought>...</thought>` is emitted as
/// `ReasoningDelta`; content outside is emitted as `Delta`.
/// Tags may be split across SSE chunks — `tag_buffer` accumulates
/// partial tag fragments until they can be resolved.
/// Leading whitespace at the start of each reasoning block is stripped.
fn process_content_think_tags(
    content: &str,
    state: &mut StreamingToolState,
    events: &mut Vec<StreamEvent>,
) {
    state.tag_buffer.push_str(content);

    loop {
        if state.in_think_block {
            let Some(current_tag) = state.current_reasoning_tag else {
                state.in_think_block = false;
                continue;
            };
            let close_tag = current_tag.close();
            // Look for the matching close tag to exit reasoning mode.
            match state.tag_buffer.find(close_tag) {
                Some(pos) => {
                    let thinking = state.tag_buffer[..pos].to_string();
                    emit_reasoning(thinking, state, events);
                    state.in_think_block = false;
                    state.current_reasoning_tag = None;
                    state.visible_strip_leading_ws = true;
                    let rest = state.tag_buffer[pos + close_tag.len()..].to_string();
                    state.tag_buffer = rest;
                    // Continue loop to process remaining content
                },
                None => {
                    // Check if buffer ends with a prefix of the closing tag
                    // to avoid emitting partial tag as reasoning text.
                    let suffix_match = longest_tag_suffix(&state.tag_buffer, close_tag);
                    if suffix_match > 0 {
                        let safe = state.tag_buffer.len() - suffix_match;
                        let emit = state.tag_buffer[..safe].to_string();
                        emit_reasoning(emit, state, events);
                        let kept = state.tag_buffer[safe..].to_string();
                        state.tag_buffer = kept;
                    } else {
                        // No partial tag — emit everything as reasoning
                        let buf = std::mem::take(&mut state.tag_buffer);
                        emit_reasoning(buf, state, events);
                    }
                    break;
                },
            }
        } else {
            // Look for an opening reasoning tag to enter reasoning mode.
            match find_next_open_tag(&state.tag_buffer) {
                Some((pos, tag)) => {
                    let visible = state.tag_buffer[..pos].to_string();
                    emit_visible(visible, state, events);
                    state.in_think_block = true;
                    state.current_reasoning_tag = Some(tag);
                    state.think_strip_leading_ws = true;
                    let rest = state.tag_buffer[pos + tag.open().len()..].to_string();
                    state.tag_buffer = rest;
                    // Continue loop to process remaining content
                },
                None => {
                    // Check if buffer ends with a prefix of any opening reasoning tag.
                    let suffix_match = longest_reasoning_open_tag_suffix(&state.tag_buffer);
                    if suffix_match > 0 {
                        let safe = state.tag_buffer.len() - suffix_match;
                        let emit = state.tag_buffer[..safe].to_string();
                        emit_visible(emit, state, events);
                        let kept = state.tag_buffer[safe..].to_string();
                        state.tag_buffer = kept;
                    } else {
                        // No partial tag — emit everything as visible
                        let buf = std::mem::take(&mut state.tag_buffer);
                        emit_visible(buf, state, events);
                    }
                    break;
                },
            }
        }
    }
}

/// Return the length of the longest suffix of `text` that is a prefix of `tag`.
///
/// For example, `longest_tag_suffix("abc<th", "<think>")` returns 3 because
/// `"<th"` is a 3-character prefix of `"<think>"`.
fn longest_tag_suffix(text: &str, tag: &str) -> usize {
    let text_bytes = text.as_bytes();
    let tag_bytes = tag.as_bytes();
    let max_check = text_bytes.len().min(tag_bytes.len());
    for len in (1..=max_check).rev() {
        if text_bytes[text_bytes.len() - len..] == tag_bytes[..len] {
            return len;
        }
    }
    0
}

/// Process a single SSE data line and return any events to yield.
///
/// This handles the common OpenAI streaming format used by:
/// - OpenAI API
/// - Any other OpenAI-compatible API
///
/// Content inside `<think>...</think>` tags is emitted as `ReasoningDelta`
/// events rather than `Delta`, allowing the UI to show reasoning text
/// separately. This handles models (DeepSeek R1, QwQ, MiniMax) that embed
/// chain-of-thought in `content` rather than using `reasoning_content`.
pub fn process_openai_sse_line(data: &str, state: &mut StreamingToolState) -> SseLineResult {
    if data == "[DONE]" {
        state.mark_terminal();
        return SseLineResult::Done;
    }

    let Ok(evt) = serde_json::from_str::<serde_json::Value>(data) else {
        return SseLineResult::Skip;
    };

    let mut events = vec![StreamEvent::ProviderRaw(evt.clone())];
    if !state.segment_started {
        state.segment_started = true;
        if let Some(id) = evt.get("id").and_then(serde_json::Value::as_str) {
            state.segment_id = ProviderSegmentId::new(id);
        }
        events.push(StreamEvent::SegmentStart {
            segment_id: state.segment_id.clone(),
        });
    }

    if let Some(usage) = parse_openai_compat_usage_from_payload(&evt) {
        state.input_tokens = usage.input_tokens;
        state.output_tokens = usage.output_tokens;
        state.cache_read_tokens = usage.cache_read_tokens;
        state.cache_write_tokens = usage.cache_write_tokens;
    }

    let delta = &evt["choices"][0]["delta"];

    // Handle user-visible text content, stripping <think> tags.
    if let Some(content) = delta["content"].as_str()
        && !content.is_empty()
    {
        process_content_think_tags(content, state, &mut events);
    }

    // Some OpenAI-compatible backends stream planning text in
    // `reasoning_content` or `reasoning`. Surface it separately so UI can
    //  show it in the thinking area without polluting final assistant text.
    let reasoning_text = delta["reasoning_content"]
        .as_str()
        .or_else(|| delta["reasoning"].as_str());
    if let Some(reasoning_content) = reasoning_text
        && !reasoning_content.is_empty()
    {
        let seq = state.next_seq();
        let item_id = ProviderItemId::new(CHAT_REASONING_ITEM_ID);
        let position = state.position_for(&item_id);
        events.push(StreamEvent::ProviderItemUpdate(ProviderItemUpdate {
            segment_id: state.segment_id.clone(),
            item_id,
            position,
            update_seq: seq,
            payload: ProviderItemUpdatePayload::ReasoningTextDelta {
                delta: reasoning_content.to_string(),
            },
        }));
    }

    // Handle tool calls
    if let Some(tcs) = delta["tool_calls"].as_array() {
        for tc in tcs {
            // `index` correlates argument deltas with the call that opened the
            // slot. Defaulting it would append arguments to an unrelated call.
            let Some(index) = tc["index"].as_u64().map(|index| index as usize) else {
                return chat_protocol_error(
                    state,
                    events,
                    "tool_call delta has no index".to_string(),
                );
            };

            // Check if this is a new tool call (has id and function.name)
            if let (Some(id), Some(name)) = (tc["id"].as_str(), tc["function"]["name"].as_str()) {
                state
                    .tool_calls
                    .insert(index, (id.to_string(), name.to_string(), String::new()));
                let seq = state.next_seq();
                let item_id = ProviderItemId::new(id);
                let position = state.position_for(&item_id);
                events.push(StreamEvent::ProviderItemUpdate(ProviderItemUpdate {
                    segment_id: state.segment_id.clone(),
                    item_id,
                    position,
                    update_seq: seq,
                    payload: ProviderItemUpdatePayload::FunctionCallStart {
                        name: name.to_string(),
                    },
                }));
                events.push(StreamEvent::ToolCallStart {
                    id: id.to_string(),
                    name: name.to_string(),
                    index,
                });
            }

            // Handle arguments delta
            if let Some(args_delta) = tc["function"]["arguments"].as_str()
                && !args_delta.is_empty()
            {
                let tool_id = if let Some((id, _, args_buf)) = state.tool_calls.get_mut(&index) {
                    args_buf.push_str(args_delta);
                    Some(id.clone())
                } else {
                    None
                };
                if let Some(id) = tool_id {
                    let seq = state.next_seq();
                    let item_id = ProviderItemId::new(id);
                    let position = state.position_for(&item_id);
                    events.push(StreamEvent::ProviderItemUpdate(ProviderItemUpdate {
                        segment_id: state.segment_id.clone(),
                        item_id,
                        position,
                        update_seq: seq,
                        payload: ProviderItemUpdatePayload::FunctionCallDelta {
                            delta: args_delta.to_string(),
                        },
                    }));
                }
                events.push(StreamEvent::ToolCallArgumentsDelta {
                    index,
                    delta: args_delta.to_string(),
                });
            }
        }
    }

    // Detect error finish reasons (e.g. "network_error", "content_filter").
    // Normal reasons (null, "stop", "tool_calls", "length") are not errors.
    if let Some(reason) = evt["choices"][0]["finish_reason"].as_str() {
        state.mark_terminal();
        match reason {
            "stop" | "tool_calls" | "length" | "function_call" => {},
            error_reason => {
                events.push(StreamEvent::Error(format!(
                    "Provider stream ended with finish_reason: {error_reason}"
                )));
            },
        }
    }

    SseLineResult::Events(events)
}

/// Generate the final events when stream ends (tool call completions + done).
///
/// Any residual content in the think-tag buffer is flushed as the appropriate
/// event type (reasoning if we were inside a think block, visible otherwise).
pub fn finalize_stream(state: &mut StreamingToolState) -> Vec<StreamEvent> {
    let mut events = Vec::new();

    // Flush any remaining think-tag buffer content
    if !state.tag_buffer.is_empty() {
        let remaining = std::mem::take(&mut state.tag_buffer);
        if state.in_think_block {
            let seq = state.next_seq();
            let item_id = ProviderItemId::new(CHAT_REASONING_ITEM_ID);
            let position = state.position_for(&item_id);
            events.push(StreamEvent::ProviderItemUpdate(ProviderItemUpdate {
                segment_id: state.segment_id.clone(),
                item_id,
                position,
                update_seq: seq,
                payload: ProviderItemUpdatePayload::ReasoningTextDelta { delta: remaining },
            }));
        } else {
            let seq = state.next_seq();
            let item_id = ProviderItemId::new(CHAT_MESSAGE_ITEM_ID);
            let position = state.position_for(&item_id);
            events.push(StreamEvent::ProviderItemUpdate(ProviderItemUpdate {
                segment_id: state.segment_id.clone(),
                item_id,
                position,
                update_seq: seq,
                payload: ProviderItemUpdatePayload::MessageDelta {
                    delta: remaining.clone(),
                },
            }));
            events.push(StreamEvent::Delta(remaining));
        }
    }

    // Emit completion for any pending tool calls
    for index in state.tool_calls.keys() {
        events.push(StreamEvent::ToolCallComplete { index: *index });
    }

    if state.segment_started && !state.segment_closed {
        state.segment_closed = true;
        // Only a provider-announced terminal marker completes a segment. A
        // stream that just ends was cut off, and reporting it as completed
        // would turn a truncated answer into a successful one.
        let outcome = if state.terminal_seen {
            ProviderSegmentOutcome::Completed
        } else {
            ProviderSegmentOutcome::TransportError
        };
        events.push(StreamEvent::SegmentClose {
            segment_id: state.segment_id.clone(),
            outcome,
            usage: Some(Usage {
                input_tokens: state.input_tokens,
                output_tokens: state.output_tokens,
                cache_read_tokens: state.cache_read_tokens,
                cache_write_tokens: state.cache_write_tokens,
            }),
        });
        if !state.terminal_seen {
            tracing::error!(
                segment_id = %state.segment_id,
                "Chat Completions stream ended without finish_reason or [DONE]"
            );
            events.push(StreamEvent::Error(
                "provider stream closed before announcing a finish_reason".to_string(),
            ));
        }
    }
    events.push(StreamEvent::Done(Usage {
        input_tokens: state.input_tokens,
        output_tokens: state.output_tokens,
        cache_read_tokens: state.cache_read_tokens,
        cache_write_tokens: state.cache_write_tokens,
    }));

    events
}

// ============================================================================
// Responses API helpers
// ============================================================================

/// Split system messages into `instructions` and convert the rest to Responses
/// API `input` items.
///
/// The Responses API uses a top-level `instructions` field instead of a system
/// message role.  This function extracts all system messages, joins them with
/// `\n\n`, and converts the remaining messages via [`to_responses_input`].
#[must_use]
pub fn split_responses_instructions_and_input(
    messages: Vec<ChatMessage>,
) -> (Option<String>, Vec<serde_json::Value>) {
    let mut instruction_parts: Vec<String> = Vec::new();
    let mut non_system: Vec<ChatMessage> = Vec::new();

    for message in messages {
        match message {
            ChatMessage::System { content } => {
                if !content.trim().is_empty() {
                    instruction_parts.push(content);
                }
            },
            other => non_system.push(other),
        }
    }

    let instructions = if instruction_parts.is_empty() {
        None
    } else {
        Some(instruction_parts.join("\n\n"))
    };

    (instructions, to_responses_input(&non_system))
}

/// State for tracking Responses API SSE streaming.
pub struct ResponsesStreamState {
    pub segment_id: ProviderSegmentId,
    pub segment_started: bool,
    /// Set once the segment has been closed by a terminal outcome. A segment is
    /// closed exactly once; finalization must not emit a second close.
    pub segment_closed: bool,
    pub next_seq: u64,
    pub tool_calls: HashMap<usize, (String, String)>,
    /// Set of tool call indices that have already emitted `ToolCallComplete`.
    pub completed_tool_calls: HashSet<usize>,
    /// The next tool call index to assign.
    pub current_tool_index: usize,
    /// Canonical positions for this segment, assigned in first-appearance order.
    ///
    /// The `output_index` carried by Responses delta events orders items only
    /// within a channel for some providers, so two distinct items can share one
    /// value inside a single response. Positions therefore come from the order
    /// in which item identities first appear on the stream.
    item_positions: ItemPositionAllocator,
    pub input_tokens: u32,
    pub output_tokens: u32,
    pub cache_read_tokens: u32,
    pub cache_write_tokens: u32,
    /// Reasoning summary parts already emitted from streaming events, keyed by
    /// the item they belong to and their summary index.
    ///
    /// The final `output_item.done` repeats the whole summary. A part that
    /// already arrived as a delta is not emitted again, so the two sources
    /// cannot produce the same text twice.
    pub streamed_reasoning_summary_parts: HashSet<(String, usize)>,
}

impl Default for ResponsesStreamState {
    fn default() -> Self {
        Self {
            segment_id: ProviderSegmentId::new(format!(
                "resp_{}",
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_nanos()
            )),
            segment_started: false,
            segment_closed: false,
            next_seq: 0,
            tool_calls: HashMap::new(),
            completed_tool_calls: HashSet::new(),
            current_tool_index: 0,
            item_positions: ItemPositionAllocator::default(),
            input_tokens: 0,
            output_tokens: 0,
            cache_read_tokens: 0,
            cache_write_tokens: 0,
            streamed_reasoning_summary_parts: HashSet::new(),
        }
    }
}

impl ResponsesStreamState {
    pub fn next_seq(&mut self) -> u64 {
        self.next_seq += 1;
        self.next_seq
    }

    /// Canonical position of `item_id`, assigned on first sight and stable
    /// afterwards.
    fn position_for(&mut self, item_id: &ProviderItemId) -> ProviderItemPosition {
        self.item_positions.position_for(item_id)
    }

    /// Close this segment because the transport failed mid-response.
    ///
    /// The caller aborts the stream without finalizing it, so the close is
    /// emitted here. A segment left open would later be replayed as if the
    /// response were still in progress.
    pub fn close_on_transport_error(&mut self) -> Vec<StreamEvent> {
        if !self.segment_started || self.segment_closed {
            return Vec::new();
        }
        self.segment_closed = true;
        vec![StreamEvent::SegmentClose {
            segment_id: self.segment_id.clone(),
            outcome: ProviderSegmentOutcome::TransportError,
            usage: Some(Usage {
                input_tokens: self.input_tokens,
                output_tokens: self.output_tokens,
                cache_read_tokens: self.cache_read_tokens,
                cache_write_tokens: self.cache_write_tokens,
            }),
        }]
    }
}

/// Tool call slot referenced by a Responses function-call event.
///
/// The `output_index` of these events is the provider's own tool call slot; it
/// is how the transport correlates `function_call_arguments.*` with the
/// `output_item.added` that opened the call. It is a tool-call index, not an
/// output position, so it is used only for that correlation.
///
/// Returns `None` when the provider sends no slot at all. Guessing one would
/// attach arguments to an unrelated tool call, so the caller must fail instead.
fn responses_tool_slot(event: &serde_json::Value) -> Option<usize> {
    event
        .get("output_index")
        .or_else(|| event.get("item_index"))
        .or_else(|| event.get("index"))
        .and_then(serde_json::Value::as_u64)
        .map(|index| index as usize)
}

/// Report a malformed provider event and fail the stream.
///
/// A missing identity or slot cannot be recovered from: any substitute value
/// silently merges or splits provider items. The segment is closed as failed so
/// the defect surfaces instead of corrupting the transcript.
fn responses_protocol_error(
    state: &mut ResponsesStreamState,
    mut events: Vec<StreamEvent>,
    detail: String,
) -> ResponsesEventResult {
    tracing::error!(
        segment_id = %state.segment_id,
        detail = %detail,
        "provider sent a malformed Responses event"
    );
    if !state.segment_closed {
        state.segment_closed = true;
        events.push(StreamEvent::SegmentClose {
            segment_id: state.segment_id.clone(),
            outcome: ProviderSegmentOutcome::Failed,
            usage: None,
        });
    }
    events.push(StreamEvent::Error(detail));
    ResponsesEventResult::Failed(events)
}

/// Both SSE and WebSocket transports call this function so Responses event
/// semantics cannot diverge between transports.
pub fn process_responses_event(
    evt: serde_json::Value,
    state: &mut ResponsesStreamState,
) -> ResponsesEventResult {
    let raw = StreamEvent::ProviderRaw(evt.clone());

    let mut init_events = Vec::new();

    if !state.segment_started {
        state.segment_started = true;
        if let Some(id) = evt
            .get("response")
            .and_then(|r| r.get("id"))
            .or_else(|| evt.get("id"))
            .and_then(serde_json::Value::as_str)
        {
            state.segment_id = ProviderSegmentId::new(id);
        }
        init_events.push(StreamEvent::SegmentStart {
            segment_id: state.segment_id.clone(),
        });
    }
    match evt["type"].as_str().unwrap_or("") {
        "response.output_text.delta" => {
            let mut events = init_events;
            events.push(raw);
            if let Some(delta) = evt["delta"].as_str()
                && !delta.is_empty()
            {
                let Some(id) = evt["item_id"].as_str().filter(|id| !id.is_empty()) else {
                    return responses_protocol_error(
                        state,
                        events,
                        "response.output_text.delta has no item_id".to_string(),
                    );
                };
                let item_id = ProviderItemId::new(id);
                let position = state.position_for(&item_id);
                let seq = state.next_seq();
                events.push(StreamEvent::ProviderItemUpdate(ProviderItemUpdate {
                    segment_id: state.segment_id.clone(),
                    item_id,
                    position,
                    update_seq: seq,
                    payload: ProviderItemUpdatePayload::MessageDelta {
                        delta: delta.to_string(),
                    },
                }));
                events.push(StreamEvent::Delta(delta.to_string()));
            }
            ResponsesEventResult::Events(events)
        },
        "response.reasoning_summary_text.delta" => {
            let mut events = init_events;
            events.push(raw);
            if let (Some(item_id), Some(summary_index), Some(delta)) = (
                evt["item_id"].as_str(),
                evt["summary_index"].as_u64().map(|index| index as usize),
                evt["delta"].as_str(),
            ) && !item_id.is_empty()
                && !delta.is_empty()
            {
                state
                    .streamed_reasoning_summary_parts
                    .insert((item_id.to_string(), summary_index));
                let item_id = ProviderItemId::new(item_id);
                let position = state.position_for(&item_id);
                let seq = state.next_seq();
                events.push(StreamEvent::ProviderItemUpdate(ProviderItemUpdate {
                    segment_id: state.segment_id.clone(),
                    item_id,
                    position,
                    update_seq: seq,
                    payload: ProviderItemUpdatePayload::ReasoningDelta {
                        part_index: summary_index,
                        delta: delta.to_string(),
                    },
                }));
            }
            ResponsesEventResult::Events(events)
        },
        "response.reasoning_summary_part.done" => {
            let mut events = init_events;
            events.push(raw);
            if let (Some(item_id), Some(summary_index), Some(text)) = (
                evt["item_id"].as_str(),
                evt["summary_index"].as_u64().map(|index| index as usize),
                evt["part"]["text"].as_str(),
            ) && !item_id.is_empty()
            {
                state
                    .streamed_reasoning_summary_parts
                    .insert((item_id.to_string(), summary_index));
                let item_id = ProviderItemId::new(item_id);
                let position = state.position_for(&item_id);
                let seq = state.next_seq();
                events.push(StreamEvent::ProviderItemUpdate(ProviderItemUpdate {
                    segment_id: state.segment_id.clone(),
                    item_id,
                    position,
                    update_seq: seq,
                    payload: ProviderItemUpdatePayload::ReasoningPartDone {
                        part_index: summary_index,
                        text: text.to_string(),
                    },
                }));
            }
            ResponsesEventResult::Events(events)
        },
        "response.output_item.added" => {
            let mut events = init_events;
            events.push(raw);
            if evt["item"]["type"].as_str() == Some("function_call") {
                let Some(id) = evt["item"]["call_id"]
                    .as_str()
                    .filter(|id| !id.is_empty())
                    .map(ToString::to_string)
                else {
                    return responses_protocol_error(
                        state,
                        events,
                        "response.output_item.added function_call has no call_id".to_string(),
                    );
                };
                let Some(name) = evt["item"]["name"]
                    .as_str()
                    .filter(|name| !name.is_empty())
                    .map(ToString::to_string)
                else {
                    return responses_protocol_error(
                        state,
                        events,
                        format!("function_call `{id}` has no name"),
                    );
                };
                let index = responses_tool_slot(&evt).unwrap_or(state.current_tool_index);
                // `output_item.added` opens a slot rather than referencing one,
                // so an absent index is assigned in arrival order. Events that
                // reference an existing slot must carry it and are rejected
                // otherwise.
                state.current_tool_index = state.current_tool_index.max(index + 1);
                state.tool_calls.insert(index, (id.clone(), name.clone()));
                let item_id = ProviderItemId::new(id.clone());
                let position = state.position_for(&item_id);
                let seq = state.next_seq();
                events.push(StreamEvent::ProviderItemUpdate(ProviderItemUpdate {
                    segment_id: state.segment_id.clone(),
                    item_id,
                    position,
                    update_seq: seq,
                    payload: ProviderItemUpdatePayload::FunctionCallStart { name: name.clone() },
                }));
                events.push(StreamEvent::ToolCallStart { id, name, index });
            }
            ResponsesEventResult::Events(events)
        },
        "response.output_item.done" => {
            let mut events = init_events;
            events.push(raw);
            if evt["item"]["type"].as_str() == Some("reasoning") {
                let encrypted_content = evt["item"]["encrypted_content"]
                    .as_str()
                    .map(ToString::to_string);
                let Some(id) = evt["item"]["id"].as_str().filter(|id| !id.is_empty()) else {
                    return responses_protocol_error(
                        state,
                        events,
                        "response.output_item.done reasoning item has no id".to_string(),
                    );
                };
                let item_id = ProviderItemId::new(id);
                let position = state.position_for(&item_id);
                events.extend(reasoning_summary_events(
                    &evt["item"],
                    &item_id,
                    position,
                    state,
                ));
                let seq = state.next_seq();
                events.push(StreamEvent::ProviderItemUpdate(ProviderItemUpdate {
                    segment_id: state.segment_id.clone(),
                    item_id,
                    position,
                    update_seq: seq,
                    payload: ProviderItemUpdatePayload::ReasoningItemDone { encrypted_content },
                }));
            }
            ResponsesEventResult::Events(events)
        },
        "response.function_call_arguments.delta" => {
            let mut events = init_events;
            events.push(raw);
            if let Some(delta) = evt["delta"].as_str()
                && !delta.is_empty()
            {
                let Some(index) = responses_tool_slot(&evt) else {
                    return responses_protocol_error(
                        state,
                        events,
                        "response.function_call_arguments.delta has no output_index".to_string(),
                    );
                };
                let Some(call_id) = state.tool_calls.get(&index).map(|(id, _)| id.clone()) else {
                    return responses_protocol_error(
                        state,
                        events,
                        format!(
                            "response.function_call_arguments.delta references unopened tool call slot {index}"
                        ),
                    );
                };
                let item_id = ProviderItemId::new(call_id);
                let position = state.position_for(&item_id);
                let seq = state.next_seq();
                events.push(StreamEvent::ProviderItemUpdate(ProviderItemUpdate {
                    segment_id: state.segment_id.clone(),
                    item_id,
                    position,
                    update_seq: seq,
                    payload: ProviderItemUpdatePayload::FunctionCallDelta {
                        delta: delta.to_string(),
                    },
                }));
                events.push(StreamEvent::ToolCallArgumentsDelta {
                    index,
                    delta: delta.to_string(),
                });
            }
            ResponsesEventResult::Events(events)
        },
        "response.function_call_arguments.done" => {
            let mut events = init_events;
            events.push(raw);
            let Some(index) = responses_tool_slot(&evt) else {
                return responses_protocol_error(
                    state,
                    events,
                    "response.function_call_arguments.done has no output_index".to_string(),
                );
            };
            let Some(call_id) = state.tool_calls.get(&index).map(|(id, _)| id.clone()) else {
                return responses_protocol_error(
                    state,
                    events,
                    format!(
                        "response.function_call_arguments.done references unopened tool call slot {index}"
                    ),
                );
            };
            let Some(arguments) = evt["arguments"].as_str().map(ToString::to_string) else {
                return responses_protocol_error(
                    state,
                    events,
                    format!("function_call `{call_id}` completed without an arguments field"),
                );
            };
            let item_id = ProviderItemId::new(call_id);
            let position = state.position_for(&item_id);
            let seq = state.next_seq();
            events.push(StreamEvent::ProviderItemUpdate(ProviderItemUpdate {
                segment_id: state.segment_id.clone(),
                item_id,
                position,
                update_seq: seq,
                payload: ProviderItemUpdatePayload::FunctionCallDone { arguments },
            }));
            if state.completed_tool_calls.insert(index) {
                events.push(StreamEvent::ToolCallComplete { index });
            }
            ResponsesEventResult::Events(events)
        },
        "response.completed" => {
            let mut events = init_events;
            events.push(raw);
            let usage = evt
                .get("response")
                .and_then(|response| response.get("usage"))
                .map(parse_openai_compat_usage);
            if let Some(ref u) = usage {
                state.input_tokens = u.input_tokens;
                state.output_tokens = u.output_tokens;
                state.cache_read_tokens = u.cache_read_tokens;
                state.cache_write_tokens = u.cache_write_tokens;
            }
            state.segment_closed = true;
            events.push(StreamEvent::SegmentClose {
                segment_id: state.segment_id.clone(),
                outcome: ProviderSegmentOutcome::Completed,
                usage,
            });
            ResponsesEventResult::Completed(events)
        },
        "error" | "response.failed" => {
            let mut events = init_events;
            events.push(raw);
            let msg = evt["error"]["message"]
                .as_str()
                .or_else(|| evt["response"]["error"]["message"].as_str())
                .or_else(|| evt["message"].as_str())
                .unwrap_or("unknown error");
            state.segment_closed = true;
            events.push(StreamEvent::SegmentClose {
                segment_id: state.segment_id.clone(),
                outcome: ProviderSegmentOutcome::Failed,
                usage: None,
            });
            events.push(StreamEvent::Error(msg.to_string()));
            ResponsesEventResult::Failed(events)
        },
        "response.incomplete" => {
            let mut events = init_events;
            events.push(raw);
            let msg = evt["response"]["incomplete_details"]["reason"]
                .as_str()
                .map(|reason| format!("response incomplete: {reason}"))
                .or_else(|| {
                    evt["response"]["error"]["message"]
                        .as_str()
                        .map(ToString::to_string)
                })
                .or_else(|| evt["message"].as_str().map(ToString::to_string))
                .unwrap_or_else(|| "response incomplete".to_string());
            state.segment_closed = true;
            events.push(StreamEvent::SegmentClose {
                segment_id: state.segment_id.clone(),
                outcome: ProviderSegmentOutcome::Incomplete,
                usage: None,
            });
            events.push(StreamEvent::Error(msg));
            ResponsesEventResult::Failed(events)
        },
        _ => {
            let mut events = init_events;
            events.push(raw);
            ResponsesEventResult::Events(events)
        },
    }
}

/// Decode and process one SSE data line from a Responses API stream.
pub fn process_responses_sse_line(
    data: &str,
    state: &mut ResponsesStreamState,
) -> ResponsesEventResult {
    if data == "[DONE]" {
        return ResponsesEventResult::Completed(Vec::new());
    }
    let Ok(event) = serde_json::from_str::<serde_json::Value>(data) else {
        return ResponsesEventResult::Skip;
    };
    process_responses_event(event, state)
}

/// Generate the final events when a Responses API stream ends.
///
/// Emits `ToolCallComplete` for any pending tool calls and a final `Done` with
/// accumulated usage.
pub fn finalize_responses_stream(state: &mut ResponsesStreamState) -> Vec<StreamEvent> {
    let mut events = Vec::new();

    let mut pending: Vec<usize> = state.tool_calls.keys().copied().collect();
    pending.sort_unstable();
    for index in pending {
        if state.completed_tool_calls.insert(index) {
            events.push(StreamEvent::ToolCallComplete { index });
        }
    }

    if state.segment_started && !state.segment_closed {
        state.segment_closed = true;
        // The Responses API always announces its outcome with
        // `response.completed`, `response.failed` or `response.incomplete`,
        // each of which closes the segment where it is handled. Reaching this
        // point means the stream ended before any of them arrived.
        tracing::error!(
            segment_id = %state.segment_id,
            "Responses stream closed before a terminal response event"
        );
        events.push(StreamEvent::SegmentClose {
            segment_id: state.segment_id.clone(),
            outcome: ProviderSegmentOutcome::TransportError,
            usage: Some(Usage {
                input_tokens: state.input_tokens,
                output_tokens: state.output_tokens,
                cache_read_tokens: state.cache_read_tokens,
                cache_write_tokens: state.cache_write_tokens,
            }),
        });
        events.push(StreamEvent::Error(
            "provider stream closed before response.completed".to_string(),
        ));
    }
    events.push(StreamEvent::Done(Usage {
        input_tokens: state.input_tokens,
        output_tokens: state.output_tokens,
        cache_read_tokens: state.cache_read_tokens,
        cache_write_tokens: state.cache_write_tokens,
    }));

    events
}

/// Parse a non-streaming Responses API JSON response into [`CompletionResponse`].
///
/// The Responses API returns an `output` array containing `message` items
/// (with `content[].text`) and `function_call` items (with `call_id`, `name`,
/// `arguments`).
pub fn parse_responses_completion(resp: &serde_json::Value) -> CompletionResponse {
    let mut text: Option<String> = None;
    let mut tool_calls: Vec<ToolCall> = Vec::new();

    if let Some(output) = resp.get("output").and_then(|o| o.as_array()) {
        for item in output {
            match item["type"].as_str().unwrap_or("") {
                "message" => {
                    if let Some(content) = item.get("content").and_then(|c| c.as_array()) {
                        for part in content {
                            if part["type"].as_str() == Some("output_text")
                                && let Some(t) = part["text"].as_str()
                            {
                                text = Some(text.map_or_else(|| t.to_string(), |prev| prev + t));
                            }
                        }
                    }
                },
                "function_call" => {
                    let id = item["call_id"].as_str().unwrap_or("").to_string();
                    let name = item["name"].as_str().unwrap_or("").to_string();
                    let decoded = decode_tool_call_arguments_with_diagnostic(item.get("arguments"));
                    tool_calls.push(ToolCall {
                        id,
                        name,
                        arguments: decoded.arguments,
                        argument_diagnostic: decoded.diagnostic,
                    });
                },
                _ => {},
            }
        }
    }

    let usage = resp
        .get("usage")
        .map(parse_openai_compat_usage)
        .unwrap_or_default();

    CompletionResponse {
        text,
        tool_calls,
        usage,
    }
}
