//! Shared types and small utility helpers used across `chelix_chat` modules.

use {
    serde::{Deserialize, Serialize},
    serde_json::Value,
};

use {
    chelix_agents::model::Usage,
    chelix_config::{AgentMemoryWriteMode, MemoryStyle, PromptMemoryMode},
};

/// Placeholder to match the old `BroadcastOpts` pattern. All fields are ignored;
/// the trait's `broadcast` always uses default behaviour.
#[derive(Default)]
pub struct BroadcastOpts {
    pub drop_if_slow: bool,
    pub state_version: Option<()>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum ReplyMedium {
    Text,
    Voice,
}

#[derive(Debug, Deserialize)]
pub(crate) struct InputChannelMeta {
    #[serde(default)]
    pub message_kind: Option<InputMessageKind>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct InputChannelDocumentFile {
    pub display_name: String,
    pub stored_filename: String,
    pub mime_type: String,
    #[serde(default)]
    pub size_bytes: Option<u64>,
}

#[derive(Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum InputMessageKind {
    Text,
    Voice,
    Audio,
    Photo,
    Document,
    Video,
    Other,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum InputMediumParam {
    Text,
    Voice,
}

#[derive(Debug, Clone)]
pub(crate) struct UsageSnapshot {
    total: Usage,
    request: Option<Usage>,
}

#[derive(Debug, Clone, Copy)]
struct UsageFields {
    input_tokens: u32,
    output_tokens: u32,
    cache_read_tokens: u32,
    cache_write_tokens: u32,
}

impl From<&Usage> for UsageFields {
    fn from(usage: &Usage) -> Self {
        Self {
            input_tokens: usage.input_tokens,
            output_tokens: usage.output_tokens,
            cache_read_tokens: usage.cache_read_tokens,
            cache_write_tokens: usage.cache_write_tokens,
        }
    }
}

impl UsageSnapshot {
    #[must_use]
    pub(crate) fn new(total: Usage, request: Option<Usage>) -> Self {
        Self { total, request }
    }

    fn total_fields(&self) -> UsageFields {
        UsageFields::from(&self.total)
    }

    fn request_fields(&self) -> Option<UsageFields> {
        self.request.as_ref().map(UsageFields::from)
    }

    fn request_or_total_fields(&self) -> UsageFields {
        self.request_fields().unwrap_or_else(|| self.total_fields())
    }
}

/// Typed broadcast payload for the "final" chat event.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ChatFinalBroadcast {
    pub run_id: String,
    pub session_key: String,
    pub state: &'static str,
    pub text: String,
    pub model: String,
    pub provider: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<String>,
    pub input_tokens: u32,
    pub output_tokens: u32,
    pub cache_read_tokens: u32,
    pub cache_write_tokens: u32,
    pub duration_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_input_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_output_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_cache_read_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_cache_write_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message_index: Option<usize>,
    pub reply_medium: ReplyMedium,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub iterations: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_calls_made: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub audio: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub audio_warning: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub seq: Option<u64>,
}

/// Typed broadcast payload for the "error" chat event.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ChatErrorBroadcast {
    pub run_id: String,
    pub session_key: String,
    pub state: &'static str,
    pub error: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub seq: Option<u64>,
}

