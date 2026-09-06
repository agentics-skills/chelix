use super::*;

#[test]
fn channels_offered_accepted_without_warning() {
    let toml = r#"
[channels]
offered = ["telegram"]
"#;
    let result = validate_toml_str(toml);
    let warning = result
        .diagnostics
        .iter()
        .find(|d| d.path.starts_with("channels.offered"));
    assert!(
        warning.is_none(),
        "valid channels.offered should not produce warnings, got: {:?}",
        result.diagnostics
    );
}

#[test]
fn channels_offered_telephony_accepted_for_manual_compatibility() {
    let toml = r#"
[channels]
offered = ["telephony"]
"#;
    let result = validate_toml_str(toml);
    let warning = result
        .diagnostics
        .iter()
        .find(|d| d.path.starts_with("channels.offered") && d.category == "unknown-field");
    assert!(
        warning.is_none(),
        "telephony remains a known internal channel type, got: {:?}",
        result.diagnostics
    );
}

#[test]
fn channels_offered_unknown_type_rejected() {
    let toml = r#"
[channels]
offered = ["telegram", "foobar"]
"#;
    let result = validate_toml_str(toml);
    let warning = result
        .diagnostics
        .iter()
        .find(|d| d.path == "channels.offered[1]" && d.category == "unknown-field");
    assert!(
        warning.is_some_and(|diagnostic| diagnostic.severity == Severity::Error),
        "unknown channel type should produce an error, got: {:?}",
        result.diagnostics
    );
}

#[test]
fn channels_unknown_account_type_rejected() {
    let result = validate_toml_str("[channels.unknown_type.bot]\ntoken = 'test'");
    assert!(result.diagnostics.iter().any(|diagnostic| {
        diagnostic.path == "channels.unknown_type" && diagnostic.severity == Severity::Error
    }));
}

#[test]
fn channels_offered_matrix_accepted() {
    let toml = r#"
[channels]
offered = ["telegram", "matrix"]
"#;
    let result = validate_toml_str(toml);
    let warning = result
        .diagnostics
        .iter()
        .find(|d| d.path == "channels.offered[1]" && d.category == "unknown-field");
    assert!(
        warning.is_none(),
        "matrix should be accepted, got: {:?}",
        result.diagnostics
    );
}

#[test]
fn channels_offered_configured_known_type_accepted() {
    let toml = r#"
[channels]
offered = ["telegram", "matrix"]

[channels.matrix.my-bot]
access_token = "matrix-test"
"#;
    let result = validate_toml_str(toml);
    let warning = result
        .diagnostics
        .iter()
        .find(|d| d.path.starts_with("channels.offered") && d.category == "unknown-field");
    assert!(
        warning.is_none(),
        "configured known channel type should be accepted in offered, got: {:?}",
        result.diagnostics
    );
}

#[test]
fn channels_extra_config_accepted() {
    let toml = r#"
[channels.matrix.my-bot]
access_token = "matrix-test"
dm_policy = "allowlist"
"#;
    let result = validate_toml_str(toml);
    let error = result.diagnostics.iter().find(|d| {
        d.path.starts_with("channels.matrix")
            && (d.severity == Severity::Error || d.category == "unknown-field")
    });
    assert!(
        error.is_none(),
        "extra channel config should be accepted without errors, got: {:?}",
        result.diagnostics
    );
}
