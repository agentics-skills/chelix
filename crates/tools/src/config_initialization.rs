use {
    chelix_config::{ChelixConfig, TerminalSizeConfig},
    chelix_config_service::ConfigOrigin,
};

pub fn initialize_config(
    config: &mut ChelixConfig,
    origin: ConfigOrigin,
) -> chelix_config::Result<()> {
    if origin == ConfigOrigin::Created {
        config.tools.execute_command.terminal_size = Some(TerminalSizeConfig {
            cols: 115,
            rows: 58,
        });
        config.sandbox.archived_session_retention_days = Some(7);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_document_receives_terminal_and_retention_defaults() -> chelix_config::Result<()> {
        let mut config = ChelixConfig::default();
        initialize_config(&mut config, ConfigOrigin::Created)?;
        assert_eq!(
            config.tools.execute_command.terminal_size,
            Some(TerminalSizeConfig {
                cols: 115,
                rows: 58,
            })
        );
        assert_eq!(config.sandbox.archived_session_retention_days, Some(7));
        Ok(())
    }

    #[test]
    fn existing_document_keeps_its_terminal_and_retention_values() -> chelix_config::Result<()> {
        for (terminal, retention) in [
            (None, None),
            (Some(TerminalSizeConfig { cols: 80, rows: 24 }), Some(3)),
        ] {
            let mut config = ChelixConfig::default();
            config.tools.execute_command.terminal_size = terminal;
            config.sandbox.archived_session_retention_days = retention;
            initialize_config(&mut config, ConfigOrigin::Existing)?;
            assert_eq!(config.tools.execute_command.terminal_size, terminal);
            assert_eq!(config.sandbox.archived_session_retention_days, retention);
        }
        Ok(())
    }
}
