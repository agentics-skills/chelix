use {super::*, crate::AgentRuntimeLimitSource};

#[test]
fn agent_runtime_limits_use_required_threshold_and_global_timeout() {
    let config: ChelixConfig = toml::from_str(
        r#"
[tools]
agent_timeout_secs = 120

[agents.quick]
name = "Quick"
model = "test::model"
reasoning_effort = "off"
max_tools_threshold = 11
compaction_reminder = true
prepend_sender_badge = true
"#,
    )
    .unwrap();

    let limits = config.agent_runtime_limits("quick").unwrap();
    assert_eq!(limits.timeout_secs, 120);
    assert_eq!(limits.timeout_source, AgentRuntimeLimitSource::GlobalTools);
    assert_eq!(limits.max_tools_threshold, 11);
}

#[test]
fn agent_runtime_limits_use_agent_timeout_override() {
    let config: ChelixConfig = toml::from_str(
        r#"
[tools]
agent_timeout_secs = 120

[agents.quick]
name = "Quick"
model = "test::model"
reasoning_effort = "off"
timeout_secs = 5
max_tools_threshold = 11
compaction_reminder = true
prepend_sender_badge = true
"#,
    )
    .unwrap();

    let limits = config.agent_runtime_limits("quick").unwrap();
    assert_eq!(limits.timeout_secs, 5);
    assert_eq!(limits.timeout_source, AgentRuntimeLimitSource::Agent);
    assert_eq!(limits.max_tools_threshold, 11);
}

#[test]
fn agent_runtime_limits_reject_missing_agent() {
    let config = ChelixConfig::default();
    let error = config.agent_runtime_limits("missing").unwrap_err();
    assert_eq!(error.to_string(), "agent 'missing' is not configured");
}

#[test]
fn agent_rejects_missing_name() {
    let result = toml::from_str::<ChelixConfig>(
        r#"
[agents.quick]
model = "test::model"
reasoning_effort = "off"
max_tools_threshold = 11
compaction_reminder = true
prepend_sender_badge = true
"#,
    );
    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("name"));
}

#[test]
fn agent_rejects_missing_max_tools_threshold() {
    let result = toml::from_str::<ChelixConfig>(
        r#"
[agents.quick]
name = "Quick"
model = "test::model"
reasoning_effort = "off"
compaction_reminder = true
prepend_sender_badge = true
"#,
    );
    assert!(result.is_err());
    assert!(
        result
            .unwrap_err()
            .to_string()
            .contains("max_tools_threshold")
    );
}

#[test]
fn agent_rejects_missing_compaction_reminder() {
    let result = toml::from_str::<ChelixConfig>(
        r#"
[agents.quick]
name = "Quick"
model = "test::model"
reasoning_effort = "off"
max_tools_threshold = 128
prepend_sender_badge = true
"#,
    );

    let error = result.expect_err("compaction_reminder must be explicit");
    assert!(error.to_string().contains("compaction_reminder"));
}

#[test]
fn agent_rejects_missing_prepend_sender_badge() {
    let result = toml::from_str::<ChelixConfig>(
        r#"
[agents.quick]
name = "Quick"
model = "test::model"
reasoning_effort = "off"
max_tools_threshold = 128
compaction_reminder = true
"#,
    );

    let error = result.expect_err("prepend_sender_badge must be explicit");
    assert!(error.to_string().contains("prepend_sender_badge"));
}

#[test]
fn agent_requires_model_and_reasoning_effort() {
    let cases = [
        (
            r#"
[agents.quick]
name = "Quick"
reasoning_effort = "off"
max_tools_threshold = 128
compaction_reminder = true
prepend_sender_badge = true
"#,
            "model",
        ),
        (
            r#"
[agents.quick]
name = "Quick"
model = "test::model"
max_tools_threshold = 128
compaction_reminder = true
prepend_sender_badge = true
"#,
            "reasoning_effort",
        ),
    ];

    for (config, missing_field) in cases {
        let error = toml::from_str::<ChelixConfig>(config)
            .expect_err("incomplete agent configuration must be rejected");
        assert!(
            error.to_string().contains(missing_field),
            "expected missing {missing_field} error, got: {error}"
        );
    }
}

