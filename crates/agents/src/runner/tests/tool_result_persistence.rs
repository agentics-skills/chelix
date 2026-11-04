//! Tests for full tool-output persistence and in-context truncation with
//! a pointer to the persisted file.

use std::{
    pin::Pin,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
};

use {
    async_trait::async_trait,
    chelix_common::{
        hooks::{HookAction, HookEvent, HookHandler, HookPayload, HookRegistry},
        tool_lifecycle::ToolLifecycleUpdate,
    },
    chelix_sessions::ToolResultStore,
    tokio_stream::Stream,
    tokio_util::sync::CancellationToken,
};

use crate::model::{ChatMessage, CompletionResponse, LlmProvider, StreamEvent, ToolCall, Usage};

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
        ToolResultPersistence::On,
        0,
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
        ToolResultPersistence::On,
        0,
    )
    .await
    .unwrap();

    assert!(result.starts_with("xxxx"));
    assert!(result.contains("[Truncated — full tool result (1KB or 1 lines) written to file."));
    assert!(result.contains("Use the read_file tool to access the content at:"));
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
        ToolResultPersistence::On,
        0,
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
        ToolResultPersistence::On,
        0,
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
        ToolResultPersistence::On,
        0,
    )
    .await
    .unwrap();

    let prefix_end = result.find("\n\n[Truncated").unwrap();
    assert!(prefix_end <= 51);
    assert_eq!(prefix_end % 2, 0, "must not split a 2-byte char");
}

#[tokio::test]
async fn truncated_marker_reports_full_file_line_count() {
    let dir = tempfile::tempdir().unwrap();
    let store = store_in(&dir);
    let raw = "line\n".repeat(40);

    let result = persist_and_truncate(
        &store,
        "chat:main",
        "call_lines",
        &serde_json::json!(raw),
        15,
        Truncation::Standard,
        ToolResultPersistence::On,
        0,
    )
    .await
    .unwrap();

    let marker = result.find("\n\n[Truncated").unwrap();
    assert_eq!(&result[..marker], &raw[..15]);
    assert!(result.contains("[Truncated — full tool result (1KB or 40 lines) written to file."));
    let content_path = dir
        .path()
        .join("tool-results")
        .join("chat_main")
        .join("call_lines")
        .join("content.txt");
    assert_eq!(std::fs::read_to_string(content_path).unwrap(), raw);
}

#[tokio::test]
async fn end_truncation_keeps_suffix() {
    let dir = tempfile::tempdir().unwrap();
    let store = store_in(&dir);
    let raw = format!("{}TAIL", "a".repeat(100));

    let result = persist_and_truncate(
        &store,
        "chat:main",
        "call_end",
        &serde_json::json!(raw),
        4,
        Truncation::End,
        ToolResultPersistence::On,
        0,
    )
    .await
    .unwrap();

    let marker = result.find("\n\n[Truncated").unwrap();
    assert_eq!(&result[..marker], "TAIL");
    assert!(result.contains("[Truncated — full tool result (1KB or 1 lines) written to file."));
    let content_path = dir
        .path()
        .join("tool-results")
        .join("chat_main")
        .join("call_end")
        .join("content.txt");
    assert_eq!(std::fs::read_to_string(content_path).unwrap(), raw);
}

#[tokio::test]
async fn end_truncation_respects_char_boundary() {
    let dir = tempfile::tempdir().unwrap();
    let store = store_in(&dir);
    let raw = "é".repeat(10);

    let result = persist_and_truncate(
        &store,
        "chat:main",
        "call_end_char",
        &serde_json::json!(raw),
        5,
        Truncation::End,
        ToolResultPersistence::On,
        0,
    )
    .await
    .unwrap();

    let marker = result.find("\n\n[Truncated").unwrap();
    assert_eq!(&result[..marker], "éé");
}

