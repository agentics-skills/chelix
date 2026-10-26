use super::*;

#[test]
fn unknown_field_inside_provider_entry() {
    let toml = r#"
[providers.openai]
api_ky = "sk-test"
"#;
    let result = validate_toml_str(toml);
    let unknown = result.diagnostics.iter().find(|d| {
        d.category == DiagnosticCategory::UnknownField && d.path == "providers.openai.api_ky"
    });
    assert!(
        unknown.is_some(),
        "expected unknown-field for 'providers.openai.api_ky', got: {:?}",
        result.diagnostics
    );
    assert!(unknown.unwrap().message.contains("api_key"));
}

#[test]
fn env_section_passes_validation() {
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

[env]
FIRECRAWL_API_KEY = "test-key"
OPENROUTER_API_KEY = "sk-or-test"
CUSTOM_VAR = "some-value"

[tools.execute_command]
terminal_size = "115x58"

[sandbox]
archived_session_retention_days = 3
"#;
    let result = validate_toml_str(toml);
    let errors: Vec<_> = result
        .diagnostics
        .iter()
        .filter(|d| d.severity == Severity::Error)
        .collect();
    assert!(
        errors.is_empty(),
        "env section should not produce errors: {errors:?}"
    );
    let unknown_fields: Vec<_> = result
        .diagnostics
        .iter()
        .filter(|d| d.category == DiagnosticCategory::UnknownField && d.path.starts_with("env"))
        .collect();
    assert!(
        unknown_fields.is_empty(),
        "env keys should not be flagged as unknown: {unknown_fields:?}"
    );
}

#[test]
fn tool_mode_field_accepted_in_provider_entry() {
    let toml = r#"
[providers.openrouter]
enabled = true
tool_mode = "text"
"#;
    let result = validate_toml_str(toml);
    let unknown = result
        .diagnostics
        .iter()
        .find(|d| d.category == DiagnosticCategory::UnknownField && d.path.contains("tool_mode"));
    assert!(
        unknown.is_none(),
        "tool_mode should be a known field, got: {:?}",
        result.diagnostics
    );
}

#[test]
fn tool_mode_all_values_parse_correctly() {
    for mode in ["native", "text", "off"] {
        let toml = format!(
            r#"
[providers.openai]
tool_mode = "{mode}"
"#
        );
        let result = validate_toml_str(&toml);
        let type_error = result
            .diagnostics
            .iter()
            .find(|d| d.category == DiagnosticCategory::TypeError);
        assert!(
            type_error.is_none(),
            "tool_mode = \"{mode}\" should parse without type error, got: {:?}",
            result.diagnostics
        );
    }
}

#[test]
fn upstream_proxy_not_flagged_as_unknown() {
    let toml = r#"upstream_proxy = "http://127.0.0.1:8080""#;
    let result = validate_toml_str(toml);
    let unknown: Vec<_> = result
        .diagnostics
        .iter()
        .filter(|d| {
            d.category == DiagnosticCategory::UnknownField && d.path.contains("upstream_proxy")
        })
        .collect();
    assert!(
        unknown.is_empty(),
        "upstream_proxy should be a known field: {unknown:?}"
    );
}
