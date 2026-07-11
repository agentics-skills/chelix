//! Tests for full tool-output persistence and in-context truncation with
//! a pointer to the persisted file.

use chelix_sessions::ToolResultStore;

use super::helpers::*;

fn store_in(dir: &tempfile::TempDir) -> ToolResultStore {
    ToolResultStore::new(dir.path().to_path_buf())
}

// ── persist_and_truncate ────────────────────────────────────────────────

#[tokio::test]
async fn small_result_is_persisted_but_not_truncated() {
    let dir = tempfile::tempdir().unwrap();
    let store = store_in(&dir);

    let result = persist_and_truncate(
        &store,
        "chat:main",
        "call_1",
        &serde_json::json!("short output"),
        50_000,
        Truncation::Standard,
        ToolResultPersistence::Structured,
    )
    .await
    .unwrap();

    assert_eq!(result, "short output");
    // Full output persisted even below the truncation budget.
    let content_path = dir
        .path()
        .join("tool-results")
        .join("chat_main")
        .join("call_1")
        .join("content.txt");
    assert_eq!(
        std::fs::read_to_string(content_path).unwrap(),
        "short output"
    );
}

#[tokio::test]
async fn oversized_result_is_truncated_with_pointer_to_file() {
    let dir = tempfile::tempdir().unwrap();
    let store = store_in(&dir);
    let raw = "x".repeat(1000);

    let result = persist_and_truncate(
        &store,
        "chat:main",
        "call_2",
        &serde_json::json!(raw),
        100,
        Truncation::Standard,
        ToolResultPersistence::Structured,
    )
    .await
    .unwrap();

    assert!(result.starts_with("xxxx"));
    assert!(result.contains("[Truncated — full tool result (1KB) written to file."));
    assert!(result.contains("Use the Read tool to access the content at:"));
    let content_path = dir
        .path()
        .join("tool-results")
        .join("chat_main")
        .join("call_2")
        .join("content.txt");
    assert!(result.contains(content_path.to_str().unwrap()));
    assert_eq!(std::fs::read_to_string(content_path).unwrap(), raw);
}

