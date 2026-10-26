use std::sync::Arc;

use chelix_config_service::{
    BlockingValidator, ConfigInitialization, ConfigValidation, ConfigValidator, SyncValidator,
};

/// Owner checks for configuration admission.
///
/// Ordinary open, commit, and refresh use the load profile. Agent and UI
/// saves use the full profile.
#[must_use]
pub fn validators() -> ConfigValidation {
    ConfigValidation::new(
        vec![
            Arc::new(SyncValidator(
                chelix_memory::config_validation::validate_config,
            )),
            Arc::new(SyncValidator(
                chelix_providers::config_validation::validate_config,
            )),
            Arc::new(SyncValidator(
                chelix_auth::config_validation::validate_config,
            )),
            Arc::new(BlockingValidator(
                chelix_tls::config_validation::validate_config,
            )),
            Arc::new(BlockingValidator(
                chelix_tools::config_validation::validate_config,
            )),
            Arc::new(SyncValidator(
                chelix_agents::config_validation::validate_config,
            )),
            Arc::new(SyncValidator(
                chelix_chat::config_validation::validate_config,
            )),
            Arc::new(SyncValidator(
                chelix_context7::config_validation::validate_config,
            )),
            Arc::new(SyncValidator(
                chelix_linkup::config_validation::validate_config,
            )),
            Arc::new(SyncValidator(
                chelix_exa::config_validation::validate_config,
            )),
            Arc::new(SyncValidator(
                chelix_google::config_validation::validate_config,
            )),
            Arc::new(SyncValidator(
                chelix_felo::config_validation::validate_config,
            )),
            Arc::new(SyncValidator(
                chelix_duckduckgo::config_validation::validate_config,
            )),
            Arc::new(SyncValidator(
                chelix_github::config_validation::validate_config,
            )),
            Arc::new(BlockingValidator(
                chelix_mcp::config_validation::validate_config,
            )),
            Arc::new(SyncValidator(
                chelix_channels::config_validation::validate_config,
            )),
            Arc::new(SyncValidator(
                chelix_voice::config_validation::validate_config,
            )),
            Arc::new(SyncValidator(
                chelix_plugins::config_validation::validate_config,
            )),
            Arc::new(SyncValidator(crate::config_validation::validate_config)),
        ],
        load_validators(),
    )
}

/// Hard checks for ordinary open, commit, and refresh.
fn load_validators() -> Vec<Arc<dyn ConfigValidator>> {
    vec![
        Arc::new(SyncValidator(
            chelix_providers::config_validation::validate_load_config,
        )),
        Arc::new(SyncValidator(
            chelix_agents::config_validation::validate_load_config,
        )),
        Arc::new(SyncValidator(
            chelix_channels::config_validation::validate_config,
        )),
        Arc::new(SyncValidator(
            chelix_context7::config_validation::validate_config,
        )),
        Arc::new(SyncValidator(
            chelix_linkup::config_validation::validate_config,
        )),
        Arc::new(SyncValidator(
            chelix_exa::config_validation::validate_config,
        )),
        Arc::new(SyncValidator(
            chelix_google::config_validation::validate_config,
        )),
        Arc::new(SyncValidator(
            chelix_felo::config_validation::validate_config,
        )),
        Arc::new(SyncValidator(
            chelix_tools::config_validation::validate_load_config,
        )),
    ]
}

#[must_use]
pub fn initializers() -> ConfigInitialization {
    ConfigInitialization::new(vec![
        chelix_tools::config_initialization::initialize_config,
        crate::config_initialization::initialize_config,
    ])
}

#[cfg(test)]
mod tests {
    use {
        super::*,
        chelix_config::{ChelixConfig, Severity},
    };

