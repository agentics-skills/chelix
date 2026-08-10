use super::*;

#[test]
fn shadowed_defaults_emits_info_diagnostics() {
    let user_toml = r#"
[tools]
agent_timeout_secs = 600

[auth]
disabled = false
"#;
    let mut diagnostics = Vec::new();
    check_shadowed_defaults(user_toml, &mut diagnostics);

    let shadowed_paths: Vec<&str> = diagnostics
        .iter()
        .filter(|d| d.category == "shadowed-default")
        .map(|d| d.path.as_str())
        .collect();

    assert!(
        shadowed_paths.contains(&"tools.agent_timeout_secs"),
        "should detect tools.agent_timeout_secs as shadowed, got: {shadowed_paths:?}"
    );
    assert!(
        shadowed_paths.contains(&"auth.disabled"),
        "should detect auth.disabled as shadowed, got: {shadowed_paths:?}"
    );

    // All should be info severity
    for d in &diagnostics {
        if d.category == "shadowed-default" {
            assert_eq!(
                d.severity,
                Severity::Info,
                "shadowed-default should be Info"
            );
        }
    }
}

#[test]
fn shadowed_defaults_skips_user_owned_agents() {
    let user_toml = r#"
[agents]
default = "rex"

[agents.rex]
name = "Rex"
max_tools_threshold = 128
"#;
    let mut diagnostics = Vec::new();
    check_shadowed_defaults(user_toml, &mut diagnostics);

    let shadowed = diagnostics
        .iter()
        .filter(|diagnostic| diagnostic.category == "shadowed-default")
        .count();

    assert_eq!(
        shadowed, 0,
        "user-owned agent fields must not generate shadowed-default diagnostics"
    );
}

#[test]
fn shadowed_defaults_message_suggests_removal() {
    let user_toml = r#"
[tools]
agent_timeout_secs = 600
"#;
    let mut diagnostics = Vec::new();
    check_shadowed_defaults(user_toml, &mut diagnostics);

    let msg = diagnostics
        .iter()
        .find(|d| d.category == "shadowed-default" && d.path == "tools.agent_timeout_secs")
        .map(|d| d.message.as_str());

    assert!(
        msg.is_some_and(|m| m.contains("remove it from chelix.toml")),
        "message should suggest removing the shadowed key"
    );
}