#[derive(Clone)]
pub(crate) struct AssistantTurnOutput {
    pub text: String,
    /// Canonical history entry already persisted by the agent run. The caller
    /// must not append another assistant message when this is present.
    pub persisted_message_index: Option<usize>,
    pub input_tokens: u32,
    pub output_tokens: u32,
    pub cache_read_tokens: u32,
    pub cache_write_tokens: u32,
    pub duration_ms: u64,
    pub request_input_tokens: u32,
    pub request_output_tokens: u32,
    pub request_cache_read_tokens: u32,
    pub request_cache_write_tokens: u32,
    pub audio_path: Option<String>,
    pub reasoning: Option<String>,
    pub llm_api_response: Option<Value>,
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn build_chat_final_broadcast(
    run_id: &str,
    session_key: &str,
    text: String,
    model: String,
    provider: String,
    reasoning_effort: Option<String>,
    usage: UsageSnapshot,
    duration_ms: u64,
    message_index: Option<usize>,
    reply_medium: ReplyMedium,
    iterations: Option<usize>,
    tool_calls_made: Option<usize>,
    audio: Option<String>,
    audio_warning: Option<String>,
    reasoning: Option<String>,
    seq: Option<u64>,
) -> ChatFinalBroadcast {
    let total = usage.total_fields();
    let request = usage.request_fields();
    ChatFinalBroadcast {
        run_id: run_id.to_string(),
        session_key: session_key.to_string(),
        state: "final",
        text,
        model,
        provider,
        reasoning_effort,
        input_tokens: total.input_tokens,
        output_tokens: total.output_tokens,
        cache_read_tokens: total.cache_read_tokens,
        cache_write_tokens: total.cache_write_tokens,
        duration_ms,
        request_input_tokens: request.map(|usage| usage.input_tokens),
        request_output_tokens: request.map(|usage| usage.output_tokens),
        request_cache_read_tokens: request.map(|usage| usage.cache_read_tokens),
        request_cache_write_tokens: request.map(|usage| usage.cache_write_tokens),
        message_index,
        reply_medium,
        iterations,
        tool_calls_made,
        audio,
        audio_warning,
        reasoning,
        seq,
    }
}

pub(crate) fn build_assistant_turn_output(
    text: String,
    persisted_message_index: Option<usize>,
    usage: UsageSnapshot,
    duration_ms: u64,
    audio_path: Option<String>,
    reasoning: Option<String>,
    llm_api_response: Option<Value>,
) -> AssistantTurnOutput {
    let total = usage.total_fields();
    let request = usage.request_or_total_fields();
    AssistantTurnOutput {
        text,
        persisted_message_index,
        input_tokens: total.input_tokens,
        output_tokens: total.output_tokens,
        cache_read_tokens: total.cache_read_tokens,
        cache_write_tokens: total.cache_write_tokens,
        duration_ms,
        request_input_tokens: request.input_tokens,
        request_output_tokens: request.output_tokens,
        request_cache_read_tokens: request.cache_read_tokens,
        request_cache_write_tokens: request.cache_write_tokens,
        audio_path,
        reasoning,
        llm_api_response,
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct SessionTokenUsage {
    pub session_input_tokens: u64,
    pub session_output_tokens: u64,
    pub session_cache_read_tokens: u64,
    pub session_cache_write_tokens: u64,
    pub current_request_input_tokens: u64,
    pub current_request_output_tokens: u64,
    pub current_request_cache_read_tokens: u64,
    pub current_request_cache_write_tokens: u64,
}

#[must_use]
pub(crate) fn session_token_usage_from_messages(messages: &[Value]) -> SessionTokenUsage {
    let session_input_tokens = messages
        .iter()
        .filter_map(|m| m.get("inputTokens").and_then(|v| v.as_u64()))
        .sum();
    let session_output_tokens = messages
        .iter()
        .filter_map(|m| m.get("outputTokens").and_then(|v| v.as_u64()))
        .sum();
    let session_cache_read_tokens = messages
        .iter()
        .filter_map(|m| m.get("cacheReadTokens").and_then(|v| v.as_u64()))
        .sum();
    let session_cache_write_tokens = messages
        .iter()
        .filter_map(|m| m.get("cacheWriteTokens").and_then(|v| v.as_u64()))
        .sum();

    let (
        current_request_input_tokens,
        current_request_output_tokens,
        current_request_cache_read_tokens,
        current_request_cache_write_tokens,
    ) = messages
        .iter()
        .rev()
        .find(|m| m.get("role").and_then(|v| v.as_str()) == Some("assistant"))
        .map_or((0, 0, 0, 0), |m| {
            let input = m
                .get("requestInputTokens")
                .and_then(|v| v.as_u64())
                .or_else(|| m.get("inputTokens").and_then(|v| v.as_u64()))
                .unwrap_or(0);
            let output = m
                .get("requestOutputTokens")
                .and_then(|v| v.as_u64())
                .or_else(|| m.get("outputTokens").and_then(|v| v.as_u64()))
                .unwrap_or(0);
            let cache_read = m
                .get("requestCacheReadTokens")
                .and_then(|v| v.as_u64())
                .or_else(|| m.get("cacheReadTokens").and_then(|v| v.as_u64()))
                .unwrap_or(0);
            let cache_write = m
                .get("requestCacheWriteTokens")
                .and_then(|v| v.as_u64())
                .or_else(|| m.get("cacheWriteTokens").and_then(|v| v.as_u64()))
                .unwrap_or(0);
            (input, output, cache_read, cache_write)
        });

    SessionTokenUsage {
        session_input_tokens,
        session_output_tokens,
        session_cache_read_tokens,
        session_cache_write_tokens,
        current_request_input_tokens,
        current_request_output_tokens,
        current_request_cache_read_tokens,
        current_request_cache_write_tokens,
    }
}

#[cfg(test)]
mod tests {
    use {
        super::{
            ReplyMedium, UsageSnapshot, assistant_message_is_visible, build_assistant_turn_output,
            build_chat_final_broadcast, session_token_usage_from_messages,
        },
        chelix_agents::model::Usage,
    };

    #[test]
    fn assistant_message_with_tool_calls_is_visible() {
        let msg = serde_json::json!({
            "role": "assistant",
            "content": "",
            "tool_calls": [{"id": "call_1", "type": "function", "function": {"name": "execute_command", "arguments": "{}"}}]
        });
        assert!(assistant_message_is_visible(&msg));
    }

    #[test]
    fn empty_assistant_message_without_tool_calls_is_hidden() {
        let msg = serde_json::json!({
            "role": "assistant",
            "content": "",
        });
        assert!(!assistant_message_is_visible(&msg));
    }

    #[test]
    fn session_token_usage_tracks_cached_tokens() {
        let messages = vec![
            serde_json::json!({
                "role": "assistant",
                "inputTokens": 200,
                "outputTokens": 20,
                "cacheReadTokens": 150,
                "cacheWriteTokens": 10,
                "requestInputTokens": 180,
                "requestOutputTokens": 18,
                "requestCacheReadTokens": 140,
                "requestCacheWriteTokens": 8
            }),
            serde_json::json!({
                "role": "assistant",
                "inputTokens": 300,
                "outputTokens": 30,
                "cacheReadTokens": 120,
                "cacheWriteTokens": 5,
                "requestInputTokens": 250,
                "requestOutputTokens": 25,
                "requestCacheReadTokens": 100,
                "requestCacheWriteTokens": 2
            }),
        ];

        let usage = session_token_usage_from_messages(&messages);

        assert_eq!(usage.session_input_tokens, 500);
        assert_eq!(usage.session_output_tokens, 50);
        assert_eq!(usage.session_cache_read_tokens, 270);
        assert_eq!(usage.session_cache_write_tokens, 15);
        assert_eq!(usage.current_request_input_tokens, 250);
        assert_eq!(usage.current_request_output_tokens, 25);
        assert_eq!(usage.current_request_cache_read_tokens, 100);
        assert_eq!(usage.current_request_cache_write_tokens, 2);
    }

    #[test]
    fn build_chat_final_broadcast_includes_cache_usage() {
        let usage = Usage {
            input_tokens: 1200,
            output_tokens: 80,
            cache_read_tokens: 1050,
            cache_write_tokens: 4,
        };
        let request_usage = Usage {
            input_tokens: 900,
            output_tokens: 60,
            cache_read_tokens: 850,
            cache_write_tokens: 2,
        };

        let payload = build_chat_final_broadcast(
            "run-1",
            "main",
            "hello".to_string(),
            "gpt-4.1".to_string(),
            "openai".to_string(),
            Some("high".to_string()),
            UsageSnapshot::new(usage, Some(request_usage)),
            250,
            Some(7),
            ReplyMedium::Text,
            Some(2),
            Some(1),
            None,
            None,
            Some("thinking".to_string()),
            Some(42),
        );

        assert_eq!(payload.cache_read_tokens, 1050);
        assert_eq!(payload.cache_write_tokens, 4);
        assert_eq!(payload.request_cache_read_tokens, Some(850));
        assert_eq!(payload.request_cache_write_tokens, Some(2));
        assert_eq!(payload.message_index, Some(7));
        assert_eq!(payload.seq, Some(42));
    }

    #[test]
    fn build_assistant_turn_output_copies_cache_usage() {
        let output = build_assistant_turn_output(
            "hello".to_string(),
            None,
            UsageSnapshot::new(
                Usage {
                    input_tokens: 1200,
                    output_tokens: 80,
                    cache_read_tokens: 1050,
                    cache_write_tokens: 4,
                },
                Some(Usage {
                    input_tokens: 900,
                    output_tokens: 60,
                    cache_read_tokens: 850,
                    cache_write_tokens: 2,
                }),
            ),
            250,
            None,
            Some("thinking".to_string()),
            None,
        );

        assert_eq!(output.cache_read_tokens, 1050);
        assert_eq!(output.cache_write_tokens, 4);
        assert_eq!(output.request_cache_read_tokens, 850);
        assert_eq!(output.request_cache_write_tokens, 2);
    }
}

#[must_use]
pub(crate) fn assistant_message_is_visible(message: &Value) -> bool {
    if message.get("role").and_then(Value::as_str) != Some("assistant") {
        return true;
    }

    // Keep assistant messages that carry tool calls (even with empty content).
    if message
        .get("tool_calls")
        .and_then(Value::as_array)
        .is_some_and(|arr| !arr.is_empty())
    {
        return true;
    }

    ["content", "reasoning"].iter().any(|field| {
        message
            .get(*field)
            .and_then(Value::as_str)
            .is_some_and(|text| !text.trim().is_empty())
    })
}

pub(crate) fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

#[must_use]
pub(crate) fn truncate_at_char_boundary(text: &str, max_bytes: usize) -> &str {
    &text[..text.floor_char_boundary(max_bytes)]
}

/// Extract preview text from a single message JSON value.
pub(crate) fn extract_preview_from_value(msg: &Value) -> Option<String> {
    fn message_text(msg: &Value) -> Option<String> {
        let content = msg.get("content")?;
        let text = if let Some(s) = content.as_str() {
            s.to_string()
        } else {
            let blocks = content.as_array()?;
            blocks
                .iter()
                .filter_map(|b| {
                    if b.get("type").and_then(|v| v.as_str()) == Some("text") {
                        b.get("text").and_then(|v| v.as_str())
                    } else {
                        None
                    }
                })
                .collect::<Vec<_>>()
                .join(" ")
        };
        let trimmed = text.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.to_string())
        }
    }
    fn truncate_preview(s: &str, max: usize) -> String {
        if s.len() <= max {
            s.to_string()
        } else {
            format!("{}…", &s[..s.floor_char_boundary(max)])
        }
    }
    message_text(msg).map(|t| truncate_preview(&t, 200))
}

pub(crate) fn shell_reply_text_from_command_result(result: &Value) -> String {
    let stdout = result
        .get("stdout")
        .and_then(Value::as_str)
        .map(str::trim_end)
        .unwrap_or("");
    if !stdout.is_empty() {
        return stdout.to_string();
    }

    let output = result
        .get("output")
        .and_then(Value::as_str)
        .map(str::trim_end)
        .unwrap_or("");
    if !output.is_empty() {
        return output.to_string();
    }

    let stderr = result
        .get("stderr")
        .and_then(Value::as_str)
        .map(str::trim_end)
        .unwrap_or("");
    if !stderr.is_empty() {
        return stderr.to_string();
    }

    let exit_code = result
        .get("exit_code")
        .or_else(|| result.get("exitCode"))
        .and_then(Value::as_i64)
        .or_else(|| {
            result
                .get("exit_code")
                .or_else(|| result.get("exitCode"))
                .and_then(Value::as_u64)
                .and_then(|code| i64::try_from(code).ok())
        });
    match exit_code {
        Some(code) if code != 0 => format!("Command failed (exit {code})."),
        _ => String::new(),
    }
}

pub fn normalize_model_key(value: &str) -> String {
    let mut normalized = String::with_capacity(value.len());
    let mut last_was_separator = true;

    for ch in value.trim().chars() {
        if ch.is_ascii_alphanumeric() {
            normalized.push(ch.to_ascii_lowercase());
            last_was_separator = false;
            continue;
        }

        if !last_was_separator {
            normalized.push(' ');
            last_was_separator = true;
        }
    }

    normalized.trim().to_string()
}

pub(crate) fn normalize_provider_key(value: &str) -> String {
    value.trim().to_ascii_lowercase()
}

pub(crate) fn subscription_provider_rank(provider_name: &str) -> usize {
    match normalize_provider_key(provider_name).as_str() {
        "openai-codex" | "github-copilot" => 0,
        _ => 1,
    }
}

/// Returns `true` if the model matches the allowlist patterns.
/// An empty pattern list means all models are allowed.
/// Matching is case-insensitive against the full model ID, raw model ID, and
/// display name:
/// - patterns with digits use exact-or-suffix matching (boundary aware)
/// - patterns without digits use substring matching
///
/// This keeps precise model pins like "gpt 5.2" from matching variants such as
/// "gpt-5.2-chat-latest", while still allowing broad buckets like "mini".
#[allow(dead_code)]
pub(crate) fn allowlist_pattern_matches_key(pattern: &str, key: &str) -> bool {
    if pattern.chars().any(|ch| ch.is_ascii_digit()) {
        if key == pattern {
            return true;
        }
        return key
            .strip_suffix(pattern)
            .is_some_and(|prefix| prefix.ends_with(' '));
    }
    key.contains(pattern)
}

#[allow(dead_code)]
pub fn model_matches_allowlist(model: &chelix_providers::ModelInfo, patterns: &[String]) -> bool {
    if patterns.is_empty() {
        return true;
    }
    let full = normalize_model_key(&model.id);
    let raw = normalize_model_key(chelix_providers::model_id::raw_model_id(&model.id));
    let display = normalize_model_key(&model.display_name);
    patterns.iter().any(|p| {
        allowlist_pattern_matches_key(p, &full)
            || allowlist_pattern_matches_key(p, &raw)
            || allowlist_pattern_matches_key(p, &display)
    })
}

pub(crate) fn provider_filter_from_params(params: &Value) -> Option<String> {
    params
        .get("provider")
        .and_then(|v| v.as_str())
        .map(normalize_provider_key)
        .filter(|v| !v.is_empty())
}

pub(crate) fn provider_matches_filter(model_provider: &str, provider_filter: Option<&str>) -> bool {
    provider_filter.is_none_or(|expected| normalize_provider_key(model_provider) == expected)
}

pub(crate) fn probe_max_parallel_per_provider(params: &Value) -> usize {
    params
        .get("maxParallelPerProvider")
        .and_then(|v| v.as_u64())
        .map(|v| v.clamp(1, 8) as usize)
        .unwrap_or(1)
}

pub(crate) fn provider_model_entry(model_id: &str, display_name: &str) -> Value {
    serde_json::json!({
        "modelId": model_id,
        "displayName": display_name,
    })
}

pub(crate) fn push_provider_model(
    grouped: &mut std::collections::BTreeMap<String, Vec<Value>>,
    provider_name: &str,
    model_id: &str,
    display_name: &str,
) {
    if provider_name.trim().is_empty() || model_id.trim().is_empty() {
        return;
    }
    grouped
        .entry(provider_name.to_string())
        .or_default()
        .push(provider_model_entry(model_id, display_name));
}

pub(crate) fn is_safe_user_audio_filename(filename: &str) -> bool {
    !filename.is_empty()
        && filename.len() <= 255
        && filename
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' || ch == '.')
}

pub(crate) fn sanitize_user_document_display_name(name: &str) -> Option<String> {
    let trimmed = name.trim();
    if trimmed.is_empty() || trimmed.len() > 255 || trimmed.chars().any(char::is_control) {
        None
    } else {
        Some(trimmed.to_string())
    }
}

pub(crate) fn parse_explicit_shell_command(text: &str) -> Option<&str> {
    let trimmed = text.trim_start();
    let rest = trimmed.strip_prefix("/sh")?;
    let first = rest.chars().next()?;
    if !first.is_whitespace() {
        return None;
    }
    let command = &rest[first.len_utf8()..];
    if command.trim().is_empty() {
        None
    } else {
        Some(command)
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PromptMemoryStatus {
    pub style: MemoryStyle,
    pub mode: PromptMemoryMode,
    pub write_mode: AgentMemoryWriteMode,
    pub snapshot_active: bool,
    pub present: bool,
    pub chars: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file_source: Option<chelix_config::WorkspaceMarkdownSource>,
}

/// Pre-loaded persona data used to build the system prompt.
pub(crate) struct PromptPersona {
    pub config: chelix_config::ChelixConfig,
    pub identity: chelix_config::AgentIdentity,
    pub user: chelix_config::UserProfile,
    pub soul_text: Option<String>,
    pub boot_text: Option<String>,
    pub agents_text: Option<String>,
    pub tools_text: Option<String>,
    pub guidelines_text: Option<String>,
    pub memory_text: Option<String>,
    pub memory_status: PromptMemoryStatus,
}

/// Compatibility shim: delegates to [`ChatRuntime::broadcast`].
///
/// Matches the old `broadcast(state, topic, payload, opts)` signature so that
/// the hundreds of call sites inside this crate need no change.
pub(crate) async fn broadcast(
    state: &std::sync::Arc<dyn crate::runtime::ChatRuntime>,
    event: &str,
    payload: Value,
    _opts: BroadcastOpts,
) {
    state.broadcast(event, payload).await;
}

#[cfg(feature = "metrics")]
pub(crate) use chelix_metrics::{counter, histogram, labels, llm as llm_metrics};

/// Detect the current user's runtime shell from environment variables.
pub(crate) fn detect_runtime_shell() -> Option<String> {
    use std::{ffi::OsStr, path::Path};
    let candidate = std::env::var("SHELL")
        .ok()
        .or_else(|| std::env::var("COMSPEC").ok())?;
    let trimmed = candidate.trim();
    if trimmed.is_empty() {
        return None;
    }
    let name = Path::new(trimmed)
        .file_name()
        .and_then(OsStr::to_str)
        .unwrap_or(trimmed)
        .trim();
    if name.is_empty() {
        None
    } else {
        Some(name.to_string())
    }
}

pub(crate) async fn detect_host_sudo_access() -> (Option<bool>, Option<String>) {
    #[cfg(not(unix))]
    {
        return (None, Some("unsupported".to_string()));
    }

    #[cfg(unix)]
    {
        use std::process::Stdio;
        let output = match tokio::time::timeout(
            std::time::Duration::from_millis(150),
            tokio::process::Command::new("sudo")
                .arg("-n")
                .arg("true")
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::piped())
                .output(),
        )
        .await
        {
            Ok(output) => output,
            Err(_) => {
                tracing::info!("runtime sudo detection timed out");
                return (None, Some("timeout".to_string()));
            },
        };

        match output {
            Ok(out) if out.status.success() => (Some(true), Some("passwordless".to_string())),
            Ok(out) => {
                let stderr = String::from_utf8_lossy(&out.stderr).to_lowercase();
                if stderr.contains("a password is required") {
                    (Some(false), Some("requires_password".to_string()))
                } else if stderr.contains("not in the sudoers")
                    || stderr.contains("is not in the sudoers")
                    || stderr.contains("is not allowed to run sudo")
                    || stderr.contains("may not run sudo")
                {
                    (Some(false), Some("denied".to_string()))
                } else {
                    (None, Some("unknown".to_string()))
                }
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                (None, Some("not_installed".to_string()))
            },
            Err(_) => (None, Some("unknown".to_string())),
        }
    }
}

pub(crate) async fn detect_host_root_user() -> Option<bool> {
    #[cfg(not(unix))]
    {
        return None;
    }

    #[cfg(unix)]
    {
        use std::process::Stdio;
        if let Some(uid) = std::env::var("EUID")
            .ok()
            .or_else(|| std::env::var("UID").ok())
            .and_then(|raw| raw.trim().parse::<u32>().ok())
        {
            return Some(uid == 0);
        }
        if let Ok(user) = std::env::var("USER") {
            let trimmed = user.trim();
            if !trimmed.is_empty() {
                return Some(trimmed == "root");
            }
        }
        let output = tokio::process::Command::new("id")
            .arg("-u")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .output()
            .await
            .ok()?;
        if !output.status.success() {
            return None;
        }
        let uid_text = String::from_utf8_lossy(&output.stdout);
        uid_text.trim().parse::<u32>().ok().map(|uid| uid == 0)
    }
}

pub(crate) fn normalized_iana_timezone(timezone: Option<&str>) -> Option<String> {
    timezone
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .and_then(|value| value.parse::<chrono_tz::Tz>().ok())
        .map(|tz| tz.to_string())
}

pub(crate) fn server_prompt_timezone(configured_timezone: Option<&str>) -> String {
    if let Some(timezone) = configured_timezone
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        return timezone.to_string();
    }
    "server-local".to_string()
}

pub(crate) fn prompt_now_for_timezone(timezone: Option<&str>) -> String {
    #[cfg(any(feature = "web-ui", feature = "push-notifications"))]
    {
        use chrono::{Local, Utc};

        let trimmed_timezone = timezone.map(str::trim).filter(|value| !value.is_empty());

        if let Some(tz) = trimmed_timezone.and_then(|name| name.parse::<chrono_tz::Tz>().ok()) {
            return Utc::now()
                .with_timezone(&tz)
                .format("%Y-%m-%d %H:%M:%S %Z")
                .to_string();
        }

        // Fallback to server local clock when timezone is unknown/non-IANA.
        Local::now().format("%Y-%m-%d %H:%M:%S %Z").to_string()
    }

    #[cfg(not(any(feature = "web-ui", feature = "push-notifications")))]
    {
        let unix_secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let tz = timezone.unwrap_or("server-local");
        format!("unix={unix_secs} {tz}")
    }
}

pub(crate) fn prompt_today_for_timezone(timezone: Option<&str>) -> String {
    #[cfg(any(feature = "web-ui", feature = "push-notifications"))]
    {
        use chrono::{Local, Utc};

        let trimmed_timezone = timezone.map(str::trim).filter(|value| !value.is_empty());

        if let Some(tz) = trimmed_timezone.and_then(|name| name.parse::<chrono_tz::Tz>().ok()) {
            return Utc::now().with_timezone(&tz).format("%Y-%m-%d").to_string();
        }

        Local::now().format("%Y-%m-%d").to_string()
    }

    #[cfg(not(any(feature = "web-ui", feature = "push-notifications")))]
    {
        let _ = timezone;
        let unix_days = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs()
            / 86_400;
        format!("unix-day={unix_days}")
    }
}

pub(crate) fn refresh_runtime_prompt_time(
    host: &mut chelix_agents::prompt::PromptHostRuntimeContext,
) {
    host.time = Some(prompt_now_for_timezone(host.timezone.as_deref()));
    host.today = Some(prompt_today_for_timezone(host.timezone.as_deref()));
}

pub fn memory_write_mode_allows_save(mode: AgentMemoryWriteMode) -> bool {
    !matches!(mode, AgentMemoryWriteMode::Off)
}

pub(crate) fn default_agent_memory_file_for_mode(mode: AgentMemoryWriteMode) -> &'static str {
    match mode {
        AgentMemoryWriteMode::SearchOnly => "memory/notes.md",
        AgentMemoryWriteMode::Hybrid
        | AgentMemoryWriteMode::PromptOnly
        | AgentMemoryWriteMode::Off => "MEMORY.md",
    }
}