    fn owner_diagnostic_fixture() -> ChelixConfig {
        use std::collections::HashMap;

        let mut config = ChelixConfig::default();
        config.server.port = 8080;
        config.server.bind = "0.0.0.0".into();
        config.server.external_url = Some("ftp://chelix.example.com".into());
        config.auth.disabled = true;
        config.memory.url = Some("http://127.0.0.1:8080".into());
        config.memory.api_key = None;
        config.memory.dimensions = None;
        config.providers.providers.insert(
            "voice-openai".into(),
            chelix_config::schema::ProviderEntry::default(),
        );
        config.tls.public_ip = Some("chat.example.com".into());
        config.tools.execute_command.terminal_size = None;
        config.sandbox.mode = chelix_config::schema::SandboxMode::On;
        config.sandbox.archived_session_retention_days = None;
        config.agents.default = "main".into();
        config.agents.entries.insert(
            "main".into(),
            chelix_config::AgentConfig::new(
                "",
                "test::model",
                chelix_config::schema::ReasoningEffort::from("off"),
            ),
        );
        config.agents.entries.insert(
            "bad_id".into(),
            chelix_config::AgentConfig::new(
                "Bad",
                "test::model",
                chelix_config::schema::ReasoningEffort::from("off"),
            ),
        );
        config.chat.workspace_file_max_chars = 0;
        config.tools.context7.request_timeout_secs = 0;
        config.tools.linkup.request_timeout_secs = 0;
        config.tools.exa.request_timeout_secs = 0;
        config.tools.google.request_timeout_secs = 0;
        config.tools.felo.request_timeout_secs = 0;
        config.tools.duckduckgo.request_timeout_secs = 0;
        config.tools.github.request_timeout_secs = 0;
        config.mcp.request_timeout_secs = 0;
        config.channels.offered = vec!["unknown_type".into()];
        config.voice.tts.providers = vec!["not-a-provider".into()];
        config.hooks = Some(chelix_config::schema::HooksConfig {
            hooks: vec![chelix_config::schema::ShellHookConfigEntry {
                name: "test".into(),
                command: "echo test".into(),
                events: vec!["NotAnEvent".into()],
                timeout: 10,
                env: HashMap::new(),
            }],
        });
        config
    }

    #[tokio::test]
    async fn validators_diagnose_reports_assembled_owner_paths() {
        let config = owner_diagnostic_fixture();
        let raw = toml::to_string(&config)
            .unwrap_or_else(|error| panic!("fixture config should serialize: {error}"));
        let result = validators()
            .diagnose(&raw, std::path::Path::new("chelix.toml"))
            .await;
        assert!(result.has_errors());
        let expected = [
            ("memory", Severity::Error),
            ("providers.voice-openai", Severity::Error),
            ("auth", Severity::Warning),
            ("tls.public_ip", Severity::Error),
            ("tools.execute_command.terminal_size", Severity::Error),
            ("agents.main.name", Severity::Error),
            ("chat.workspace_file_max_chars", Severity::Warning),
            ("tools.context7.request_timeout_secs", Severity::Error),
            ("tools.linkup.request_timeout_secs", Severity::Error),
            ("tools.exa.request_timeout_secs", Severity::Error),
            ("tools.google.request_timeout_secs", Severity::Error),
            ("tools.felo.request_timeout_secs", Severity::Error),
            ("tools.duckduckgo.request_timeout_secs", Severity::Error),
            ("tools.github.request_timeout_secs", Severity::Error),
            ("mcp.request_timeout_secs", Severity::Error),
            ("channels.offered[0]", Severity::Error),
            ("voice.tts.providers[0]", Severity::Warning),
            ("hooks.hooks[0].events[0]", Severity::Warning),
            ("server.external_url", Severity::Error),
        ];
        for (path, severity) in expected {
            assert!(
                result.diagnostics.iter().any(|diagnostic| {
                    diagnostic.path == path && diagnostic.severity == severity
                }),
                "missing {severity:?} at {path}"
            );
        }
    }

    #[tokio::test]
    async fn load_validators_report_startup_hard_paths() {
        use std::collections::BTreeSet;

        let config = Arc::new(owner_diagnostic_fixture());
        let mut diagnostics = Vec::new();
        for check in load_validators() {
            diagnostics.extend(check.validate(Arc::clone(&config)).await);
        }
        let paths = diagnostics
            .iter()
            .filter(|diagnostic| diagnostic.severity == Severity::Error)
            .map(|diagnostic| diagnostic.path.as_str())
            .collect::<BTreeSet<_>>();
        let expected = BTreeSet::from([
            "agents.bad_id",
            "channels.offered[0]",
            "providers.voice-openai",
            "sandbox.archived_session_retention_days",
            "tools.context7.request_timeout_secs",
            "tools.exa.request_timeout_secs",
            "tools.execute_command.terminal_size",
            "tools.felo.request_timeout_secs",
            "tools.google.request_timeout_secs",
            "tools.linkup.request_timeout_secs",
        ]);
        assert_eq!(paths, expected);
    }
}
