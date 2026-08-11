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
fn misspelled_provider_name_rejected_with_suggestion() {
    let toml = r#"
[providers.opnai]
enabled = true
"#;
    let result = validate_toml_str(toml);
    let diagnostic = result
        .diagnostics
        .iter()
        .find(|d| d.category == "unknown-provider" && d.path == "providers.opnai");
    assert!(
        diagnostic.is_some(),
        "expected unknown-provider for 'opnai', got: {:?}",
        result.diagnostics
    );
    let d = diagnostic.unwrap();
    assert_eq!(d.severity, Severity::Error);
    assert!(d.message.contains("openai"));
}

#[test]
fn providers_offered_key_not_treated_as_provider_name() {
    let toml = r#"
[providers]
offered = ["openai", "openai-codex"]
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
offered = ["openai", "unsupported-provider"]
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
    for name in ["claude", "google", "alibaba", "openai_codex"] {
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
fn provider_without_custom_prefix_is_rejected() {
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
    assert!(d.message.contains("custom-"));
}

#[test]
fn valid_known_providers_are_accepted() {
    let toml = r#"
[providers.moonshot]
enabled = true

[providers.openai]
enabled = true

[providers.openrouter]
enabled = true
"#;
    let result = validate_toml_str(toml);
    let warnings: Vec<_> = result
        .diagnostics
        .iter()
        .filter(|d| d.category == "unknown-provider")
        .collect();
    assert!(
        warnings.is_empty(),
        "known providers should not be warned about: {warnings:?}"
    );
}

#[test]
fn all_canonical_providers_are_accepted() {
    use crate::schema::KNOWN_PROVIDER_NAMES;
    for name in KNOWN_PROVIDER_NAMES {
        let toml = format!(
            r#"
[providers.{name}]
enabled = true
"#
        );
        let result = validate_toml_str(&toml);
        let warnings: Vec<_> = result
            .diagnostics
            .iter()
            .filter(|d| d.category == "unknown-provider")
            .collect();
        assert!(
            warnings.is_empty(),
            "canonical provider \"{name}\" triggered unknown-provider warning: {warnings:?}"
        );
    }
}

#[test]
fn env_section_passes_validation() {
    let toml = r#"
[agents]
default = "main"

[agents.main]
name = "Chelix"
max_tools_threshold = 128

[env]
FIRECRAWL_API_KEY = "test-key"
OPENROUTER_API_KEY = "sk-or-test"
CUSTOM_VAR = "some-value"
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
fn custom_provider_prefix_is_accepted() {
    let toml = r#"
[providers.custom-together-ai]
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
        "custom- prefix should not trigger unknown-provider warning: {unknown_providers:?}"
    );
}

#[test]
fn non_custom_unknown_provider_is_rejected() {
    let toml = r#"
[providers.typo-provider]
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
fn url_field_accepted_in_provider_entry() {
    let toml = r#"
[providers.openrouter]
enabled = true
url = "http://192.168.0.9:11434"
"#;
    let result = validate_toml_str(toml);
    let unknown = result
        .diagnostics
        .iter()
        .find(|d| d.category == "unknown-field" && d.path.contains("providers.openrouter.url"));
    assert!(
        unknown.is_none(),
        "url should be accepted as a provider field alias, got: {:?}",
        result.diagnostics
    );
}

#[test]
fn tool_mode_all_values_parse_correctly() {
    for mode in ["auto", "native", "text", "off"] {
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
fn cache_retention_field_accepted_in_provider_entry() {
    let toml = r#"
[providers.openrouter]
enabled = true
cache_retention = "short"
"#;
    let result = validate_toml_str(toml);
    let unknown = result
        .diagnostics
        .iter()
        .find(|d| d.category == "unknown-field" && d.path.contains("cache_retention"));
    assert!(
        unknown.is_none(),
        "cache_retention should be a known field, got: {:?}",
        result.diagnostics
    );
}

#[test]
fn cache_retention_all_values_parse_correctly() {
    for mode in ["none", "short", "long"] {
        let toml = format!(
            r#"
[providers.openrouter]
cache_retention = "{mode}"
"#
        );
        let result = validate_toml_str(&toml);
        let type_error = result
            .diagnostics
            .iter()
            .find(|d| d.category == "type-error");
        assert!(
            type_error.is_none(),
            "cache_retention = \"{mode}\" should parse without type error, got: {:?}",
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
