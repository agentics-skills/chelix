//! Command terminal, sandbox retention, mount, and GPU checks.

use chelix_config::{ChelixConfig, Diagnostic, DiagnosticCategory, Severity, schema::SandboxMode};

/// Report a missing execute-command terminal size and missing sandbox retention.
#[must_use]
pub fn validate_load_config(config: &ChelixConfig) -> Vec<Diagnostic> {
    let mut diagnostics = Vec::new();
    check_terminal_size(config, &mut diagnostics);
    check_sandbox_retention(config, &mut diagnostics);
    diagnostics
}

/// Report terminal size, sandbox retention, mount, and GPU diagnostics.
#[must_use]
pub fn validate_config(config: &ChelixConfig) -> Vec<Diagnostic> {
    let mut diagnostics = Vec::new();
    check_terminal_size(config, &mut diagnostics);
    check_sandbox_mode(config, &mut diagnostics);
    check_sandbox_retention(config, &mut diagnostics);
    check_sandbox_scope(config, &mut diagnostics);
    check_sandbox_mounts(config, &mut diagnostics);
    check_sandbox_gpus(config, &mut diagnostics);
    diagnostics
}

/// Editor warning when sandbox execution is not isolated.
#[must_use]
pub fn configuration_editor_warning(config: &ChelixConfig) -> Option<&'static str> {
    if config.sandbox.mode == SandboxMode::Off {
        Some("Sandbox mode is off. Commands will run directly on host without isolation.")
    } else {
        None
    }
}

fn check_terminal_size(config: &ChelixConfig, diagnostics: &mut Vec<Diagnostic>) {
    if config.tools.execute_command.terminal_size.is_none() {
        diagnostics.push(Diagnostic {
            severity: Severity::Error,
            summary_message: None,
            category: DiagnosticCategory::MissingField,
            path: "tools.execute_command.terminal_size".into(),
            message: "tools.execute_command.terminal_size is required".into(),
        });
    }
}

fn check_sandbox_mode(config: &ChelixConfig, diagnostics: &mut Vec<Diagnostic>) {
    if config.sandbox.mode == SandboxMode::Off {
        diagnostics.push(Diagnostic {
            severity: Severity::Warning,
            summary_message: None,
            category: DiagnosticCategory::Security,
            path: "sandbox.mode".into(),
            message: "sandbox mode is disabled — commands run without isolation".into(),
        });
    }
}

fn check_sandbox_retention(config: &ChelixConfig, diagnostics: &mut Vec<Diagnostic>) {
    if config.sandbox.mode == SandboxMode::On
        && config.sandbox.archived_session_retention_days.is_none()
    {
        diagnostics.push(Diagnostic {
            severity: Severity::Error,
            summary_message: None,
            category: DiagnosticCategory::MissingField,
            path: "sandbox.archived_session_retention_days".into(),
            message: "sandbox.archived_session_retention_days is required when sandbox.mode is On"
                .into(),
        });
    }
}

fn check_sandbox_scope(config: &ChelixConfig, diagnostics: &mut Vec<Diagnostic>) {
    let valid_sandbox_scopes = ["session", "agent", "shared"];
    if !valid_sandbox_scopes.contains(&config.sandbox.scope.as_str()) {
        diagnostics.push(Diagnostic {
            severity: Severity::Warning,
            summary_message: None,
            category: DiagnosticCategory::UnknownField,
            path: "sandbox.scope".into(),
            message: format!(
                "unknown sandbox scope \"{}\"; expected one of: {}",
                config.sandbox.scope,
                valid_sandbox_scopes.join(", ")
            ),
        });
    }

    let valid_workspace_sysmounts = ["ro", "rw"];
    if !valid_workspace_sysmounts.contains(&config.sandbox.workspace_sysmount.as_str()) {
        diagnostics.push(Diagnostic {
            severity: Severity::Warning,
            summary_message: None,
            category: DiagnosticCategory::UnknownField,
            path: "sandbox.workspace_sysmount".into(),
            message: format!(
                "unknown workspace_sysmount \"{}\"; expected one of: {}",
                config.sandbox.workspace_sysmount,
                valid_workspace_sysmounts.join(", ")
            ),
        });
    }
}

