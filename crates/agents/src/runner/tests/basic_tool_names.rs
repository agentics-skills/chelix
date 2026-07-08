//! Tool-name sanitization and lookup tests.

use super::helpers::*;

#[test]
fn sanitize_tool_name_clean_input() {
    assert_eq!(sanitize_tool_name("execute_command"), "execute_command");
}

#[test]
fn sanitize_tool_name_trims_whitespace() {
    assert_eq!(sanitize_tool_name("  execute_command  "), "execute_command");
    assert_eq!(sanitize_tool_name("\texecute_command\n"), "execute_command");
}

#[test]
fn sanitize_tool_name_strips_quotes() {
    assert_eq!(sanitize_tool_name("\"execute_command\""), "execute_command");
    assert_eq!(sanitize_tool_name("  \"web_search\"  "), "web_search");
}

#[test]
fn sanitize_tool_name_partial_quotes_unchanged() {
    assert_eq!(sanitize_tool_name("\"execute_command"), "\"execute_command");
    assert_eq!(sanitize_tool_name("execute_command\""), "execute_command\"");
}

#[test]
fn sanitize_tool_name_noop_on_real_tool_names() {
    let real_names = [
        "execute_command",
        "web_search",
        "web_fetch",
        "memory_save",
        "memory_forget",
        "memory_delete",
        "memory_search",
        "file_read",
        "file_write",
        "calc",
        "mcp-server_tool-name",
    ];
    for name in real_names {
        assert_eq!(
            sanitize_tool_name(name),
            name,
            "sanitize_tool_name must be no-op on valid tool name '{name}'"
        );
    }
}

#[test]
fn sanitize_tool_name_empty_string() {
    assert_eq!(sanitize_tool_name(""), "");
    assert_eq!(sanitize_tool_name("  "), "");
}

#[test]
fn sanitize_tool_name_only_quotes() {
    assert_eq!(sanitize_tool_name("\"\""), "");
}

#[test]
fn sanitize_tool_name_preserves_internal_quotes() {
    assert_eq!(sanitize_tool_name("my\"tool"), "my\"tool");
}

#[test]
fn sanitize_tool_name_single_quotes_not_stripped() {
    assert_eq!(sanitize_tool_name("'execute_command'"), "'execute_command'");
}

#[test]
fn sanitize_tool_name_strips_numeric_suffix() {
    assert_eq!(sanitize_tool_name("execute_command_2"), "execute_command");
    assert_eq!(sanitize_tool_name("browser_4"), "browser");
    assert_eq!(sanitize_tool_name("execute_command_123"), "execute_command");
}

#[test]
fn sanitize_tool_name_strips_functions_prefix() {
    assert_eq!(sanitize_tool_name("functions_spawn_agent"), "spawn_agent");
    assert_eq!(
        sanitize_tool_name("functions_execute_command"),
        "execute_command"
    );
}

#[test]
fn sanitize_tool_name_strips_prefix_and_suffix() {
    assert_eq!(sanitize_tool_name("functions_spawn_agent_6"), "spawn_agent");
    assert_eq!(
        sanitize_tool_name("functions_execute_command_2"),
        "execute_command"
    );
}

#[test]
fn sanitize_tool_name_preserves_legitimate_underscores() {
    assert_eq!(sanitize_tool_name("web_search"), "web_search");
    assert_eq!(sanitize_tool_name("memory_save"), "memory_save");
    assert_eq!(sanitize_tool_name("memory_forget"), "memory_forget");
    assert_eq!(sanitize_tool_name("memory_delete"), "memory_delete");
    assert_eq!(sanitize_tool_name("spawn_agent"), "spawn_agent");
    assert_eq!(sanitize_tool_name("get_user_location"), "get_user_location");
}

#[test]
fn sanitize_tool_name_preserves_mcp_names() {
    assert_eq!(
        sanitize_tool_name("mcp__ai__find-tasks"),
        "mcp__ai__find-tasks"
    );
    assert_eq!(
        sanitize_tool_name("mcp__jmap-mcp-0-1-1__get_emails"),
        "mcp__jmap-mcp-0-1-1__get_emails"
    );
    assert_eq!(
        sanitize_tool_name("mcp-server_tool-name"),
        "mcp-server_tool-name"
    );
}

#[test]
fn sanitize_tool_name_functions_prefix_alone_yields_empty() {
    assert_eq!(sanitize_tool_name("functions_"), "");
}

#[test]
fn legacy_public_tool_alias_strips_wasm_suffix() {
    assert_eq!(
        legacy_public_tool_alias("web_search_wasm"),
        Some("web_search")
    );
    assert_eq!(legacy_public_tool_alias("calc_wasm"), Some("calc"));
    assert_eq!(legacy_public_tool_alias("web_search"), None);
}

#[test]
fn resolve_tool_lookup_prefers_public_alias_when_both_exist() {
    let mut tools = ToolRegistry::new();
    tools.register(Box::new(LargeResultTool {
        tool_name: "web_search",
        payload: "public".into(),
    }));
    tools.register_wasm(
        Box::new(LargeResultTool {
            tool_name: "web_search_wasm",
            payload: "legacy".into(),
        }),
        [0x11; 32],
    );

    let (tool, resolved_name) = resolve_tool_lookup(&tools, "web_search_wasm");
    let tool = tool.expect("resolved tool should exist");
    assert_eq!(resolved_name, "web_search");
    assert_eq!(tool.name(), "web_search");
}

#[test]
fn resolve_tool_lookup_falls_back_to_legacy_name_when_no_public_tool_exists() {
    let mut tools = ToolRegistry::new();
    tools.register_wasm(
        Box::new(LargeResultTool {
            tool_name: "web_search_wasm",
            payload: "legacy".into(),
        }),
        [0x22; 32],
    );

    let (tool, resolved_name) = resolve_tool_lookup(&tools, "web_search_wasm");
    let tool = tool.expect("legacy tool should exist");
    assert_eq!(resolved_name, "web_search_wasm");
    assert_eq!(tool.name(), "web_search_wasm");
}
