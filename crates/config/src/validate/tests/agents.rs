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
fn agent_requires_model_and_reasoning_effort() {
    let cases = [
        (
            r#"
[agents.quick]
name = "Quick"
reasoning_effort = "off"
max_tools_threshold = 128
"#,
            "model",
        ),
        (
            r#"
[agents.quick]
name = "Quick"
model = "test::model"
max_tools_threshold = 128
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
fn agent_rejects_empty_model_and_reasoning_effort() {
    let cases = [
        (
            "model = \"\"\nreasoning_effort = \"off\"",
            "agents.quick.model",
        ),
        (
            "model = \"test::model\"\nreasoning_effort = \"\"",
            "agents.quick.reasoning_effort",
        ),
    ];

    for (selection, expected_path) in cases {
        let result = validate_toml_str(&format!(
            "[agents]\ndefault = \"quick\"\n\n[agents.quick]\nname = \"Quick\"\n{selection}\nmax_tools_threshold = 128\n"
        ));
        assert!(
            result.diagnostics.iter().any(|diagnostic| {
                diagnostic.severity == Severity::Error && diagnostic.path == expected_path
            }),
            "expected error at {expected_path}, got: {:?}",
            result.diagnostics
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
arbitrary_extra_key = true
"#,
    );

    assert!(result.diagnostics.iter().any(|diagnostic| {
        diagnostic.severity == Severity::Error
            && diagnostic.category == "unknown-field"
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
model = "test::model"
reasoning_effort = "off"
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
model = "test::model"
reasoning_effort = "off"
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
    config.agents.entries.insert(
        "default".to_string(),
        crate::AgentConfig::new(
            "Reserved",
            "test::model",
            crate::schema::ReasoningEffort::from("off"),
        ),
    );
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
model = "test::model"
reasoning_effort = "off"
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
fn reasoning_effort_accepts_provider_defined_value() {
    let result = validate_toml_str(
        r#"
[agents.thinker]
name = "Thinker"
model = "anthropic::claude-opus-4-5-20251101"
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
model = "test::model"
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
