use super::*;

#[test]
fn sandbox_mode_off_warned() {
    let toml = r#"
[sandbox]
mode = "Off"
"#;
    let result = validate_toml_str(toml);
    let warning = result.diagnostics.iter().find(|d| d.path == "sandbox.mode");
    assert!(warning.is_some(), "expected warning for sandbox mode off");
}

#[test]
fn port_zero_info() {
    let toml = r#"
[server]
port = 0
"#;
    let result = validate_toml_str(toml);
    let info = result
        .diagnostics
        .iter()
        .find(|d| d.severity == Severity::Info && d.path == "server.port");
    assert!(info.is_some(), "expected info for port 0");
}

#[test]
fn podman_sandbox_backend_accepted() {
    let toml = r#"
[sandbox]
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
        diagnostic.category == "unknown-field" && diagnostic.path == "sandbox.workspace_mount"
    });
    assert!(
        unknown.is_some(),
        "removed workspace_mount setting must not be accepted, got: {:?}",
        result.diagnostics
    );
}

#[test]
fn valid_declarative_sandbox_mount_is_accepted() {
    let toml = r#"
[[sandbox.mounts]]
host = "/srv/reference"
guest = "/mnt/reference"
mode = "ro"
"#;
    let result = validate_toml_str(toml);
    let mount_errors: Vec<_> = result
        .diagnostics
        .iter()
        .filter(|diagnostic| diagnostic.path.starts_with("sandbox.mounts"))
        .collect();
    assert!(
        mount_errors.is_empty(),
        "valid custom mount should be accepted, got: {mount_errors:?}"
    );
}

#[test]
fn sandbox_data_dir_source_cannot_use_a_different_guest_path() {
    let toml = r#"
[sandbox]
host_data_dir = "/host/chelix-data"

[[sandbox.mounts]]
host = "/host/chelix-data"
guest = "/different/data"
mode = "rw"
"#;
    let result = validate_toml_str(toml);
    let error = result.diagnostics.iter().find(|diagnostic| {
        diagnostic.path == "sandbox.mounts[0].guest"
            && diagnostic.severity == Severity::Error
            && diagnostic.message.contains("identical agent path")
    });
    assert!(
        error.is_some(),
        "expected mismatched data_dir guest path error, got: {:?}",
        result.diagnostics
    );
}

#[test]
fn sandbox_custom_mount_paths_must_be_absolute() {
    let toml = r#"
[[sandbox.mounts]]
host = "relative/source"
guest = "/mnt/source"
mode = "ro"
"#;
    let result = validate_toml_str(toml);
    let error = result.diagnostics.iter().find(|diagnostic| {
        diagnostic.path == "sandbox.mounts[0].host"
            && diagnostic.severity == Severity::Error
            && diagnostic.message.contains("must be absolute")
    });
    assert!(
        error.is_some(),
        "expected relative mount source error, got: {:?}",
        result.diagnostics
    );
}

#[test]
fn sandbox_custom_mount_diagnostics_preserve_configured_index() {
    let toml = r#"
[[sandbox.mounts]]
host = "/srv/reference"
guest = "/mnt/reference"
mode = "ro"

[[sandbox.mounts]]
host = "/srv/invalid"
guest = "relative/guest"
mode = "rw"
"#;
    let result = validate_toml_str(toml);
    let error = result.diagnostics.iter().find(|diagnostic| {
        diagnostic.path == "sandbox.mounts[1].guest"
            && diagnostic.severity == Severity::Error
            && diagnostic.message.contains("must be absolute")
    });
    assert!(
        error.is_some(),
        "expected indexed custom mount error, got: {:?}",
        result.diagnostics
    );
}

#[test]
fn sandbox_data_mount_cannot_source_config_dir() {
    let toml = r#"
[sandbox]
mode = "Off"
host_data_dir = "/"
"#;
    let result = validate_toml_str(toml);
    let error = result.diagnostics.iter().find(|diagnostic| {
        diagnostic.path == "sandbox.host_data_dir"
            && diagnostic.severity == Severity::Error
            && diagnostic.category == "security"
    });
    assert!(
        error.is_some(),
        "expected data/config overlap error, got: {:?}",
        result.diagnostics
    );
}

#[test]
fn sandbox_shared_home_cannot_source_config_dir() {
    let toml = r#"
[sandbox]
home_persistence = "shared"
shared_home_dir = "/"
"#;
    let result = validate_toml_str(toml);
    let error = result.diagnostics.iter().find(|diagnostic| {
        diagnostic.path == "sandbox.shared_home_dir"
            && diagnostic.severity == Severity::Error
            && diagnostic.category == "security"
    });
    assert!(
        error.is_some(),
        "expected shared home/config overlap error, got: {:?}",
        result.diagnostics
    );
}

