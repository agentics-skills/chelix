use super::*;

#[test]
fn unknown_memory_citations_mode_is_parse_error() {
    let toml = r#"
[memory]
citations = "sometimes"
"#;
    let result = validate_toml_str(toml);
    assert!(
        result.has_errors(),
        "expected parse error for unknown memory citations mode"
    );
}

#[test]
fn unknown_memory_search_merge_strategy_is_parse_error() {
    let toml = r#"
[memory]
search_merge_strategy = "blend"
"#;
    let result = validate_toml_str(toml);
    assert!(
        result.has_errors(),
        "expected parse error for unknown memory search merge strategy"
    );
}

#[test]
fn memory_disable_rag_is_valid_field() {
    let toml = r#"
[memory]
disable_rag = true
"#;
    let result = validate_toml_str(toml);
    let unknown = result
        .diagnostics
        .iter()
        .find(|d| d.category == "unknown-field" && d.path == "memory.disable_rag");
    assert!(
        unknown.is_none(),
        "memory.disable_rag should be accepted as a known field"
    );
}

#[test]
fn memory_style_is_valid_field() {
    let toml = r#"
[memory]
style = "search-only"
"#;
    let result = validate_toml_str(toml);
    let unknown = result
        .diagnostics
        .iter()
        .find(|d| d.category == "unknown-field" && d.path == "memory.style");
    assert!(
        unknown.is_none(),
        "memory.style should be accepted as a known field"
    );
}

#[test]
fn memory_agent_write_mode_is_valid_field() {
    let toml = r#"
[memory]
agent_write_mode = "prompt-only"
"#;
    let result = validate_toml_str(toml);
    let unknown = result
        .diagnostics
        .iter()
        .find(|d| d.category == "unknown-field" && d.path == "memory.agent_write_mode");
    assert!(
        unknown.is_none(),
        "memory.agent_write_mode should be accepted as a known field"
    );
}

#[test]
fn memory_user_profile_write_mode_is_valid_field() {
    let toml = r#"
[memory]
user_profile_write_mode = "explicit-only"
"#;
    let result = validate_toml_str(toml);
    let unknown = result
        .diagnostics
        .iter()
        .find(|d| d.category == "unknown-field" && d.path == "memory.user_profile_write_mode");
    assert!(
        unknown.is_none(),
        "memory.user_profile_write_mode should be accepted as a known field"
    );
}

#[test]
fn partial_memory_embedding_fields_are_invalid() {
    let toml = r#"
[memory]
url = "http://127.0.0.1:8080"
"#;
    let result = validate_toml_str(toml);
    assert!(
        result.diagnostics.iter().any(|d| {
            d.category == "invalid-value" && d.severity == Severity::Error && d.path == "memory"
        }),
        "expected invalid embedding endpoint: {:?}",
        result.diagnostics
    );
}

#[test]
fn memory_prefetch_fields_are_valid() {
    let toml = r#"
[agents]
default = "main"

[agents.main]
name = "Chelix"
model = "test::model"
reasoning_effort = "off"
max_tools_threshold = 128
compaction_reminder = true
prepend_sender_badge = true

[memory]
enable_prefetch = true
prefetch_limit = 5

[tools.execute_command]
terminal_size = "115x58"

[sandbox]
archived_session_retention_days = 3
"#;
    let result = validate_toml_str(toml);
    let unknown: Vec<_> = result
        .diagnostics
        .iter()
        .filter(|d| d.category == "unknown-field" && d.path.starts_with("memory."))
        .collect();
    assert!(
        unknown.is_empty(),
        "new memory lifecycle fields should be accepted: {unknown:?}"
    );
    assert!(
        !result.has_errors(),
        "no errors for valid memory lifecycle config: {:?}",
        result.diagnostics
    );
}

#[test]
fn skills_enable_self_improvement_is_valid() {
    let toml = r#"
[skills]
enable_self_improvement = false
"#;
    let result = validate_toml_str(toml);
    let unknown = result
        .diagnostics
        .iter()
        .find(|d| d.category == "unknown-field" && d.path == "skills.enable_self_improvement");
    assert!(
        unknown.is_none(),
        "skills.enable_self_improvement should be accepted"
    );
}

#[test]
fn memory_lifecycle_fields_default_to_true() {
    let config: ChelixConfig = toml::from_str("").unwrap();
    assert!(
        config.memory.enable_prefetch,
        "enable_prefetch should default true"
    );
    assert_eq!(
        config.memory.prefetch_limit, 3,
        "prefetch_limit should default 3"
    );
    assert!(
        config.skills.enable_self_improvement,
        "enable_self_improvement should default true"
    );
}