pub(crate) fn memory_style_allows_prompt(style: MemoryStyle) -> bool {
    matches!(style, MemoryStyle::Hybrid | MemoryStyle::PromptOnly)
}

pub(crate) fn memory_style_allows_tools(style: MemoryStyle) -> bool {
    matches!(style, MemoryStyle::Hybrid | MemoryStyle::SearchOnly)
}

pub(crate) fn is_prompt_memory_file(file: &str) -> bool {
    matches!(file.trim(), "MEMORY.md" | "memory.md")
}

pub(crate) fn validate_agent_memory_target_for_mode(
    mode: AgentMemoryWriteMode,
    file: &str,
) -> anyhow::Result<()> {
    match mode {
        AgentMemoryWriteMode::Hybrid => Ok(()),
        AgentMemoryWriteMode::PromptOnly => {
            if is_prompt_memory_file(file) {
                Ok(())
            } else {
                anyhow::bail!(
                    "memory.agent_write_mode = \"prompt-only\" only allows MEMORY.md writes"
                );
            }
        },
        AgentMemoryWriteMode::SearchOnly => {
            if is_prompt_memory_file(file) {
                anyhow::bail!(
                    "memory.agent_write_mode = \"search-only\" only allows memory/<name>.md writes"
                );
            }
            Ok(())
        },
        AgentMemoryWriteMode::Off => {
            anyhow::bail!("agent-authored memory writes are disabled by memory.agent_write_mode");
        },
    }
}

