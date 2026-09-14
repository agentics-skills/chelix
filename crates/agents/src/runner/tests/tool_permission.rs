//! Permission sub-step outcomes for the shared tool executor.

use std::sync::Arc;

use {
    super::helpers::*,
    crate::runner::{
        OnToolPermission, TOOL_PERMISSION_SKIP_ERROR, ToolPermissionDecision, ToolPermissionPhase,
        tool_permission_deny_error,
    },
    chelix_common::tool_lifecycle::ToolLifecycleUpdate,
    tokio_util::sync::CancellationToken,
};

fn completed_error(events: &[RunnerToolLifecycleEvent]) -> Option<String> {
    events
        .iter()
        .rev()
        .find_map(|event| match &event.lifecycle.update {
            ToolLifecycleUpdate::Completed { error, .. } => error.clone(),
            _ => None,
        })
}

#[tokio::test]
async fn permission_skip_returns_standard_tool_error_without_executing() {
    let provider = Arc::new(ToolCallingProvider {
        call_count: std::sync::atomic::AtomicUsize::new(0),
    });
    let mut tools = ToolRegistry::new();
    tools.register(Box::new(EchoTool));
    let events = Arc::new(std::sync::Mutex::new(Vec::new()));
    let on_lifecycle = recording_tool_lifecycle(&events);
    let callback: OnToolPermission = Arc::new(|request| {
        Box::pin(async move {
            assert_eq!(request.phase, ToolPermissionPhase::BeforeExecution);
            Ok(ToolPermissionDecision::Skip)
        })
    });
    let tools_config = chelix_config::schema::ToolsConfig::default();
    let user_content = UserContent::text("Hi");
    super::super::streaming::run_agent_loop_streaming_with_limits(
        provider,
        &tools,
        &tools_config,
        "You are a test bot.",
        &user_content,
        None,
        Some(&on_lifecycle),
        Some(&callback),
        None,
        None,
        None,
        None,
        None,
        None,
        &CancellationToken::new(),
        test_agent_loop_limits(),
    )
    .await
    .unwrap();
    assert_eq!(
        completed_error(&events.lock().unwrap()).as_deref(),
        Some(TOOL_PERMISSION_SKIP_ERROR)
    );
}

#[tokio::test]
async fn permission_deny_returns_feedback_error() {
    let provider = Arc::new(ToolCallingProvider {
        call_count: std::sync::atomic::AtomicUsize::new(0),
    });
    let mut tools = ToolRegistry::new();
    tools.register(Box::new(EchoTool));
    let events = Arc::new(std::sync::Mutex::new(Vec::new()));
    let on_lifecycle = recording_tool_lifecycle(&events);
    let callback: OnToolPermission = Arc::new(|request| {
        Box::pin(async move {
            if request.phase == ToolPermissionPhase::BeforeExecution {
                Ok(ToolPermissionDecision::Deny {
                    feedback: "use the other URL".into(),
                })
            } else {
                Ok(ToolPermissionDecision::Approve)
            }
        })
    });
    let tools_config = chelix_config::schema::ToolsConfig::default();
    let user_content = UserContent::text("Hi");
    super::super::streaming::run_agent_loop_streaming_with_limits(
        provider,
        &tools,
        &tools_config,
        "You are a test bot.",
        &user_content,
        None,
        Some(&on_lifecycle),
        Some(&callback),
        None,
        None,
        None,
        None,
        None,
        None,
        &CancellationToken::new(),
        test_agent_loop_limits(),
    )
    .await
    .unwrap();
    let expected = tool_permission_deny_error("use the other URL");
    assert_eq!(
        completed_error(&events.lock().unwrap()).as_deref(),
        Some(expected.as_str())
    );
}

#[tokio::test]
async fn permission_approve_executes_tool() {
    let provider = Arc::new(ToolCallingProvider {
        call_count: std::sync::atomic::AtomicUsize::new(0),
    });
    let mut tools = ToolRegistry::new();
    tools.register(Box::new(EchoTool));
    let callback: OnToolPermission =
        Arc::new(|_request| Box::pin(async move { Ok(ToolPermissionDecision::Approve) }));
    let tools_config = chelix_config::schema::ToolsConfig::default();
    let user_content = UserContent::text("Hi");
    let result = super::super::streaming::run_agent_loop_streaming_with_limits(
        provider,
        &tools,
        &tools_config,
        "You are a test bot.",
        &user_content,
        None,
        None,
        Some(&callback),
        None,
        None,
        None,
        None,
        None,
        None,
        &CancellationToken::new(),
        test_agent_loop_limits(),
    )
    .await
    .unwrap();
    assert!(result.tool_calls_made >= 1);
}

#[tokio::test]
async fn permission_runs_before_unknown_tool_validation() {
    let provider = Arc::new(ToolCallingProvider {
        call_count: std::sync::atomic::AtomicUsize::new(0),
    });
    let tools = ToolRegistry::new();
    let events = Arc::new(std::sync::Mutex::new(Vec::new()));
    let on_lifecycle = recording_tool_lifecycle(&events);
    let callback: OnToolPermission = Arc::new(|request| {
        Box::pin(async move {
            assert_eq!(request.phase, ToolPermissionPhase::BeforeExecution);
            Ok(ToolPermissionDecision::Skip)
        })
    });
    let tools_config = chelix_config::schema::ToolsConfig::default();
    let user_content = UserContent::text("Hi");
    super::super::streaming::run_agent_loop_streaming_with_limits(
        provider,
        &tools,
        &tools_config,
        "You are a test bot.",
        &user_content,
        None,
        Some(&on_lifecycle),
        Some(&callback),
        None,
        None,
        None,
        None,
        None,
        None,
        &CancellationToken::new(),
        test_agent_loop_limits(),
    )
    .await
    .unwrap();
    assert_eq!(
        completed_error(&events.lock().unwrap()).as_deref(),
        Some(TOOL_PERMISSION_SKIP_ERROR)
    );
}