#[tokio::test]
async fn oversized_json_result_mentions_schema_file() {
    let dir = tempfile::tempdir().unwrap();
    let store = store_in(&dir);
    let raw = format!(r#"{{"stdout":"{}"}}"#, "y".repeat(1000));

    let result = persist_and_truncate(
        &store,
        "chat:main",
        "call_3",
        &serde_json::from_str(&raw).unwrap(),
        100,
        Truncation::Standard,
        ToolResultPersistence::Structured,
    )
    .await
    .unwrap();

    assert!(result.contains("content.json"));
    assert!(result.contains("[Data schema found at:"));
    let schema_path = dir
        .path()
        .join("tool-results")
        .join("chat_main")
        .join("call_3")
        .join("schema.json");
    assert!(result.contains(schema_path.to_str().unwrap()));
    assert!(schema_path.exists());
}

#[tokio::test]
async fn truncation_off_keeps_full_result_in_context() {
    let dir = tempfile::tempdir().unwrap();
    let store = store_in(&dir);
    let raw = "z".repeat(1000);

    let result = persist_and_truncate(
        &store,
        "chat:main",
        "call_4",
        &serde_json::json!(raw),
        100,
        Truncation::Off,
        ToolResultPersistence::Structured,
    )
    .await
    .unwrap();

    assert_eq!(result, raw, "Truncation::Off must never truncate");
    // Still persisted to disk.
    let content_path = dir
        .path()
        .join("tool-results")
        .join("chat_main")
        .join("call_4")
        .join("content.txt");
    assert!(content_path.exists());
}

#[tokio::test]
async fn truncation_respects_char_boundary() {
    let dir = tempfile::tempdir().unwrap();
    let store = store_in(&dir);
    let raw = "é".repeat(100);

    let result = persist_and_truncate(
        &store,
        "chat:main",
        "call_6",
        &serde_json::json!(raw),
        51,
        Truncation::Standard,
        ToolResultPersistence::Structured,
    )
    .await
    .unwrap();

    let prefix_end = result.find("\n\n[Truncated").unwrap();
    assert!(prefix_end <= 51);
    assert_eq!(prefix_end % 2, 0, "must not split a 2-byte char");
}

#[tokio::test]
async fn blob_stripping_applies_to_in_context_copy_only() {
    let dir = tempfile::tempdir().unwrap();
    let store = store_in(&dir);
    let payload = "A".repeat(300);
    let raw = format!("before data:image/png;base64,{payload} after");

    let result = persist_and_truncate(
        &store,
        "chat:main",
        "call_7",
        &serde_json::json!(raw),
        50_000,
        Truncation::Standard,
        ToolResultPersistence::Structured,
    )
    .await
    .unwrap();

    // In-context copy is blob-stripped…
    assert!(result.contains("[screenshot captured and displayed in UI]"));
    assert!(!result.contains(&payload));
    // …while the persisted file keeps the full raw output.
    let content_path = dir
        .path()
        .join("tool-results")
        .join("chat_main")
        .join("call_7")
        .join("content.txt");
    let on_disk = std::fs::read_to_string(content_path).unwrap();
    assert!(on_disk.contains(&payload));
}

#[tokio::test]
async fn terminal_result_is_always_persisted_as_line_oriented_text() {
    let dir = tempfile::tempdir().unwrap();
    let store = store_in(&dir);
    let result_value = serde_json::json!({
        "stdout": "line one\nline two\nline three",
        "stderr": "warning one\nwarning two",
        "exit_code": 7
    });

    let result = persist_and_truncate(
        &store,
        "chat:main",
        "terminal_call",
        &result_value,
        10,
        Truncation::Standard,
        ToolResultPersistence::TextFields(&["stderr", "stdout", "output"]),
    )
    .await
    .unwrap();

    assert!(result.contains("content.txt"));
    assert!(!result.contains("content.json"));
    assert!(!result.contains("schema.json"));
    let content_path = dir
        .path()
        .join("tool-results")
        .join("chat_main")
        .join("terminal_call")
        .join("content.txt");
    let content = std::fs::read_to_string(content_path).unwrap();
    assert!(!content.contains("\"result\""));
    assert!(!content.contains("[truncated —"));
    assert!(content.contains("\"exit_code\": 7"));
    assert!(content.contains("[stderr]\nwarning one\nwarning two"));
    assert!(content.contains("[stdout]\nline one\nline two\nline three"));
    assert_eq!(
        content.lines().filter(|line| *line == "line two").count(),
        1
    );
}

#[tokio::test]
async fn text_field_persistence_does_not_depend_on_result_size() {
    let dir = tempfile::tempdir().unwrap();
    let store = store_in(&dir);
    let result_value = serde_json::json!({"output": "short\noutput", "completed": true});

    let result = persist_and_truncate(
        &store,
        "chat:main",
        "small_terminal_call",
        &result_value,
        50_000,
        Truncation::Standard,
        ToolResultPersistence::TextFields(&["output"]),
    )
    .await
    .unwrap();

    assert!(!result.contains("Truncated"));
    let content_path = dir
        .path()
        .join("tool-results")
        .join("chat_main")
        .join("small_terminal_call")
        .join("content.txt");
    assert!(content_path.exists());
    assert!(!content_path.with_file_name("content.json").exists());
}

// ── Truncation trait hook ───────────────────────────────────────────────

struct NoTruncationTool;

#[async_trait::async_trait]
impl crate::tool_registry::AgentTool for NoTruncationTool {
    fn name(&self) -> &str {
        "no_truncation_tool"
    }

    fn description(&self) -> &str {
        "Returns a large payload that must never be truncated"
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({"type": "object", "properties": {}})
    }

    fn truncation(&self, _params: &serde_json::Value) -> Truncation {
        Truncation::Off
    }

    async fn execute(&self, _params: serde_json::Value) -> anyhow::Result<serde_json::Value> {
        Ok(serde_json::json!({ "stdout": "L".repeat(200) }))
    }
}

#[test]
fn default_truncation_is_standard() {
    let tool = EchoTool;
    use crate::tool_registry::AgentTool as _;
    assert_eq!(
        tool.truncation(&serde_json::json!({})),
        Truncation::Standard
    );
}

#[test]
fn default_persistence_is_structured() {
    let tool = EchoTool;
    use crate::tool_registry::AgentTool as _;
    assert_eq!(
        tool.result_persistence(&serde_json::json!({})),
        ToolResultPersistence::Structured
    );
}

#[test]
fn tool_can_opt_out_of_truncation() {
    use crate::tool_registry::AgentTool as _;
    assert_eq!(
        NoTruncationTool.truncation(&serde_json::json!({})),
        Truncation::Off
    );
}