/// Normalize a model lookup key by stripping non-alphanumeric characters and
/// lowercasing.
pub(crate) fn normalize_model_lookup_key(value: &str) -> String {
    value
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .flat_map(char::to_lowercase)
        .collect()
}

pub(crate) fn model_id_provider(model_id: &str) -> Option<&str> {
    model_id.split_once("::").map(|(provider, _)| provider)
}

pub(crate) fn levenshtein_distance(a: &str, b: &str) -> usize {
    if a.is_empty() {
        return b.chars().count();
    }
    if b.is_empty() {
        return a.chars().count();
    }

    let b_chars: Vec<char> = b.chars().collect();
    let a_chars: Vec<char> = a.chars().collect();
    let mut prev: Vec<usize> = (0..=b_chars.len()).collect();
    let mut curr = vec![0; b_chars.len() + 1];

    for (i, a_ch) in a_chars.iter().enumerate() {
        curr[0] = i + 1;
        for (j, b_ch) in b_chars.iter().enumerate() {
            let cost = usize::from(a_ch != b_ch);
            let deletion = prev[j + 1] + 1;
            let insertion = curr[j] + 1;
            let substitution = prev[j] + cost;
            curr[j + 1] = deletion.min(insertion).min(substitution);
        }
        std::mem::swap(&mut prev, &mut curr);
    }

    prev[b_chars.len()]
}