#[tokio::test]
async fn end_truncation_keeps_status_prefix_and_output_tail() {
    let dir = tempfile::tempdir().unwrap();
    let store = store_in(&dir);
    let header = "Command finished in terminal (id: 7) with exit code 0.\nOutput:\n";
    let raw = format!("{header}{}TAIL", "a".repeat(300));

    let result = persist_and_truncate(
        &store,
        "chat:main",
        "call_end_status",
        &serde_json::json!(raw),
        header.len() + 4,
        Truncation::End,
        ToolResultPersistence::On,
        header.len(),
    )
    .await
    .unwrap();

    let marker = result.find("\n\n[Truncated").unwrap();
    assert_eq!(&result[..marker], &format!("{header}TAIL"));
    let content_path = dir
        .path()
        .join("tool-results")
        .join("chat_main")
        .join("call_end_status")
        .join("content.txt");
    assert_eq!(std::fs::read_to_string(&content_path).unwrap(), raw);

    let short = persist_and_truncate(
        &store,
        "chat:main",
        "call_end_status_short",
        &serde_json::json!(raw),
        10,
        Truncation::End,
        ToolResultPersistence::On,
        header.len(),
    )
    .await
    .unwrap();
    let short_marker = short.find("\n\n[Truncated").unwrap();
    assert_eq!(&short[..short_marker], &header[..10]);
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
        ToolResultPersistence::On,
        0,
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
async fn agent_facing_text_is_persisted_without_protocol_metadata() {
    let dir = tempfile::tempdir().unwrap();
    let store = store_in(&dir);
    let result_value = serde_json::json!(
        "Command finished in terminal (id: 7).\nOutput:\nline one\nline two\nline three"
    );

    let result = persist_and_truncate(
        &store,
        "chat:main",
        "terminal_call",
        &result_value,
        10,
        Truncation::Standard,
        ToolResultPersistence::On,
        0,
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
    assert_eq!(content, result_value.as_str().unwrap());
}

#[tokio::test]
async fn structured_agent_result_is_persisted_as_json() {
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
        ToolResultPersistence::On,
        0,
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
    assert!(!content_path.exists());
    assert!(content_path.with_file_name("content.json").exists());
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
fn default_persistence_is_enabled() {
    let tool = EchoTool;
    use crate::tool_registry::AgentTool as _;
    assert_eq!(
        tool.result_persistence(&serde_json::json!({})),
        ToolResultPersistence::On
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

#[test]
fn default_in_context_result_bytes_is_none() {
    let tool = EchoTool;
    use crate::tool_registry::AgentTool as _;
    assert_eq!(tool.in_context_result_bytes(&serde_json::json!({})), None);
}

struct EndWindowTool {
    body: String,
    prefix: usize,
}

#[async_trait]
impl crate::tool_registry::AgentTool for EndWindowTool {
    fn name(&self) -> &str {
        "end_window_tool"
    }

    fn description(&self) -> &str {
        "Returns a payload truncated from the end with a preserved status prefix"
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({"type": "object", "properties": {}})
    }

    fn truncation(&self, _params: &serde_json::Value) -> Truncation {
        Truncation::End
    }

    fn in_context_preserved_prefix(
        &self,
        _params: &serde_json::Value,
        _raw_result: &serde_json::Value,
    ) -> usize {
        self.prefix
    }

    async fn execute(&self, _params: serde_json::Value) -> anyhow::Result<serde_json::Value> {
        Ok(serde_json::Value::String(self.body.clone()))
    }
}

struct EndWindowProvider {
    call_count: AtomicUsize,
}

impl LlmProvider for EndWindowProvider {
    fn name(&self) -> &str {
        "end-window"
    }

    fn id(&self) -> &str {
        "end-window-model"
    }

    fn context_window(&self) -> Option<u32> {
        Some(TEST_CONTEXT_WINDOW)
    }

    fn max_input_tokens(&self) -> Option<u32> {
        Some(TEST_MAX_INPUT_TOKENS)
    }

    fn max_output_tokens(&self) -> Option<u32> {
        Some(TEST_MAX_OUTPUT_TOKENS)
    }

    fn supports_tools(&self) -> bool {
        true
    }

    fn stream_with_tools(
        &self,
        _messages: Vec<ChatMessage>,
        _tools: Vec<serde_json::Value>,
    ) -> Pin<Box<dyn Stream<Item = StreamEvent> + Send + '_>> {
        crate::model::response_stream(async move {
            let count = self.call_count.fetch_add(1, Ordering::SeqCst);
            if count == 0 {
                return Ok(CompletionResponse {
                    text: None,
                    tool_calls: vec![ToolCall {
                        id: "call_end_window".into(),
                        name: "end_window_tool".into(),
                        arguments: serde_json::json!({}),
                        argument_diagnostic: None,
                    }],
                    usage: Usage {
                        input_tokens: 10,
                        output_tokens: 5,
                        ..Default::default()
                    },
                    ..Default::default()
                });
            }
            Ok(CompletionResponse {
                text: Some("Done!".into()),
                tool_calls: vec![],
                usage: Usage {
                    input_tokens: 20,
                    output_tokens: 10,
                    ..Default::default()
                },
                ..Default::default()
            })
        })
    }

    fn stream(
        &self,
        messages: Vec<ChatMessage>,
    ) -> Pin<Box<dyn Stream<Item = StreamEvent> + Send + '_>> {
        self.stream_with_tools(messages, Vec::new())
    }
}

struct ReplacePersistResultHook {
    replacement: serde_json::Value,
}

#[async_trait]
impl HookHandler for ReplacePersistResultHook {
    fn name(&self) -> &str {
        "replace-persist-result-hook"
    }

    fn events(&self) -> &[HookEvent] {
        static EVENTS: [HookEvent; 1] = [HookEvent::ToolResultPersist];
        &EVENTS
    }

    async fn handle(
        &self,
        _event: HookEvent,
        _payload: &HookPayload,
    ) -> chelix_common::error::Result<HookAction> {
        Ok(HookAction::ModifyPayload(self.replacement.clone()))
    }
}

async fn run_end_window(hooks: Option<Arc<HookRegistry>>) -> String {
    let header = "Command finished in terminal (id: 7) with exit code 0.\nOutput:\n";
    let mut tools = ToolRegistry::new();
    tools.register(Box::new(EndWindowTool {
        body: format!("{header}{}TAIL", "a".repeat(300)),
        prefix: header.len(),
    }));
    let events = Arc::new(Mutex::new(Vec::new()));
    let on_lifecycle = recording_tool_lifecycle(&events);
    let mut config = test_config();
    config
        .agents
        .entries
        .get_mut("main")
        .unwrap()
        .max_tool_result_bytes = Some(header.len() + 4);
    let fixture = crate::test_support::ConfigFixture::new(config)
        .await
        .unwrap();
    let tools_config = chelix_config::schema::ToolsConfig::default();
    let user_content = UserContent::text("run");

    super::super::run_agent_loop_streaming_with_limits(
        Arc::new(EndWindowProvider {
            call_count: AtomicUsize::new(0),
        }),
        &tools,
        &tools_config,
        &fixture.service,
        "main",
        "You are a test bot.",
        &user_content,
        None,
        Some(&on_lifecycle),
        None,
        None,
        test_tool_context(None),
        None,
        hooks,
        None,
        None,
        &CancellationToken::new(),
        test_agent_loop_limits(),
    )
    .await
    .unwrap();

    let events = events.lock().unwrap();
    completed_tool_result(&events, "call_end_window").to_string()
}

#[tokio::test]
async fn end_window_keeps_prefix_until_persist_hook_replaces_result() {
    let header = "Command finished in terminal (id: 7) with exit code 0.\nOutput:\n";
    let plain = run_end_window(None).await;
    assert_eq!(truncated_prefix(&plain), &format!("{header}TAIL"));

    let mut hooks = HookRegistry::new();
    hooks.register(Arc::new(ReplacePersistResultHook {
        replacement: serde_json::Value::String(format!("{}{}ZZZZ", "HEAD", "m".repeat(300))),
    }));
    let replaced = run_end_window(Some(Arc::new(hooks))).await;
    assert_eq!(
        truncated_prefix(&replaced),
        &format!("{}ZZZZ", "m".repeat(63))
    );
}

struct QuickBudgetTool {
    payload: String,
}

#[async_trait]
impl crate::tool_registry::AgentTool for QuickBudgetTool {
    fn name(&self) -> &str {
        "quick_budget_tool"
    }

    fn description(&self) -> &str {
        "Returns a large payload and honors quickResultBytes"
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "quickResultBytes": { "type": "integer" }
            }
        })
    }

    fn in_context_result_bytes(&self, params: &serde_json::Value) -> Option<usize> {
        params
            .get("quickResultBytes")
            .and_then(serde_json::Value::as_u64)
            .and_then(|n| usize::try_from(n).ok())
    }

    async fn execute(&self, _params: serde_json::Value) -> anyhow::Result<serde_json::Value> {
        Ok(serde_json::Value::String(self.payload.clone()))
    }
}

