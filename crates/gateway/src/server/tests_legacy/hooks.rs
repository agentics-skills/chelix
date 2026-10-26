use std::{collections::HashSet, sync::Arc};

use {
    super::common::LocalModelConfigTestGuard,
    crate::server::hooks::discover_and_build_hooks as discover_hooks,
};

async fn discover_and_build_hooks(
    disabled: &HashSet<String>,
) -> (
    Option<Arc<chelix_common::hooks::HookRegistry>>,
    Vec<crate::state::DiscoveredHookInfo>,
) {
    let config_service = crate::config_document::open_test_service().await;
    discover_hooks(&config_service, disabled)
        .await
        .expect("discover hooks")
}

fn write_config_hook(config_dir: &std::path::Path, command: &str, env: Option<(&str, &str)>) {
    let env_config = env
        .map(|(key, value)| format!("\n[hooks.hooks.env]\n{key} = {value:?}\n"))
        .unwrap_or_default();
    std::fs::write(
        config_dir.join("chelix.toml"),
        format!(
            r#"[hooks]

[[hooks.hooks]]
name = "config-test-hook"
command = {command:?}
events = ["BeforeLLMCall"]
timeout = 7

[tools.execute_command]
terminal_size = "115x58"

[sandbox]
archived_session_retention_days = 3
{env_config}"#
        ),
    )
    .unwrap();
}

fn write_filesystem_hook(data_dir: &std::path::Path, name: &str, command: &str) {
    let hook_dir = data_dir.join("hooks").join(name);
    std::fs::create_dir_all(&hook_dir).unwrap();
    std::fs::write(
        hook_dir.join("HOOK.md"),
        format!(
            r#"+++
name = {name:?}
command = {command:?}
events = ["BeforeLLMCall"]
timeout = 7
+++
"#
        ),
    )
    .unwrap();
}

#[tokio::test]
async fn discover_hooks_registers_builtin_handlers() {
    let guard = LocalModelConfigTestGuard::new().await;
    let project_dir = tempfile::tempdir().unwrap();
    let old_cwd = std::env::current_dir().unwrap();
    std::env::set_current_dir(project_dir.path()).unwrap();
    std::fs::write(
        guard.config_dir().join("chelix.toml"),
        "[tools.execute_command]\nterminal_size = \"115x58\"\n\n[sandbox]\narchived_session_retention_days = 3\n",
    )
    .unwrap();
    let (registry, info) = discover_and_build_hooks(&HashSet::new()).await;
    let registry = registry.expect("expected hook registry to be created");
    let handler_names = registry.handler_names();

    assert!(handler_names.iter().any(|n| n == "command-logger"));
    assert!(
        info.iter()
            .any(|h| h.name == "command-logger" && h.source == "builtin")
    );

    std::env::set_current_dir(old_cwd).unwrap();
}

#[tokio::test]
async fn discover_hooks_registers_config_shell_hooks() {
    let guard = LocalModelConfigTestGuard::new().await;
    write_config_hook(
        guard.config_dir(),
        "printf ''",
        Some(("CHELIX_TEST_HOOK", "1")),
    );

    let (registry, info) = discover_and_build_hooks(&HashSet::new()).await;
    let registry = registry.expect("expected hook registry to be created");
    let handler_names = registry.handler_names();

    assert!(handler_names.iter().any(|n| n == "config-test-hook"));
    let hook_info = info
        .iter()
        .find(|hook| hook.name == "config-test-hook")
        .expect("config hook should be discovered");
    assert_eq!(hook_info.source, "config");
    assert_eq!(hook_info.command.as_deref(), Some("printf ''"));
    assert_eq!(hook_info.events, vec!["BeforeLLMCall"]);
    assert_eq!(hook_info.timeout, 7);
    assert!(hook_info.enabled);
    assert!(hook_info.eligible);
}

#[tokio::test]
async fn discover_hooks_lists_disabled_config_hooks_without_registering() {
    let guard = LocalModelConfigTestGuard::new().await;
    write_config_hook(guard.config_dir(), "printf ''", None);

    let disabled = HashSet::from(["config-test-hook".to_string()]);

    let (registry, info) = discover_and_build_hooks(&disabled).await;
    let registry = registry.expect("expected hook registry to be created");
    let handler_names = registry.handler_names();

    assert!(!handler_names.iter().any(|n| n == "config-test-hook"));
    let hook_info = info
        .iter()
        .find(|hook| hook.name == "config-test-hook")
        .expect("disabled config hook should still be listed");
    assert_eq!(hook_info.source, "config");
    assert!(!hook_info.enabled);
    assert!(hook_info.eligible);
}

