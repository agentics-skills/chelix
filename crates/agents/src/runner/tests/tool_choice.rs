//! Tests for per-run `tool_choice`.

use std::sync::Arc;

use {super::helpers::*, crate::model::ToolChoice};

#[tokio::test]
async fn tool_choice_none_forces_text_only() {
    // With tool_choice=none the provider receives an empty tool list.
    // MockProvider returns text regardless.
    let provider = Arc::new(MockProvider {
        response_text: "text-only".into(),
    });
    let mut tools = ToolRegistry::new();
    tools.register(Box::new(EchoTool));

    let result = run_agent_loop_with_context_and_limits(
        provider,
        &tools,
        "You are a test bot.",
        &UserContent::text("Hi"),
        None,
        None,
        None,
        Some(ToolChoice::None),
        None,
        None,
        test_agent_loop_limits(),
    )
    .await
    .unwrap();

    assert_eq!(result.output.text, "text-only");
    assert_eq!(result.tool_calls_made, 0);
}

#[tokio::test]
async fn tool_choice_any_with_no_tools_errors() {
    let provider = Arc::new(ToolCallingProvider {
        call_count: std::sync::atomic::AtomicUsize::new(0),
    });
    let tools = ToolRegistry::new();

    let err = run_agent_loop_with_context_and_limits(
        provider,
        &tools,
        "You are a test bot.",
        &UserContent::text("Hi"),
        None,
        None,
        None,
        Some(ToolChoice::Any),
        None,
        None,
        test_agent_loop_limits(),
    )
    .await
    .unwrap_err();

    let msg = err.to_string();
    assert!(
        msg.contains("requires at least one active tool"),
        "expected empty tool registry error, got: {msg}"
    );
}

#[tokio::test]
async fn tool_choice_forced_missing_tool_errors() {
    let provider = Arc::new(ToolCallingProvider {
        call_count: std::sync::atomic::AtomicUsize::new(0),
    });
    let mut tools = ToolRegistry::new();
    tools.register(Box::new(EchoTool));

    let err = run_agent_loop_with_context_and_limits(
        provider,
        &tools,
        "You are a test bot.",
        &UserContent::text("Hi"),
        None,
        None,
        None,
        Some(ToolChoice::Tool {
            name: "nonexistent_tool".to_string(),
        }),
        None,
        None,
        test_agent_loop_limits(),
    )
    .await
    .unwrap_err();

    let msg = err.to_string();
    assert!(
        msg.contains("unavailable tool"),
        "expected unavailable tool error, got: {msg}"
    );
}

#[tokio::test]
async fn no_tool_choice_runs_normally() {
    let provider = Arc::new(ToolCallingProvider {
        call_count: std::sync::atomic::AtomicUsize::new(0),
    });
    let mut tools = ToolRegistry::new();
    tools.register(Box::new(EchoTool));

    let result = run_agent_loop_with_context_and_limits(
        provider,
        &tools,
        "You are a test bot.",
        &UserContent::text("Hi"),
        None,
        None,
        None,
        None,
        None,
        None,
        test_agent_loop_limits(),
    )
    .await
    .unwrap();

    assert_eq!(result.output.text, "Done!");
    assert_eq!(result.tool_calls_made, 1);
}