struct QuickBudgetProvider {
    call_count: AtomicUsize,
}

impl LlmProvider for QuickBudgetProvider {
    fn name(&self) -> &str {
        "quick-budget"
    }

    fn id(&self) -> &str {
        "quick-budget-model"
    }

    fn context_window(&self) -> Option<u32> {
        Some(TEST_CONTEXT_WINDOW)
    }

    fn max_input_tokens(&self) -> Option<u32> {
        Some(TEST_MAX_INPUT_TOKENS)
    }

    fn max_output_tokens(&self) -> Option<u32> {
        Some(TEST_MAX_OUTPUT_TOKENS)
    }

    fn supports_tools(&self) -> bool {
        true
    }

    fn stream_with_tools(
        &self,
        _messages: Vec<ChatMessage>,
        _tools: Vec<serde_json::Value>,
    ) -> Pin<Box<dyn Stream<Item = StreamEvent> + Send + '_>> {
        crate::model::response_stream(async move {
            let count = self.call_count.fetch_add(1, Ordering::SeqCst);
            match count {
                0 => Ok(CompletionResponse {
                    text: None,
                    tool_calls: vec![ToolCall {
                        id: "call_quick_override".into(),
                        name: "quick_budget_tool".into(),
                        arguments: serde_json::json!({ "quickResultBytes": 20 }),
                        argument_diagnostic: None,
                    }],
                    usage: Usage {
                        input_tokens: 10,
                        output_tokens: 5,
                        ..Default::default()
                    },
                    ..Default::default()
                }),
                1 => Ok(CompletionResponse {
                    text: None,
                    tool_calls: vec![ToolCall {
                        id: "call_quick_omit".into(),
                        name: "quick_budget_tool".into(),
                        arguments: serde_json::json!({}),
                        argument_diagnostic: None,
                    }],
                    usage: Usage {
                        input_tokens: 10,
                        output_tokens: 5,
                        ..Default::default()
                    },
                    ..Default::default()
                }),
                _ => Ok(CompletionResponse {
                    text: Some("Done!".into()),
                    tool_calls: vec![],
                    usage: Usage {
                        input_tokens: 20,
                        output_tokens: 10,
                        ..Default::default()
                    },
                    ..Default::default()
                }),
            }
        })
    }

    fn stream(
        &self,
        messages: Vec<ChatMessage>,
    ) -> Pin<Box<dyn Stream<Item = StreamEvent> + Send + '_>> {
        self.stream_with_tools(messages, Vec::new())
    }
}

