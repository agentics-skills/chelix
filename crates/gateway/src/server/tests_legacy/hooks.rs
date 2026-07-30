use std::{collections::HashSet, sync::Arc};

use {
    super::common::LocalModelConfigTestGuard,
    crate::server::hooks::discover_and_build_hooks as discover_hooks,
};

async fn discover_and_build_hooks(
    disabled: &HashSet<String>,
    session_store: Option<&Arc<chelix_sessions::store::SessionStore>>,
) -> (
    Option<Arc<chelix_common::hooks::HookRegistry>>,
    Vec<crate::state::DiscoveredHookInfo>,
) {
    discover_hooks(disabled, session_store)
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
    let _guard = LocalModelConfigTestGuard::new();
    let tmp = tempfile::tempdir().unwrap();
    let config_dir = tempfile::tempdir().unwrap();
    let project_dir = tempfile::tempdir().unwrap();
    let old_cwd = std::env::current_dir().unwrap();
    std::env::set_current_dir(project_dir.path()).unwrap();
    std::fs::write(
        config_dir.path().join("chelix.toml"),
        "[memory]\nsession_export = \"on-new-or-reset\"\n",
    )
    .unwrap();
    chelix_config::set_config_dir(config_dir.path().to_path_buf());
    let sessions_dir = tmp.path().join("sessions");
    std::fs::create_dir_all(&sessions_dir).unwrap();
    let session_store = Arc::new(chelix_sessions::store::SessionStore::new(sessions_dir));

    let (registry, info) = discover_and_build_hooks(&HashSet::new(), Some(&session_store)).await;
    let registry = registry.expect("expected hook registry to be created");
    let handler_names = registry.handler_names();

    assert!(handler_names.iter().any(|n| n == "command-logger"));
    assert!(handler_names.iter().any(|n| n == "session-memory"));
    assert!(
        info.iter()
            .any(|h| h.name == "command-logger" && h.source == "builtin")
    );
    assert!(
        info.iter()
            .any(|h| h.name == "session-memory" && h.source == "builtin")
    );

    std::env::set_current_dir(old_cwd).unwrap();
    chelix_config::clear_config_dir();
}

#[tokio::test]
async fn discover_hooks_respects_session_export_mode_off() {
    let _guard = LocalModelConfigTestGuard::new();
    let tmp = tempfile::tempdir().unwrap();
    let config_dir = tempfile::tempdir().unwrap();
    let project_dir = tempfile::tempdir().unwrap();
    let old_cwd = std::env::current_dir().unwrap();
    std::env::set_current_dir(project_dir.path()).unwrap();
    std::fs::write(
        config_dir.path().join("chelix.toml"),
        "[memory]\nsession_export = \"off\"\n",
    )
    .unwrap();
    chelix_config::set_config_dir(config_dir.path().to_path_buf());

    let sessions_dir = tmp.path().join("sessions");
    std::fs::create_dir_all(&sessions_dir).unwrap();
    let session_store = Arc::new(chelix_sessions::store::SessionStore::new(sessions_dir));

    let (registry, info) = discover_and_build_hooks(&HashSet::new(), Some(&session_store)).await;
    let registry = registry.expect("expected hook registry to be created");
    let handler_names = registry.handler_names();

    assert!(handler_names.iter().any(|n| n == "command-logger"));
    assert!(!handler_names.iter().any(|n| n == "session-memory"));
    assert!(
        info.iter()
            .any(|h| h.name == "session-memory" && h.source == "builtin" && !h.enabled)
    );

    std::env::set_current_dir(old_cwd).unwrap();
    chelix_config::clear_config_dir();
}