fn check_sandbox_mounts(config: &ChelixConfig, diagnostics: &mut Vec<Diagnostic>) {
    let validation =
        match chelix_config::container_mounts::sandbox_mount_validation(&config.sandbox) {
            Ok(validation) => validation,
            Err(error) => {
                diagnostics.push(Diagnostic {
                    severity: Severity::Error,
                    summary_message: None,
                    category: DiagnosticCategory::InvalidValue,
                    path: "CHELIX_CONFIG_DIR".into(),
                    message: error.to_string(),
                });
                return;
            },
        };

    if validation.data_mount_exposes_config {
        diagnostics.push(Diagnostic {
            severity: Severity::Error,
            summary_message: None,
            category: DiagnosticCategory::Security,
            path: "sandbox.host_data_dir".into(),
            message: "sandbox data_dir mount overlaps the active config_dir and could expose credentials.json or chelix.toml".into(),
        });
    }
    if validation.shared_home_exposes_config {
        diagnostics.push(Diagnostic {
            severity: Severity::Error,
            summary_message: None,
            category: DiagnosticCategory::Security,
            path: "sandbox.shared_home_dir".into(),
            message: "sandbox shared_home_dir overlaps the active config_dir and could expose credentials.json or chelix.toml".into(),
        });
    }
    if validation.managed_guest_mounts_conflict {
        diagnostics.push(Diagnostic {
            severity: Severity::Error,
            summary_message: None,
            category: DiagnosticCategory::InvalidValue,
            path: "sandbox.home_persistence".into(),
            message: "managed sandbox mount targets overlap the mandatory data_dir guest namespace"
                .into(),
        });
    }
    for rejected in validation.rejected_custom_mounts {
        let violation = rejected.violation;
        diagnostics.push(Diagnostic {
            severity: Severity::Error,
            summary_message: Some(violation.safe_description().to_owned()),
            category: DiagnosticCategory::InvalidValue,
            path: format!(
                "sandbox.mounts[{}].{}",
                rejected.index,
                violation.kind.field()
            ),
            message: violation.message,
        });
    }
}