fn completed_tool_result<'a>(events: &'a [RunnerToolLifecycleEvent], call_id: &str) -> &'a str {
    events
        .iter()
        .rev()
        .find_map(|event| match &event.lifecycle.update {
            ToolLifecycleUpdate::Completed {
                result: Some(result),
                ..
            } if event.lifecycle.tool_call_id == call_id => Some(result.as_str()),
            _ => None,
        })
        .unwrap_or_else(|| panic!("completed result missing for {call_id}"))
}

fn truncated_prefix(result: &str) -> &str {
    result.split("\n\n[Truncated").next().unwrap_or(result)
}

fn dump_contents(result: &str) -> String {
    let marker = "the content at: ";
    let start = result
        .find(marker)
        .map(|index| index + marker.len())
        .unwrap_or_else(|| panic!("dump path missing from truncated result"));
    let end = result[start..]
        .find(['\n', ']'])
        .map(|index| start + index)
        .unwrap_or(result.len());
    std::fs::read_to_string(result[start..end].trim())
        .unwrap_or_else(|error| panic!("failed to read dump: {error}"))
}

#[tokio::test]
async fn quick_result_bytes_overrides_runtime_limit_for_one_call() {
    let payload = "x".repeat(1000);
    let mut tools = ToolRegistry::new();
    tools.register(Box::new(QuickBudgetTool {
        payload: payload.clone(),
    }));
    let events = Arc::new(Mutex::new(Vec::new()));
    let on_lifecycle = recording_tool_lifecycle(&events);
    let limits = test_agent_loop_limits();
    let mut config = test_config();
    config
        .agents
        .entries
        .get_mut("main")
        .unwrap()
        .max_tool_result_bytes = Some(100);
    let fixture = crate::test_support::ConfigFixture::new(config)
        .await
        .unwrap();
    let tools_config = chelix_config::schema::ToolsConfig::default();
    let user_content = UserContent::text("run");

    super::super::run_agent_loop_streaming_with_limits(
        Arc::new(QuickBudgetProvider {
            call_count: AtomicUsize::new(0),
        }),
        &tools,
        &tools_config,
        &fixture.service,
        "main",
        "You are a test bot.",
        &user_content,
        None,
        Some(&on_lifecycle),
        None,
        None,
        test_tool_context(None),
        None,
        None,
        None,
        None,
        &CancellationToken::new(),
        limits,
    )
    .await
    .unwrap();

    let events = events.lock().unwrap();
    let override_result = completed_tool_result(&events, "call_quick_override");
    assert_eq!(truncated_prefix(override_result), "x".repeat(20));
    assert!(override_result.contains("[Truncated — full tool result"));
    assert_eq!(dump_contents(override_result), payload);

    let omit_result = completed_tool_result(&events, "call_quick_omit");
    assert_eq!(truncated_prefix(omit_result), "x".repeat(100));
    assert!(omit_result.contains("[Truncated — full tool result"));
    assert_eq!(dump_contents(omit_result), payload);
}