#[tokio::test]
async fn discover_hooks_registers_config_shell_hooks() {
    let _guard = LocalModelConfigTestGuard::new();
    let data_dir = tempfile::tempdir().unwrap();
    let config_dir = tempfile::tempdir().unwrap();
    write_config_hook(
        config_dir.path(),
        "printf ''",
        Some(("CHELIX_TEST_HOOK", "1")),
    );
    chelix_config::set_data_dir(data_dir.path().to_path_buf());
    chelix_config::set_config_dir(config_dir.path().to_path_buf());

    let sessions_dir = data_dir.path().join("sessions");
    std::fs::create_dir_all(&sessions_dir).unwrap();
    let session_store = Arc::new(chelix_sessions::store::SessionStore::new(sessions_dir));

    let (registry, info) = discover_and_build_hooks(&HashSet::new(), Some(&session_store)).await;
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

    chelix_config::clear_config_dir();
    chelix_config::clear_data_dir();
}

#[tokio::test]
async fn discover_hooks_lists_disabled_config_hooks_without_registering() {
    let _guard = LocalModelConfigTestGuard::new();
    let data_dir = tempfile::tempdir().unwrap();
    let config_dir = tempfile::tempdir().unwrap();
    write_config_hook(config_dir.path(), "printf ''", None);
    chelix_config::set_data_dir(data_dir.path().to_path_buf());
    chelix_config::set_config_dir(config_dir.path().to_path_buf());

    let sessions_dir = data_dir.path().join("sessions");
    std::fs::create_dir_all(&sessions_dir).unwrap();
    let session_store = Arc::new(chelix_sessions::store::SessionStore::new(sessions_dir));
    let disabled = HashSet::from(["config-test-hook".to_string()]);

    let (registry, info) = discover_and_build_hooks(&disabled, Some(&session_store)).await;
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

    chelix_config::clear_config_dir();
    chelix_config::clear_data_dir();
}

#[tokio::test]
async fn config_hook_info_lists_only_parsed_events() {
    let _guard = LocalModelConfigTestGuard::new();
    let data_dir = tempfile::tempdir().unwrap();
    let config_dir = tempfile::tempdir().unwrap();
    std::fs::write(
        config_dir.path().join("chelix.toml"),
        r#"[hooks]

[[hooks.hooks]]
name = "config-test-hook"
command = "printf ''"
events = ["BeforeLLMCall", "NotARealEvent"]
timeout = 7
"#,
    )
    .unwrap();
    chelix_config::set_data_dir(data_dir.path().to_path_buf());
    chelix_config::set_config_dir(config_dir.path().to_path_buf());

    let sessions_dir = data_dir.path().join("sessions");
    std::fs::create_dir_all(&sessions_dir).unwrap();
    let session_store = Arc::new(chelix_sessions::store::SessionStore::new(sessions_dir));

    let (registry, info) = discover_and_build_hooks(&HashSet::new(), Some(&session_store)).await;
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

    chelix_config::clear_config_dir();
    chelix_config::clear_data_dir();
}

#[tokio::test]
async fn duplicate_config_hook_names_keep_first_entry() {
    let _guard = LocalModelConfigTestGuard::new();
    let data_dir = tempfile::tempdir().unwrap();
    let config_dir = tempfile::tempdir().unwrap();
    let output_path = data_dir.path().join("duplicate-config-output.txt");
    let first_command = format!("printf first > {:?}", output_path);
    let second_command = format!("printf second > {:?}", output_path);
    std::fs::write(
        config_dir.path().join("chelix.toml"),
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
"#
        ),
    )
    .unwrap();
    chelix_config::set_data_dir(data_dir.path().to_path_buf());
    chelix_config::set_config_dir(config_dir.path().to_path_buf());

    let sessions_dir = data_dir.path().join("sessions");
    std::fs::create_dir_all(&sessions_dir).unwrap();
    let session_store = Arc::new(chelix_sessions::store::SessionStore::new(sessions_dir));
    let (registry, info) = discover_and_build_hooks(&HashSet::new(), Some(&session_store)).await;
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

    chelix_config::clear_config_dir();
    chelix_config::clear_data_dir();
}