pub(crate) fn suggest_model_ids(
    requested_model_id: &str,
    available_model_ids: &[String],
    limit: usize,
) -> Vec<String> {
    if requested_model_id.trim().is_empty() || available_model_ids.is_empty() || limit == 0 {
        return Vec::new();
    }

    let requested_provider = model_id_provider(requested_model_id).map(str::to_ascii_lowercase);
    let requested_raw = chelix_providers::model_id::raw_model_id(requested_model_id);
    let requested_norm = normalize_model_lookup_key(requested_model_id);
    let requested_raw_norm = normalize_model_lookup_key(requested_raw);

    let mut ranked: Vec<(String, bool, usize, usize, usize)> = available_model_ids
        .iter()
        .filter_map(|candidate| {
            let candidate_provider = model_id_provider(candidate).map(str::to_ascii_lowercase);
            let provider_match = requested_provider
                .as_deref()
                .zip(candidate_provider.as_deref())
                .is_some_and(|(left, right)| left == right);

            let candidate_raw = chelix_providers::model_id::raw_model_id(candidate);
            let candidate_norm = normalize_model_lookup_key(candidate);
            let candidate_raw_norm = normalize_model_lookup_key(candidate_raw);

            let raw_distance = levenshtein_distance(&requested_raw_norm, &candidate_raw_norm);
            let full_distance = levenshtein_distance(&requested_norm, &candidate_norm);
            let contains = requested_raw_norm.contains(&candidate_raw_norm)
                || candidate_raw_norm.contains(&requested_raw_norm)
                || requested_norm.contains(&candidate_norm)
                || candidate_norm.contains(&requested_raw_norm);

            // Keep nearest neighbors and strong substring matches. This trims
            // unrelated model IDs from suggestion logs/responses.
            let distance_cap = requested_raw_norm
                .len()
                .max(candidate_raw_norm.len())
                .max(3)
                / 2
                + 2;
            if !contains && raw_distance > distance_cap {
                return None;
            }

            Some((
                candidate.clone(),
                provider_match,
                raw_distance,
                full_distance,
                requested_raw_norm.len().abs_diff(candidate_raw_norm.len()),
            ))
        })
        .collect();

    ranked.sort_by(|left, right| {
        right
            .1
            .cmp(&left.1) // provider match first
            .then(left.2.cmp(&right.2)) // nearest raw model id
            .then(left.3.cmp(&right.3)) // nearest full model id
            .then(left.4.cmp(&right.4)) // similar length
            .then(left.0.cmp(&right.0))
    });

    ranked.into_iter().map(|(id, ..)| id).take(limit).collect()
}
