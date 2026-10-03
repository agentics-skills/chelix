use super::*;

#[test]
fn unknown_field_inside_provider_entry() {
    let toml = r#"
[providers.openai]
api_ky = "sk-test"
"#;
    let result = validate_toml_str(toml);
    let unknown = result
        .diagnostics
        .iter()
        .find(|d| d.category == "unknown-field" && d.path == "providers.openai.api_ky");
    assert!(
        unknown.is_some(),
        "expected unknown-field for 'providers.openai.api_ky', got: {:?}",
        result.diagnostics
    );
    assert!(unknown.unwrap().message.contains("api_key"));
}

#[test]
fn reserved_voice_provider_name_is_rejected() {
    let toml = r#"
[providers.voice-openai]
enabled = true
"#;
    let result = validate_toml_str(toml);
    let diagnostic = result
        .diagnostics
        .iter()
        .find(|d| d.category == "unknown-provider" && d.path == "providers.voice-openai");
    assert!(
        diagnostic.is_some(),
        "expected unknown-provider for 'voice-openai', got: {:?}",
        result.diagnostics
    );
    let d = diagnostic.unwrap();
    assert_eq!(d.severity, Severity::Error);
    assert!(d.message.contains("voice- prefix"));
}

#[test]
fn providers_offered_key_not_treated_as_provider_name() {
    let toml = r#"
[providers]
offered = ["openai", "openrouter"]
"#;
    let result = validate_toml_str(toml);
    let offered_warning = result
        .diagnostics
        .iter()
        .find(|d| d.category == "unknown-provider" && d.path == "providers.offered");
    assert!(
        offered_warning.is_none(),
        "providers.offered should be treated as metadata, got: {:?}",
        result.diagnostics
    );
}

#[test]
fn unknown_offered_provider_is_rejected() {
    let toml = r#"
[providers]
offered = ["openai", "voice-unsupported"]
"#;
    let result = validate_toml_str(toml);
    let diagnostic = result
        .diagnostics
        .iter()
        .find(|d| d.category == "unknown-provider" && d.path == "providers.offered[1]")
        .expect("unsupported offered provider should be rejected");
    assert_eq!(diagnostic.severity, Severity::Error);
}

#[test]
fn noncanonical_offered_provider_names_are_rejected() {
    for name in ["voice-openai", "offered", "bad_name"] {
        let toml = format!(
            r#"
[providers]
offered = ["{name}"]
"#
        );
        let result = validate_toml_str(&toml);
        let diagnostic = result
            .diagnostics
            .iter()
            .find(|d| d.category == "unknown-provider" && d.path == "providers.offered[0]")
            .unwrap_or_else(|| {
                panic!(
                    "noncanonical provider {name:?} should be rejected: {:?}",
                    result.diagnostics
                )
            });
        assert_eq!(diagnostic.severity, Severity::Error);
    }
}

#[test]
fn provider_section_name_with_underscore_is_rejected() {
    let toml = r#"
[providers.my_custom_llm]
enabled = true
"#;
    let result = validate_toml_str(toml);
    let diagnostic = result
        .diagnostics
        .iter()
        .find(|d| d.category == "unknown-provider");
    assert!(diagnostic.is_some());
    let d = diagnostic.unwrap();
    assert_eq!(d.severity, Severity::Error);
    assert!(d.message.contains("lowercase letters, digits, and hyphens"));
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
        .filter(|d| d.category == "unknown-field" && d.path.starts_with("env"))
        .collect();
    assert!(
        unknown_fields.is_empty(),
        "env keys should not be flagged as unknown: {unknown_fields:?}"
    );
}

#[test]
fn openai_compatible_provider_name_is_accepted() {
    let toml = r#"
[providers.together-ai]
enabled = true
"#;
    let result = validate_toml_str(toml);
    let unknown_providers: Vec<_> = result
        .diagnostics
        .iter()
        .filter(|d| d.category == "unknown-provider")
        .collect();
    assert!(
        unknown_providers.is_empty(),
        "slug should not trigger unknown-provider warning: {unknown_providers:?}"
    );
}

#[test]
fn invalid_provider_name_is_rejected() {
    let toml = r#"
[providers.voice-typo]
enabled = true
"#;
    let result = validate_toml_str(toml);
    let unknown_provider = result
        .diagnostics
        .iter()
        .find(|d| d.category == "unknown-provider")
        .expect("misspelled provider should trigger unknown-provider error");
    assert_eq!(unknown_provider.severity, Severity::Error);
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
        .find(|d| d.category == "unknown-field" && d.path.contains("tool_mode"));
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
            .find(|d| d.category == "type-error");
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
        .filter(|d| d.category == "unknown-field" && d.path.contains("upstream_proxy"))
        .collect();
    assert!(
        unknown.is_empty(),
        "upstream_proxy should be a known field: {unknown:?}"
    );
}

#[test]
fn upstream_proxy_invalid_scheme_rejected() {
    let toml = r#"upstream_proxy = "ftp://proxy.example.com""#;
    let result = validate_toml_str(toml);
    let errors: Vec<_> = result
        .diagnostics
        .iter()
        .filter(|d| d.severity == Severity::Error && d.path == "upstream_proxy")
        .collect();
    assert!(
        !errors.is_empty(),
        "upstream_proxy with ftp:// scheme should produce an error"
    );
}

#[test]
fn upstream_proxy_valid_schemes_accepted() {
    for scheme in ["http://", "https://", "socks5://", "socks5h://"] {
        let toml = format!(r#"upstream_proxy = "{scheme}proxy.example.com:1080""#);
        let result = validate_toml_str(&toml);
        let errors: Vec<_> = result
            .diagnostics
            .iter()
            .filter(|d| d.severity == Severity::Error && d.path == "upstream_proxy")
            .collect();
        assert!(
            errors.is_empty(),
            "upstream_proxy with {scheme} should not produce errors: {errors:?}"
        );
    }
}