#[tokio::test]
async fn filesystem_hook_takes_precedence_over_same_named_config_hook() {
    let _guard = LocalModelConfigTestGuard::new();
    let data_dir = tempfile::tempdir().unwrap();
    let config_dir = tempfile::tempdir().unwrap();
    let output_path = data_dir.path().join("collision-output.txt");
    write_filesystem_hook(
        data_dir.path(),
        "config-test-hook",
        &format!("printf fs > {:?}", output_path),
    );
    write_config_hook(
        config_dir.path(),
        &format!("printf config > {:?}", output_path),
        None,
    );
    chelix_config::set_data_dir(data_dir.path().to_path_buf());
    chelix_config::set_config_dir(config_dir.path().to_path_buf());

    let sessions_dir = data_dir.path().join("sessions");
    std::fs::create_dir_all(&sessions_dir).unwrap();
    let session_store = Arc::new(chelix_sessions::store::SessionStore::new(sessions_dir));
    let (registry, info) = discover_and_build_hooks(&HashSet::new(), Some(&session_store)).await;
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

    chelix_config::clear_config_dir();
    chelix_config::clear_data_dir();
}

#[tokio::test]
async fn config_shell_hook_runs_when_event_dispatches() {
    let _guard = LocalModelConfigTestGuard::new();
    let data_dir = tempfile::tempdir().unwrap();
    let config_dir = tempfile::tempdir().unwrap();
    let output_path = data_dir.path().join("hook-payload.json");
    write_config_hook(
        config_dir.path(),
        "cat > \"$CHELIX_TEST_HOOK_OUTPUT\"",
        Some((
            "CHELIX_TEST_HOOK_OUTPUT",
            output_path.to_string_lossy().as_ref(),
        )),
    );
    chelix_config::set_data_dir(data_dir.path().to_path_buf());
    chelix_config::set_config_dir(config_dir.path().to_path_buf());

    let sessions_dir = data_dir.path().join("sessions");
    std::fs::create_dir_all(&sessions_dir).unwrap();
    let session_store = Arc::new(chelix_sessions::store::SessionStore::new(sessions_dir));
    let (registry, _info) = discover_and_build_hooks(&HashSet::new(), Some(&session_store)).await;
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

    chelix_config::clear_config_dir();
    chelix_config::clear_data_dir();
}

#[tokio::test]
async fn command_hook_dispatch_saves_session_memory_file() {
    let tmp = tempfile::tempdir().unwrap();
    let sessions_dir = tmp.path().join("sessions");
    std::fs::create_dir_all(&sessions_dir).unwrap();
    let session_store = Arc::new(chelix_sessions::store::SessionStore::new(sessions_dir));

    session_store
        .append(
            "smoke-session",
            &serde_json::json!({"role": "user", "content": "Hello from smoke test"}),
        )
        .await
        .unwrap();
    session_store
        .append(
            "smoke-session",
            &serde_json::json!({"role": "assistant", "content": "Hi there"}),
        )
        .await
        .unwrap();

    let mut registry = chelix_common::hooks::HookRegistry::new();
    registry.register(Arc::new(
        chelix_plugins::bundled::session_memory::SessionMemoryHook::new(
            tmp.path().to_path_buf(),
            Arc::clone(&session_store),
        ),
    ));

    let payload = chelix_common::hooks::HookPayload::Command {
        session_key: "smoke-session".into(),
        action: "new".into(),
        sender_id: None,
    };
    let result = registry.dispatch(&payload).await.unwrap();
    assert!(matches!(result, chelix_common::hooks::HookAction::Continue));

    let memory_dir = tmp.path().join("memory");
    assert!(memory_dir.is_dir());

    let files: Vec<_> = std::fs::read_dir(&memory_dir).unwrap().flatten().collect();
    assert_eq!(files.len(), 1);

    let content = std::fs::read_to_string(files[0].path()).unwrap();
    assert!(content.contains("smoke-session"));
    assert!(content.contains("Hello from smoke test"));
    assert!(content.contains("Hi there"));
}