fn check_sandbox_gpus(config: &ChelixConfig, diagnostics: &mut Vec<Diagnostic>) {
    let Some(gpus) = config.sandbox.gpus.as_deref() else {
        return;
    };
    let valid = gpus == "all"
        || gpus.starts_with("device=")
        || gpus.chars().all(|c| c.is_ascii_digit() || c == ',');
    if !valid {
        diagnostics.push(Diagnostic {
            severity: Severity::Warning,
            summary_message: None,
            category: DiagnosticCategory::UnknownField,
            path: "sandbox.gpus".into(),
            message: format!(
                "unrecognized gpus value \"{gpus}\"; expected \"all\", \"device=0\", \"device=0,1\", or a device index",
            ),
        });
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use chelix_config::{
        TerminalSizeConfig,
        schema::{HomePersistenceConfig, MountMode, SandboxMount},
    };

    use super::*;

    fn check(config: &ChelixConfig) -> Vec<Diagnostic> {
        validate_config(config)
    }

    fn with_terminal(mut config: ChelixConfig) -> ChelixConfig {
        config.tools.execute_command.terminal_size = Some(TerminalSizeConfig {
            cols: 115,
            rows: 58,
        });
        config
    }

    #[test]
    fn editor_warning_tracks_sandbox_mode() {
        assert_eq!(configuration_editor_warning(&ChelixConfig::default()), None);
        let mut config = ChelixConfig::default();
        config.sandbox.mode = SandboxMode::Off;
        assert_eq!(
            configuration_editor_warning(&config),
            Some("Sandbox mode is off. Commands will run directly on host without isolation.")
        );
    }

    fn mount(host: &str, guest: &str, mode: MountMode) -> SandboxMount {
        SandboxMount {
            host: PathBuf::from(host),
            guest: PathBuf::from(guest),
            mode,
        }
    }

    #[test]
    fn terminal_size_is_required() {
        let missing = check(&ChelixConfig::default());
        assert!(missing.iter().any(|diagnostic| {
            diagnostic.path == "tools.execute_command.terminal_size"
                && diagnostic.category == DiagnosticCategory::MissingField
                && diagnostic.severity == Severity::Error
        }));
        let present = check(&with_terminal(ChelixConfig::default()));
        assert!(
            present
                .iter()
                .all(|diagnostic| diagnostic.path != "tools.execute_command.terminal_size")
        );
    }

    #[test]
    fn sandbox_mode_requires_retention_only_when_on() {
        let mut disabled = with_terminal(ChelixConfig::default());
        disabled.sandbox.mode = SandboxMode::Off;
        let diagnostics = check(&disabled);
        assert!(diagnostics.iter().any(|diagnostic| {
            diagnostic.path == "sandbox.mode" && diagnostic.severity == Severity::Warning
        }));
        assert!(
            diagnostics
                .iter()
                .all(|diagnostic| diagnostic.severity != Severity::Error)
        );

        for mode in [SandboxMode::On, SandboxMode::default()] {
            let mut config = with_terminal(ChelixConfig::default());
            config.sandbox.mode = mode;
            config.sandbox.archived_session_retention_days = None;
            assert!(check(&config).iter().any(|diagnostic| {
                diagnostic.path == "sandbox.archived_session_retention_days"
                    && diagnostic.category == DiagnosticCategory::MissingField
                    && diagnostic.severity == Severity::Error
                    && diagnostic
                        .message
                        .contains("required when sandbox.mode is On")
            }));
        }
    }

    #[test]
    fn zero_retention_days_is_explicit_when_sandbox_is_on() {
        let mut config = with_terminal(ChelixConfig::default());
        config.sandbox.mode = SandboxMode::On;
        config.sandbox.archived_session_retention_days = Some(0);
        assert!(
            check(&config)
                .iter()
                .all(|diagnostic| diagnostic.path != "sandbox.archived_session_retention_days")
        );
    }

    #[test]
    fn configured_sandbox_without_mount_overrides_is_accepted() {
        let mut config = with_terminal(ChelixConfig::default());
        config.sandbox.archived_session_retention_days = Some(3);
        assert!(
            check(&config)
                .iter()
                .all(|diagnostic| diagnostic.severity != Severity::Error)
        );
    }

    #[test]
    fn unknown_scope_and_sysmount_are_warned() {
        let mut config = with_terminal(ChelixConfig::default());
        config.sandbox.archived_session_retention_days = Some(3);
        config.sandbox.scope = "cluster".into();
        config.sandbox.workspace_sysmount = "sideways".into();
        let diagnostics = check(&config);
        assert!(diagnostics.iter().any(|diagnostic| {
            diagnostic.path == "sandbox.scope"
                && diagnostic.severity == Severity::Warning
                && diagnostic.message.contains("cluster")
        }));
        assert!(diagnostics.iter().any(|diagnostic| {
            diagnostic.path == "sandbox.workspace_sysmount"
                && diagnostic.severity == Severity::Warning
                && diagnostic.message.contains("sideways")
        }));
    }

    #[test]
    fn valid_declarative_sandbox_mount_is_accepted() {
        let mut config = with_terminal(ChelixConfig::default());
        config.sandbox.archived_session_retention_days = Some(3);
        config
            .sandbox
            .mounts
            .push(mount("/srv/reference", "/mnt/reference", MountMode::Ro));
        assert!(
            check(&config)
                .iter()
                .all(|diagnostic| !diagnostic.path.starts_with("sandbox.mounts"))
        );
    }

    #[test]
    fn sandbox_data_dir_source_cannot_use_a_different_guest_path() {
        let mut config = with_terminal(ChelixConfig::default());
        config.sandbox.host_data_dir = Some("/host/chelix-data".into());
        config
            .sandbox
            .mounts
            .push(mount("/host/chelix-data", "/different/data", MountMode::Rw));
        assert!(check(&config).iter().any(|diagnostic| {
            diagnostic.path == "sandbox.mounts[0].guest"
                && diagnostic.severity == Severity::Error
                && diagnostic.message.contains("identical agent path")
        }));
    }

    #[test]
    fn sandbox_custom_mount_paths_must_be_absolute() {
        let mut config = with_terminal(ChelixConfig::default());
        config
            .sandbox
            .mounts
            .push(mount("relative/source", "/mnt/source", MountMode::Ro));
        assert!(check(&config).iter().any(|diagnostic| {
            diagnostic.path == "sandbox.mounts[0].host"
                && diagnostic.severity == Severity::Error
                && diagnostic.message.contains("must be absolute")
                && diagnostic.summary_message.as_deref() == Some("host path is not absolute")
        }));
    }

    #[test]
    fn sandbox_custom_mount_diagnostics_preserve_configured_index() {
        let mut config = with_terminal(ChelixConfig::default());
        config
            .sandbox
            .mounts
            .push(mount("/srv/reference", "/mnt/reference", MountMode::Ro));
        config
            .sandbox
            .mounts
            .push(mount("/srv/invalid", "relative/guest", MountMode::Rw));
        assert!(check(&config).iter().any(|diagnostic| {
            diagnostic.path == "sandbox.mounts[1].guest"
                && diagnostic.severity == Severity::Error
                && diagnostic.message.contains("must be absolute")
        }));
    }

    #[test]
    fn sandbox_data_mount_cannot_source_config_dir() {
        let mut config = with_terminal(ChelixConfig::default());
        config.sandbox.mode = SandboxMode::Off;
        config.sandbox.host_data_dir = Some("/".into());
        assert!(check(&config).iter().any(|diagnostic| {
            diagnostic.path == "sandbox.host_data_dir"
                && diagnostic.severity == Severity::Error
                && diagnostic.category == DiagnosticCategory::Security
        }));
    }

    #[test]
    fn sandbox_shared_home_cannot_source_config_dir() {
        let mut config = with_terminal(ChelixConfig::default());
        config.sandbox.home_persistence = HomePersistenceConfig::Shared;
        config.sandbox.shared_home_dir = Some("/".into());
        assert!(check(&config).iter().any(|diagnostic| {
            diagnostic.path == "sandbox.shared_home_dir"
                && diagnostic.severity == Severity::Error
                && diagnostic.category == DiagnosticCategory::Security
        }));
    }

    #[test]
    fn sandbox_gpus_accepts_known_forms_and_warns_otherwise() {
        for gpus in ["all", "device=0", "device=0,1", "0,1"] {
            let mut config = with_terminal(ChelixConfig::default());
            config.sandbox.archived_session_retention_days = Some(3);
            config.sandbox.gpus = Some(gpus.into());
            assert!(
                check(&config)
                    .iter()
                    .all(|diagnostic| diagnostic.path != "sandbox.gpus"),
                "{gpus}"
            );
        }
        let mut config = with_terminal(ChelixConfig::default());
        config.sandbox.gpus = Some("bogus".into());
        assert!(check(&config).iter().any(|diagnostic| {
            diagnostic.path == "sandbox.gpus"
                && diagnostic.severity == Severity::Warning
                && diagnostic.message.contains("bogus")
        }));
    }
}
