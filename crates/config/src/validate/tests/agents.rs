use {
    super::*,
    crate::{AgentRuntimeLimitSource, AgentRuntimeLimits},
};

#[test]
fn agent_runtime_limits_use_required_threshold_and_global_timeout() {
    let config: ChelixConfig = toml::from_str(
        r#"
[tools]
agent_timeout_secs = 120

[agents.quick]
name = "Quick"
model = "openai/gpt-5.2"
max_tools_threshold = 11
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
timeout_secs = 5
max_tools_threshold = 11
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
model = "openai/gpt-5.2"
max_tools_threshold = 11
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
model = "openai/gpt-5.2"
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
fn spawned_agent_runtime_limits_preserve_default_no_timeout() {
    let config: ChelixConfig = toml::from_str(
        r#"
[agents.quick]
name = "Quick"
max_tools_threshold = 7
"#,
    )
    .unwrap();

    let agent = config.agents.get("quick").unwrap();
    let limits = AgentRuntimeLimits::resolve_for_spawned_agent(&config.tools, agent);
    assert_eq!(limits.timeout_secs, 0);
    assert_eq!(limits.max_tools_threshold, 7);
}

#[test]
fn spawned_agent_runtime_limits_ignore_global_timeout_without_agent_override() {
    let config: ChelixConfig = toml::from_str(
        r#"
[tools]
agent_timeout_secs = 1800

[agents.deep]
name = "Deep"
max_tools_threshold = 80
"#,
    )
    .unwrap();

    let agent = config.agents.get("deep").unwrap();
    let limits = AgentRuntimeLimits::resolve_for_spawned_agent(&config.tools, agent);
    assert_eq!(limits.timeout_secs, 0);
    assert_eq!(limits.timeout_source, AgentRuntimeLimitSource::GlobalTools);
    assert_eq!(limits.max_tools_threshold, 80);
}

#[test]
fn spawned_agent_runtime_limits_use_agent_timeout() {
    let config: ChelixConfig = toml::from_str(
        r#"
[tools]
agent_timeout_secs = 1800

[agents.deep]
name = "Deep"
timeout_secs = 600
max_tools_threshold = 80
"#,
    )
    .unwrap();

    let agent = config.agents.get("deep").unwrap();
    let limits = AgentRuntimeLimits::resolve_for_spawned_agent(&config.tools, agent);
    assert_eq!(limits.timeout_secs, 600);
    assert_eq!(limits.timeout_source, AgentRuntimeLimitSource::Agent);
    assert_eq!(limits.max_tools_threshold, 80);
}

#[test]
fn agent_runtime_limits_max_tool_result_bytes_falls_back_to_global() {
    let config: ChelixConfig = toml::from_str(
        r#"
[tools]
max_tool_result_bytes = 12345

[agents.quick]
name = "Quick"
model = "openai/gpt-5.2"
max_tools_threshold = 128
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
max_tools_threshold = 128
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
max_tools_threshold = 128
max_tool_result_bytes = 100000
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
max_tools_threshold = 128

[agents.quick.tools]
preload = ["read_file", "ripgrep"]
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
fn agent_max_tools_threshold_must_be_positive() {
    let result = validate_toml_str(
        r#"
[agents.quick]
name = "Quick"
max_tools_threshold = 0
"#,
    );
    assert!(result.diagnostics.iter().any(|diagnostic| {
        diagnostic.severity == Severity::Error
            && diagnostic.category == "invalid-value"
            && diagnostic.path == "agents.quick.max_tools_threshold"
    }));
}

#[test]
fn semantic_validation_rejects_reserved_agent_ids() {
    let mut config = ChelixConfig::default();
    config
        .agents
        .entries
        .insert("default".to_string(), crate::AgentConfig {
            name: "Reserved".to_string(),
            ..Default::default()
        });
    let mut diagnostics = Vec::new();

    crate::validate::semantic::check_semantic_warnings(&config, &mut diagnostics);

    assert!(diagnostics.iter().any(|diagnostic| {
        diagnostic.severity == Severity::Error
            && diagnostic.category == "invalid-value"
            && diagnostic.path == "agents.default"
            && diagnostic.message.contains("reserved")
    }));
}

#[test]
fn agents_default_must_reference_configured_agent() {
    let result = validate_toml_str(
        r#"
[agents]
default = "missing"

[agents.main]
name = "Main"
max_tools_threshold = 128
"#,
    );
    assert!(result.diagnostics.iter().any(|diagnostic| {
        diagnostic.severity == Severity::Error
            && diagnostic.path == "agents.default"
            && diagnostic.message.contains("missing")
    }));
}

#[test]
fn legacy_agent_keys_are_rejected() {
    for legacy in [
        "default_preset = \"research\"",
        "theme = \"focused\"",
        "delegate_only = true",
        "system_prompt_suffix = \"legacy\"",
    ] {
        let toml = if legacy.starts_with("default_preset") {
            format!("[agents]\n{legacy}\n")
        } else {
            format!("[agents.main]\nname = \"Main\"\nmax_tools_threshold = 128\n{legacy}\n")
        };
        let result = validate_toml_str(&toml);
        assert!(
            result
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.severity == Severity::Error),
            "legacy key should be rejected: {legacy}; diagnostics: {:?}",
            result.diagnostics
        );
    }
}

#[test]
fn reasoning_effort_accepts_provider_defined_value() {
    let result = validate_toml_str(
        r#"
[agents.thinker]
name = "Thinker"
model = "claude-opus-4-5-20251101"
max_tools_threshold = 128
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
max_tools_threshold = 128
reasoning_effort = "high"
"#,
    );
    let unknown = result.diagnostics.iter().find(|diagnostic| {
        diagnostic.category == "unknown-field" && diagnostic.message.contains("reasoning_effort")
    });
    assert!(
        unknown.is_none(),
        "reasoning_effort should be recognized: {:?}",
        result.diagnostics
    );
}

#[test]
fn external_agents_known_kinds_not_warned() {
    let toml = r#"
[external_agents]
enabled = true

[external_agents.agents.claude-code]
binary = "claude"

[external_agents.agents.codex]
binary = "codex"
"#;
    let result = validate_toml_str(toml);
    let warning = result.diagnostics.iter().find(|diagnostic| {
        diagnostic.path.starts_with("external_agents.agents.")
            && diagnostic.category == "unknown-field"
    });
    assert!(
        warning.is_none(),
        "known external agent kinds should not warn, got: {:?}",
        result.diagnostics
    );
}

#[test]
fn external_agents_unknown_kind_warned_with_suggestion() {
    let toml = r#"
[external_agents]
enabled = true

[external_agents.agents.claude_code]
binary = "claude"
"#;
    let result = validate_toml_str(toml);
    let warning = result
        .diagnostics
        .iter()
        .find(|diagnostic| {
            diagnostic.path == "external_agents.agents.claude_code"
                && diagnostic.category == "unknown-field"
        })
        .expect("unknown external agent kind should produce warning");
    assert!(
        warning.message.contains("Did you mean \"claude-code\"?"),
        "expected typo suggestion, got: {warning:?}"
    );
}