#[derive(Clone, Copy)]
enum LiveLimitChange {
    Global(usize),
    Agent(usize),
    RemoveAgent,
}

struct LiveLimitTool {
    service: chelix_config_service::ConfigService,
    change: LiveLimitChange,
}

#[async_trait]
impl crate::tool_registry::AgentTool for LiveLimitTool {
    fn name(&self) -> &str {
        "quick_budget_tool"
    }

    fn description(&self) -> &str {
        "Updates configuration while executing"
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({"type": "object", "properties": {"quickResultBytes": {"type": "integer"}}})
    }

    fn in_context_result_bytes(&self, params: &serde_json::Value) -> Option<usize> {
        params
            .get("quickResultBytes")
            .and_then(serde_json::Value::as_u64)
            .and_then(|bytes| usize::try_from(bytes).ok())
    }

    async fn execute(&self, _params: serde_json::Value) -> anyhow::Result<serde_json::Value> {
        let change = self.change;
        self.service
            .update(move |config| {
                match change {
                    LiveLimitChange::Global(bytes) => config.tools.max_tool_result_bytes = bytes,
                    LiveLimitChange::Agent(bytes) => {
                        config
                            .agents
                            .entries
                            .get_mut("main")
                            .unwrap()
                            .max_tool_result_bytes = Some(bytes)
                    },
                    LiveLimitChange::RemoveAgent => {
                        config.agents.entries.remove("main");
                    },
                }
                Ok(())
            })
            .await?;
        Ok(serde_json::Value::String("x".repeat(1000)))
    }
}

#[tokio::test]
async fn live_agent_and_global_limits_are_read_after_tool_execution() {
    let fixture = test_config_fixture().await;
    for change in [LiveLimitChange::Global(75), LiveLimitChange::Agent(50)] {
        fixture
            .service
            .update(|config| {
                config.tools.max_tool_result_bytes = 1000;
                config
                    .agents
                    .entries
                    .get_mut("main")
                    .unwrap()
                    .max_tool_result_bytes = None;
                Ok(())
            })
            .await
            .unwrap();
        let mut tools = ToolRegistry::new();
        tools.register(Box::new(LiveLimitTool {
            service: fixture.service.clone(),
            change,
        }));
        let events = Arc::new(Mutex::new(Vec::new()));
        let on_lifecycle = recording_tool_lifecycle(&events);
        run_agent_loop_streaming_with_tool_lifecycle(
            &fixture.service,
            Arc::new(QuickBudgetProvider {
                call_count: AtomicUsize::new(0),
            }),
            &tools,
            "You are a test bot.",
            &UserContent::text("run"),
            None,
            Some(&on_lifecycle),
            None,
            None,
            None,
            None,
            None,
        )
        .await
        .unwrap();
        let expected = match change {
            LiveLimitChange::Global(bytes) | LiveLimitChange::Agent(bytes) => bytes,
            LiveLimitChange::RemoveAgent => unreachable!(),
        };
        let events = events.lock().unwrap();
        assert_eq!(
            truncated_prefix(completed_tool_result(&events, "call_quick_omit")),
            "x".repeat(expected)
        );
    }
}

#[tokio::test]
async fn removing_current_agent_during_tool_execution_errors_even_with_call_override() {
    let fixture = test_config_fixture().await;
    let mut tools = ToolRegistry::new();
    tools.register(Box::new(LiveLimitTool {
        service: fixture.service.clone(),
        change: LiveLimitChange::RemoveAgent,
    }));
    let error = run_agent_loop_streaming_with_tool_lifecycle(
        &fixture.service,
        Arc::new(QuickBudgetProvider {
            call_count: AtomicUsize::new(0),
        }),
        &tools,
        "You are a test bot.",
        &UserContent::text("run"),
        None,
        None,
        None,
        None,
        None,
        None,
        None,
    )
    .await
    .unwrap_err();
    assert_eq!(error.to_string(), "agent 'main' is not configured");
}
