use super::*;

#[test]
fn podman_sandbox_backend_accepted() {
    let toml = r#"
[sandbox]
archived_session_retention_days = 3
backend = "podman"
"#;
    let result = validate_toml_str(toml);
    let warning = result
        .diagnostics
        .iter()
        .find(|d| d.path == "sandbox.backend");
    assert!(
        warning.is_none(),
        "podman should be accepted as a valid sandbox backend"
    );
}

#[test]
fn removed_workspace_mount_is_rejected_as_unknown() {
    let toml = r#"
[sandbox]
workspace_mount = "ro"
"#;
    let result = validate_toml_str(toml);
    let unknown = result.diagnostics.iter().find(|diagnostic| {
        diagnostic.category == DiagnosticCategory::UnknownField
            && diagnostic.path == "sandbox.workspace_mount"
    });
    assert!(
        unknown.is_some(),
        "removed workspace_mount setting must not be accepted, got: {:?}",
        result.diagnostics
    );
}

#[test]
fn execute_command_rewrite_timeout_is_accepted() {
    let toml = r#"
[tools.execute_command]
rewrite_timeout_secs = 300
"#;
    let result = validate_toml_str(toml);
    let invalid = result.diagnostics.iter().find(|diagnostic| {
        diagnostic.path == "tools.execute_command.rewrite_timeout_secs"
            || diagnostic.category == DiagnosticCategory::TypeError
    });
    assert!(
        invalid.is_none(),
        "rewrite_timeout_secs should be accepted, got: {:?}",
        result.diagnostics
    );
}

#[test]
fn invalid_sandbox_mode_is_a_type_error() {
    let result = validate_toml_str("[sandbox]\nmode = \"Of\"\n");
    assert!(result.diagnostics.iter().any(|diagnostic| {
        diagnostic.severity == Severity::Error
            && diagnostic.category == DiagnosticCategory::TypeError
            && diagnostic.message.contains("Of")
    }));
}

#[test]
fn tools_agent_max_iterations_is_rejected_as_unknown() {
    let toml = r#"
[tools]
agent_max_iterations = 25
"#;
    let result = validate_toml_str(toml);
    let unknown = result.diagnostics.iter().find(|d| {
        d.path == "tools.agent_max_iterations"
            && d.severity == Severity::Error
            && d.category == DiagnosticCategory::UnknownField
    });
    assert!(
        unknown.is_some(),
        "expected tools.agent_max_iterations unknown-field error, got: {:?}",
        result.diagnostics
    );
}