#[test]
fn unknown_security_level_warned() {
    let toml = r#"
[tools.execute_command]
security_level = "paranoid"
"#;
    let result = validate_toml_str(toml);
    let warning = result
        .diagnostics
        .iter()
        .find(|d| d.path == "tools.execute_command.security_level");
    assert!(
        warning.is_some(),
        "expected warning for unknown security level"
    );
}

#[test]
fn ssh_command_host_accepted() {
    let toml = r#"
[tools.execute_command]
host = "ssh"
ssh_target = "deploy@example"
"#;
    let result = validate_toml_str(toml);
    let warning = result
        .diagnostics
        .iter()
        .find(|d| d.path == "tools.execute_command.host");
    assert!(
        warning.is_none(),
        "ssh should be accepted as a valid command host"
    );
}

#[test]
fn ssh_command_host_without_target_warned() {
    let toml = r#"
[tools.execute_command]
host = "ssh"
"#;
    let result = validate_toml_str(toml);
    let warning = result
        .diagnostics
        .iter()
        .find(|d| d.path == "tools.execute_command.ssh_target");
    assert!(warning.is_some(), "expected warning for missing ssh target");
}

#[test]
fn browser_obscura_path_accepted() {
    let toml = r#"
[tools.browser]
obscura_path = "/usr/local/bin/obscura"
"#;
    let result = validate_toml_str(toml);
    let unknown = result
        .diagnostics
        .iter()
        .find(|d| d.category == "unknown-field" && d.path == "tools.browser.obscura_path");
    assert!(
        unknown.is_none(),
        "obscura_path should be accepted as a browser config field, got: {:?}",
        result.diagnostics
    );
}

#[test]
fn browser_lightpanda_path_accepted() {
    let toml = r#"
[tools.browser]
lightpanda_path = "/usr/local/bin/lightpanda"
"#;
    let result = validate_toml_str(toml);
    let unknown = result
        .diagnostics
        .iter()
        .find(|d| d.category == "unknown-field" && d.path == "tools.browser.lightpanda_path");
    assert!(
        unknown.is_none(),
        "lightpanda_path should be accepted as a browser config field, got: {:?}",
        result.diagnostics
    );
}

#[test]
fn browser_sandbox_override_rejected() {
    let toml = r#"
[tools.browser]
sandbox = false
"#;
    let result = validate_toml_str(toml);
    let unknown = result.diagnostics.iter().find(|diagnostic| {
        diagnostic.category == "unknown-field" && diagnostic.path == "tools.browser.sandbox"
    });
    assert!(
        unknown.is_some(),
        "browser sandbox override must be rejected, got: {:?}",
        result.diagnostics
    );
}

#[test]
fn tools_agent_max_iterations_must_be_positive() {
    let toml = r#"
[tools]
agent_max_iterations = 0
"#;
    let result = validate_toml_str(toml);
    let invalid = result.diagnostics.iter().find(|d| {
        d.path == "tools.agent_max_iterations"
            && d.severity == Severity::Error
            && d.category == "invalid-value"
    });
    assert!(
        invalid.is_some(),
        "expected tools.agent_max_iterations invalid-value error, got: {:?}",
        result.diagnostics
    );
}

#[test]
fn mcp_request_timeout_must_be_positive() {
    let toml = r#"
[mcp]
request_timeout_secs = 0
"#;
    let result = validate_toml_str(toml);
    let invalid = result.diagnostics.iter().find(|d| {
        d.path == "mcp.request_timeout_secs"
            && d.severity == Severity::Error
            && d.category == "invalid-value"
    });
    assert!(
        invalid.is_some(),
        "expected mcp.request_timeout_secs invalid-value error, got: {:?}",
        result.diagnostics
    );
}

#[test]
fn mcp_server_request_timeout_override_must_be_positive() {
    let toml = r#"
[mcp.servers.memory]
command = "npx"
request_timeout_secs = 0
"#;
    let result = validate_toml_str(toml);
    let invalid = result.diagnostics.iter().find(|d| {
        d.path == "mcp.servers.memory.request_timeout_secs"
            && d.severity == Severity::Error
            && d.category == "invalid-value"
    });
    assert!(
        invalid.is_some(),
        "expected mcp server request_timeout_secs invalid-value error, got: {:?}",
        result.diagnostics
    );
}