#[test]
fn agent_rejects_arbitrary_extra_key() {
    let result = validate_toml_str(
        r#"
[agents]
default = "quick"

[agents.quick]
name = "Quick"
model = "test::model"
reasoning_effort = "off"
max_tools_threshold = 128
compaction_reminder = true
prepend_sender_badge = true
arbitrary_extra_key = true
"#,
    );

    assert!(result.diagnostics.iter().any(|diagnostic| {
        diagnostic.severity == Severity::Error
            && diagnostic.category == DiagnosticCategory::UnknownField
            && diagnostic.path == "agents.quick.arbitrary_extra_key"
    }));
}

#[test]
fn agent_runtime_limits_max_tool_result_bytes_falls_back_to_global() {
    let config: ChelixConfig = toml::from_str(
        r#"
[tools]
max_tool_result_bytes = 12345

[agents.quick]
name = "Quick"
model = "test::model"
reasoning_effort = "off"
max_tools_threshold = 128
compaction_reminder = true
prepend_sender_badge = true
"#,
    )
    .unwrap();

    let limits = config.agent_runtime_limits("quick").unwrap();
    assert_eq!(limits.max_tool_result_bytes, 12345);
    assert_eq!(
        limits.max_tool_result_bytes_source,
        AgentRuntimeLimitSource::GlobalTools
    );
}

#[test]
fn agent_runtime_limits_max_tool_result_bytes_uses_agent_override() {
    let config: ChelixConfig = toml::from_str(
        r#"
[tools]
max_tool_result_bytes = 12345

[agents.quick]
name = "Quick"
model = "test::model"
reasoning_effort = "off"
max_tools_threshold = 128
compaction_reminder = true
prepend_sender_badge = true
max_tool_result_bytes = 999
"#,
    )
    .unwrap();

    let limits = config.agent_runtime_limits("quick").unwrap();
    assert_eq!(limits.max_tool_result_bytes, 999);
    assert_eq!(
        limits.max_tool_result_bytes_source,
        AgentRuntimeLimitSource::Agent
    );
}

#[test]
fn agent_max_tool_result_bytes_is_valid_config_key() {
    let result = validate_toml_str(
        r#"
[agents]
default = "quick"

[agents.quick]
name = "Quick"
model = "test::model"
reasoning_effort = "off"
max_tools_threshold = 128
compaction_reminder = true
prepend_sender_badge = true
max_tool_result_bytes = 100000

[tools.execute_command]
terminal_size = "115x58"

[sandbox]
archived_session_retention_days = 3
"#,
    );
    assert!(
        result
            .diagnostics
            .iter()
            .all(|diagnostic| diagnostic.severity != Severity::Error),
        "unexpected diagnostics: {:?}",
        result.diagnostics
    );
}

#[test]
fn agent_tools_preload_is_valid_config_key() {
    let result = validate_toml_str(
        r#"
[agents]
default = "quick"

[agents.quick]
name = "Quick"
model = "test::model"
reasoning_effort = "off"
max_tools_threshold = 128
compaction_reminder = true
prepend_sender_badge = true

[agents.quick.tools]
preload = ["read_file", "ripgrep"]

[tools.execute_command]
terminal_size = "115x58"

[sandbox]
archived_session_retention_days = 3
"#,
    );
    assert!(
        result
            .diagnostics
            .iter()
            .all(|diagnostic| diagnostic.severity != Severity::Error),
        "unexpected diagnostics: {:?}",
        result.diagnostics
    );
}

#[test]
fn reasoning_effort_accepts_provider_defined_value() {
    let result = validate_toml_str(
        r#"
[agents.thinker]
name = "Thinker"
model = "anthropic::claude-opus-4-5-20251101"
max_tools_threshold = 128
compaction_reminder = true
prepend_sender_badge = true
reasoning_effort = "ultra"
"#,
    );
    let errors: Vec<_> = result
        .diagnostics
        .iter()
        .filter(|diagnostic| {
            diagnostic.path.contains("reasoning_effort") && diagnostic.severity == Severity::Error
        })
        .collect();
    assert!(
        errors.is_empty(),
        "provider-defined effort should pass schema validation: {errors:?}"
    );
}

#[test]
fn reasoning_effort_is_recognized_in_schema() {
    let result = validate_toml_str(
        r#"
[agents.thinker]
name = "Thinker"
model = "test::model"
max_tools_threshold = 128
compaction_reminder = true
prepend_sender_badge = true
reasoning_effort = "high"
"#,
    );
    let unknown = result.diagnostics.iter().find(|diagnostic| {
        diagnostic.category == DiagnosticCategory::UnknownField
            && diagnostic.message.contains("reasoning_effort")
    });
    assert!(
        unknown.is_none(),
        "reasoning_effort should be recognized: {:?}",
        result.diagnostics
    );
}