#[tokio::test]
async fn config_hook_info_lists_only_parsed_events() {
    let guard = LocalModelConfigTestGuard::new().await;
    std::fs::write(
        guard.config_dir().join("chelix.toml"),
        r#"[hooks]

[[hooks.hooks]]
name = "config-test-hook"
command = "printf ''"
events = ["BeforeLLMCall", "NotARealEvent"]
timeout = 7

[tools.execute_command]
terminal_size = "115x58"

[sandbox]
archived_session_retention_days = 3
"#,
    )
    .unwrap();

    let (registry, info) = discover_and_build_hooks(&HashSet::new()).await;
    let registry = registry.expect("expected hook registry to be created");
    let handler_names = registry.handler_names();

    assert!(handler_names.iter().any(|n| n == "config-test-hook"));
    let hook_info = info
        .iter()
        .find(|hook| hook.name == "config-test-hook")
        .expect("config hook should be discovered");
    assert_eq!(hook_info.events, vec!["BeforeLLMCall"]);
    assert!(hook_info.enabled);
    assert!(hook_info.eligible);
}

#[tokio::test]
async fn duplicate_config_hook_names_keep_first_entry() {
    let guard = LocalModelConfigTestGuard::new().await;
    let output_path = guard.data_dir().join("duplicate-config-output.txt");
    let first_command = format!("printf first > {:?}", output_path);
    let second_command = format!("printf second > {:?}", output_path);
    std::fs::write(
        guard.config_dir().join("chelix.toml"),
        format!(
            r#"[hooks]

[[hooks.hooks]]
name = "config-test-hook"
command = {first_command:?}
events = ["BeforeLLMCall"]
timeout = 7

[[hooks.hooks]]
name = "config-test-hook"
command = {second_command:?}
events = ["BeforeLLMCall"]
timeout = 7

[tools.execute_command]
terminal_size = "115x58"

[sandbox]
archived_session_retention_days = 3
"#
        ),
    )
    .unwrap();

    let (registry, info) = discover_and_build_hooks(&HashSet::new()).await;
    let registry = registry.expect("expected hook registry to be created");

    assert_eq!(
        info.iter()
            .filter(|hook| hook.name == "config-test-hook" && hook.source == "config")
            .count(),
        1
    );

    registry
        .dispatch(&chelix_common::hooks::HookPayload::BeforeLLMCall {
            session_key: "config-hook-test".to_string(),
            provider: "test-provider".to_string(),
            model: "test-model".to_string(),
            messages: serde_json::json!([]),
            tool_count: 0,
            iteration: 1,
        })
        .await
        .unwrap();

    let captured = std::fs::read_to_string(&output_path).unwrap();
    assert_eq!(captured, "first");
}

#[tokio::test]
async fn filesystem_hook_takes_precedence_over_same_named_config_hook() {
    let guard = LocalModelConfigTestGuard::new().await;
    let output_path = guard.data_dir().join("collision-output.txt");
    write_filesystem_hook(
        guard.data_dir(),
        "config-test-hook",
        &format!("printf fs > {:?}", output_path),
    );
    write_config_hook(
        guard.config_dir(),
        &format!("printf config > {:?}", output_path),
        None,
    );

    let (registry, info) = discover_and_build_hooks(&HashSet::new()).await;
    let registry = registry.expect("expected hook registry to be created");

    assert!(
        info.iter()
            .any(|hook| hook.name == "config-test-hook" && hook.source == "user")
    );
    assert!(
        !info
            .iter()
            .any(|hook| hook.name == "config-test-hook" && hook.source == "config")
    );

    registry
        .dispatch(&chelix_common::hooks::HookPayload::BeforeLLMCall {
            session_key: "config-hook-test".to_string(),
            provider: "test-provider".to_string(),
            model: "test-model".to_string(),
            messages: serde_json::json!([]),
            tool_count: 0,
            iteration: 1,
        })
        .await
        .unwrap();

    let captured = std::fs::read_to_string(&output_path).unwrap();
    assert_eq!(captured, "fs");
}

#[tokio::test]
async fn config_shell_hook_runs_when_event_dispatches() {
    let guard = LocalModelConfigTestGuard::new().await;
    let output_path = guard.data_dir().join("hook-payload.json");
    write_config_hook(
        guard.config_dir(),
        "cat > \"$CHELIX_TEST_HOOK_OUTPUT\"",
        Some((
            "CHELIX_TEST_HOOK_OUTPUT",
            output_path.to_string_lossy().as_ref(),
        )),
    );

    let (registry, _info) = discover_and_build_hooks(&HashSet::new()).await;
    let registry = registry.expect("expected hook registry to be created");

    let action = registry
        .dispatch(&chelix_common::hooks::HookPayload::BeforeLLMCall {
            session_key: "config-hook-test".to_string(),
            provider: "test-provider".to_string(),
            model: "test-model".to_string(),
            messages: serde_json::json!([]),
            tool_count: 0,
            iteration: 1,
        })
        .await
        .unwrap();

    assert!(matches!(action, chelix_common::hooks::HookAction::Continue));
    let captured = std::fs::read_to_string(&output_path).unwrap();
    assert!(captured.contains("BeforeLLMCall"));
    assert!(captured.contains("config-hook-test"));
}
