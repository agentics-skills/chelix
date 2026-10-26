//! Agent identity, shape, and registry-state checks.

use chelix_config::{
    AgentsConfigStateError, ChelixConfig, Diagnostic, DiagnosticCategory, Severity,
    validate_agent_id,
};

/// Report whether an agent display name is empty or only whitespace.
#[must_use]
pub fn agent_name_is_empty(agent: &chelix_config::AgentConfig) -> bool {
    agent.name.trim().is_empty()
}

/// Report whether an agent tool budget is zero.
#[must_use]
pub fn agent_max_tools_threshold_is_zero(agent: &chelix_config::AgentConfig) -> bool {
    agent.max_tools_threshold == 0
}

/// Report agent ids rejected by [`validate_agent_id`].
#[must_use]
pub fn validate_load_config(config: &ChelixConfig) -> Vec<Diagnostic> {
    config
        .agents
        .entries
        .keys()
        .filter_map(|agent_id| {
            validate_agent_id(agent_id).err().map(|message| Diagnostic {
                severity: Severity::Error,
                summary_message: None,
                category: DiagnosticCategory::InvalidValue,
                path: format!("agents.{agent_id}"),
                message: format!("invalid agent id \"{agent_id}\": {message}"),
            })
        })
        .collect()
}

/// Report empty agent fields, illegal agent ids, and an unusable default agent.
#[must_use]
pub fn validate_config(config: &ChelixConfig) -> Vec<Diagnostic> {
    let mut diagnostics = Vec::new();
    for (name, agent) in &config.agents.entries {
        if agent_name_is_empty(agent) {
            diagnostics.push(Diagnostic {
                severity: Severity::Error,
                summary_message: None,
                category: DiagnosticCategory::InvalidValue,
                path: format!("agents.{name}.name"),
                message: "agent name must not be empty".into(),
            });
        }
        if agent.model.trim().is_empty() {
            diagnostics.push(Diagnostic {
                severity: Severity::Error,
                summary_message: None,
                category: DiagnosticCategory::InvalidValue,
                path: format!("agents.{name}.model"),
                message: "agent model must not be empty".into(),
            });
        }
        if agent.reasoning_effort.as_str().trim().is_empty() {
            diagnostics.push(Diagnostic {
                severity: Severity::Error,
                summary_message: None,
                category: DiagnosticCategory::InvalidValue,
                path: format!("agents.{name}.reasoning_effort"),
                message: "agent reasoning_effort must not be empty".into(),
            });
        }
        if agent_max_tools_threshold_is_zero(agent) {
            diagnostics.push(Diagnostic {
                severity: Severity::Error,
                summary_message: None,
                category: DiagnosticCategory::InvalidValue,
                path: format!("agents.{name}.max_tools_threshold"),
                message: "agents.<id>.max_tools_threshold must be at least 1".into(),
            });
        }
    }
    diagnostics.extend(validate_load_config(config));
    if let Err(error) = config.agents.resolve_state() {
        let category = match &error {
            AgentsConfigStateError::MissingDefault => DiagnosticCategory::MissingField,
            AgentsConfigStateError::DefaultNotConfigured { .. } => DiagnosticCategory::InvalidValue,
        };
        diagnostics.push(Diagnostic {
            severity: Severity::Error,
            summary_message: None,
            category,
            path: "agents.default".into(),
            message: error.to_string(),
        });
    }
    diagnostics
}

#[cfg(test)]
mod tests {
    use chelix_config::{AgentConfig, schema::ReasoningEffort};

    use super::*;

    fn check(config: &ChelixConfig) -> Vec<Diagnostic> {
        validate_config(config)
    }

    fn agent(name: &str, model: &str, effort: &str) -> AgentConfig {
        AgentConfig::new(name, model, ReasoningEffort::from(effort))
    }

    #[test]
    fn agent_rejects_empty_name_model_and_reasoning_effort() {
        let cases = [
            ("", "test::model", "off", "agents.quick.name"),
            ("   ", "test::model", "off", "agents.quick.name"),
            ("Quick", "", "off", "agents.quick.model"),
            ("Quick", "   ", "off", "agents.quick.model"),
            ("Quick", "test::model", "", "agents.quick.reasoning_effort"),
            (
                "Quick",
                "test::model",
                "   ",
                "agents.quick.reasoning_effort",
            ),
        ];
        for (name, model, effort, expected_path) in cases {
            let mut config = ChelixConfig::default();
            config.agents.default = "quick".into();
            config
                .agents
                .entries
                .insert("quick".into(), agent(name, model, effort));
            assert!(
                check(&config).iter().any(|diagnostic| {
                    diagnostic.severity == Severity::Error && diagnostic.path == expected_path
                }),
                "expected error at {expected_path}"
            );
        }
    }

    #[test]
    fn agent_max_tools_threshold_must_be_positive() {
        let mut config = ChelixConfig::default();
        config.agents.default = "quick".into();
        let mut quick = agent("Quick", "test::model", "off");
        quick.max_tools_threshold = 0;
        config.agents.entries.insert("quick".into(), quick);
        assert!(check(&config).iter().any(|diagnostic| {
            diagnostic.severity == Severity::Error
                && diagnostic.category == DiagnosticCategory::InvalidValue
                && diagnostic.path == "agents.quick.max_tools_threshold"
        }));
    }

    #[test]
    fn semantic_validation_rejects_reserved_agent_ids() {
        let mut config = ChelixConfig::default();
        config
            .agents
            .entries
            .insert("default".into(), agent("Reserved", "test::model", "off"));
        assert!(check(&config).iter().any(|diagnostic| {
            diagnostic.severity == Severity::Error
                && diagnostic.category == DiagnosticCategory::InvalidValue
                && diagnostic.path == "agents.default"
                && diagnostic.message.contains("reserved")
        }));
    }

    #[test]
    fn invalid_agent_id_shape_is_rejected() {
        let mut config = ChelixConfig::default();
        config.agents.default = "Bad".into();
        config
            .agents
            .entries
            .insert("Bad".into(), agent("Bad", "test::model", "off"));
        assert!(check(&config).iter().any(|diagnostic| {
            diagnostic.severity == Severity::Error
                && diagnostic.path == "agents.Bad"
                && diagnostic.message.contains("lowercase")
        }));
    }

    #[test]
    fn agents_default_must_reference_configured_agent() {
        let mut config = ChelixConfig::default();
        config.agents.default = "missing".into();
        config
            .agents
            .entries
            .insert("main".into(), agent("Main", "test::model", "off"));
        assert!(check(&config).iter().any(|diagnostic| {
            diagnostic.severity == Severity::Error
                && diagnostic.category == DiagnosticCategory::InvalidValue
                && diagnostic.path == "agents.default"
                && diagnostic.message.contains("missing")
        }));
    }

    #[test]
    fn agents_default_is_required_when_entries_exist() {
        let mut config = ChelixConfig::default();
        config
            .agents
            .entries
            .insert("main".into(), agent("Main", "test::model", "off"));
        assert!(check(&config).iter().any(|diagnostic| {
            diagnostic.severity == Severity::Error
                && diagnostic.category == DiagnosticCategory::MissingField
                && diagnostic.path == "agents.default"
        }));
    }

    #[test]
    fn empty_agent_registry_is_accepted() {
        assert!(check(&ChelixConfig::default()).is_empty());
    }
}
