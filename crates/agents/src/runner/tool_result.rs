//! Tool result handling: blob stripping, disk persistence, and truncation
//! with a pointer to the persisted full output.
//!
//! Every tool call's full output is persisted via
//! [`chelix_sessions::ToolResultStore`]. When the in-context copy exceeds the
//! configured byte budget it is truncated and a marker pointing at the
//! persisted `content.txt`/`content.json` file is appended, so the agent can
//! re-read the full result with Read/Grep. Modeled on VS Code Copilot Chat's
//! large-tool-results-to-disk mechanism.

use std::fmt::Write;

use {
    crate::tool_registry::{ToolResultPersistence, Truncation},
    chelix_sessions::{PersistedToolResult, ToolResultStore},
};

/// Tag that starts a base64 data URI.
const BASE64_TAG: &str = "data:";
/// Marker between MIME type and base64 payload.
const BASE64_MARKER: &str = ";base64,";
/// Minimum length of a blob payload (base64 or hex) to be worth stripping.
const BLOB_MIN_LEN: usize = 200;

fn is_base64_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'+' || b == b'/' || b == b'='
}

/// Strip base64 data-URI blobs (e.g. `data:image/png;base64,AAAA...`) and
/// replace them with a short placeholder. Only targets payloads >= 200 chars.
fn strip_base64_blobs(input: &str) -> String {
    let mut result = String::with_capacity(input.len());
    let mut rest = input;

    while let Some(start) = rest.find(BASE64_TAG) {
        result.push_str(&rest[..start]);
        let after_tag = &rest[start + BASE64_TAG.len()..];

        if let Some(marker_pos) = after_tag.find(BASE64_MARKER) {
            let mime_part = &after_tag[..marker_pos];
            let payload_start = marker_pos + BASE64_MARKER.len();
            let payload = &after_tag[payload_start..];
            let payload_len = payload.bytes().take_while(|b| is_base64_byte(*b)).count();

            if payload_len >= BLOB_MIN_LEN {
                let total_uri_len = BASE64_TAG.len() + payload_start + payload_len;
                // Provide a descriptive message based on MIME type
                if mime_part.starts_with("image/") {
                    result.push_str("[screenshot captured and displayed in UI]");
                } else {
                    let _ = write!(result, "[{mime_part} data removed — {total_uri_len} bytes]");
                }
                rest = &rest[start + total_uri_len..];
                continue;
            }
        }

        result.push_str(BASE64_TAG);
        rest = after_tag;
    }
    result.push_str(rest);
    result
}

/// Strip long hex sequences (>= 200 hex chars) that look like binary dumps.
fn strip_hex_blobs(input: &str) -> String {
    let mut result = String::with_capacity(input.len());
    let mut chars = input.char_indices().peekable();

    while let Some(&(start, ch)) = chars.peek() {
        if ch.is_ascii_hexdigit() {
            let mut end = start;
            while let Some(&(i, c)) = chars.peek() {
                if c.is_ascii_hexdigit() {
                    end = i + c.len_utf8();
                    chars.next();
                } else {
                    break;
                }
            }
            let run = end - start;
            if run >= BLOB_MIN_LEN {
                let _ = write!(result, "[hex data removed — {run} chars]");
            } else {
                result.push_str(&input[start..end]);
            }
        } else {
            result.push(ch);
            chars.next();
        }
    }
    result
}

/// Sanitize a tool result string before feeding it to the LLM.
///
/// 1. Strips base64 data URIs (>= 200 char payloads).
/// 2. Strips long hex sequences (>= 200 hex chars).
#[must_use]
pub fn sanitize_tool_result(input: &str) -> String {
    strip_hex_blobs(&strip_base64_blobs(input))
}

/// Truncate `result` to `max_bytes` at a char boundary.
fn truncate_at_char_boundary(result: &mut String, max_bytes: usize) {
    let mut end = max_bytes;
    while end > 0 && !result.is_char_boundary(end) {
        end -= 1;
    }
    result.truncate(end);
}

/// Persist the full tool output and build the in-context copy.
///
/// The raw output is always written to the session's tool-results directory
/// (when a store is available). The in-context copy is blob-stripped and,
/// when it exceeds `max_bytes` and `truncation` is [`Truncation::Standard`],
/// truncated with an appended marker pointing at the persisted full output.
pub async fn persist_and_truncate(
    store: &ToolResultStore,
    session_key: &str,
    call_id: &str,
    result_value: &serde_json::Value,
    max_bytes: usize,
    truncation: Truncation,
    persistence: ToolResultPersistence,
) -> anyhow::Result<String> {
    let raw = result_value
        .as_str()
        .map_or_else(|| result_value.to_string(), str::to_string);
    // Persistence is mandatory: the in-context pointer must always resolve.
    let persisted = persist_result(store, session_key, call_id, result_value, persistence).await?;

    let mut result = sanitize_tool_result(&raw);
    if truncation == Truncation::Off || result.len() <= max_bytes {
        return Ok(result);
    }

    truncate_at_char_boundary(&mut result, max_bytes);
    append_full_output_pointer(&mut result, &persisted);
    Ok(result)
}

async fn persist_result(
    store: &ToolResultStore,
    session_key: &str,
    call_id: &str,
    result: &serde_json::Value,
    persistence: ToolResultPersistence,
) -> chelix_sessions::Result<PersistedToolResult> {
    match persistence {
        ToolResultPersistence::Structured => {
            store
                .persist(session_key, call_id, &result.to_string())
                .await
        },
        ToolResultPersistence::TextFields(fields) => {
            let content = render_text_result(result, fields)?;
            store.persist_text(session_key, call_id, &content).await
        },
    }
}

fn render_text_result(
    result: &serde_json::Value,
    text_fields: &[&str],
) -> chelix_sessions::Result<String> {
    let Some(object) = result.as_object() else {
        return Ok(result.as_str().unwrap_or_default().to_string());
    };

    let metadata = object
        .iter()
        .filter(|(key, _)| !text_fields.contains(&key.as_str()))
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect::<serde_json::Map<_, _>>();
    let mut sections = Vec::new();
    if !metadata.is_empty() {
        sections.push(serde_json::to_string_pretty(&metadata)?);
    }
    sections.extend(text_fields.iter().filter_map(|field| {
        object
            .get(*field)
            .and_then(serde_json::Value::as_str)
            .filter(|content| !content.is_empty())
            .map(|content| format!("[{field}]\n{content}"))
    }));
    Ok(sections.join("\n\n"))
}

/// Append the marker pointing the agent at the persisted full output.
fn append_full_output_pointer(result: &mut String, persisted: &PersistedToolResult) {
    let kb = persisted.content_bytes.div_ceil(1024);
    let _ = write!(
        result,
        "\n\n[Truncated — full tool result ({kb}KB) written to file. Use the Read tool to access \
         the content at: {}]",
        persisted.content_path.display()
    );
    if let Some(schema_path) = &persisted.schema_path {
        let _ = write!(
            result,
            "\n[Data schema found at: {}]",
            schema_path.display()
        );
    }
}
