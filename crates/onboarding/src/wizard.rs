//! Terminal-based onboarding wizard using the shared state machine.

use std::io::{BufRead, Write};

use chelix_config::{AgentConfig, find_or_default_config_path};

use crate::{Context, Error, Result, state::WizardState};

/// Run the interactive onboarding wizard in the terminal.
pub async fn run_onboarding() -> Result<()> {
    let config_path = find_or_default_config_path();
    if !config_path.exists() {
        return Err(Error::message(format!(
            "onboarding config does not exist: {}",
            config_path.display()
        )));
    }

    let mut config = chelix_config::loader::load_config(&config_path)
        .context("failed to load existing onboarding config")?;
    let default_id = config.agents.default.clone();
    let default_agent = configured_default_agent(&config)?.clone();
    let user = chelix_config::resolve_user_profile_from_config(&config);

    if !default_agent.name.trim().is_empty() && user.name.is_some() {
        println!(
            "Already onboarded as {} with agent {}.",
            user.name.as_deref().unwrap_or(""),
            default_agent.name,
        );
        return Ok(());
    }

    let mut state = WizardState::new();
    state.agent = default_agent;
    state.user = user;

    let stdin = std::io::stdin();
    let mut reader = stdin.lock();

    while !state.is_done() {
        println!("{}", state.prompt());
        print!("> ");
        std::io::stdout().flush()?;
        let mut line = String::new();
        reader.read_line(&mut line)?;
        state.advance(&line);
    }

    config.agents.entries.insert(default_id, state.agent);
    config.user = state.user;

    chelix_config::loader::save_config_to_path(&config_path, &config)
        .context("failed to save onboarding config")?;
    chelix_config::save_user_with_mode(&config.user, config.memory.user_profile_write_mode)
        .context("failed to save user profile")?;
    println!("Config saved to {}", config_path.display());
    println!("Onboarding complete!");
    Ok(())
}

/// Return the configured default agent without normalization or fallback.
fn configured_default_agent(config: &chelix_config::ChelixConfig) -> Result<&AgentConfig> {
    let default_id = config.agents.default.as_str();
    if default_id.trim().is_empty() {
        return Err(Error::message("agents.default is empty"));
    }
    config.agents.entries.get(default_id).ok_or_else(|| {
        Error::message(format!(
            "default agent \"{default_id}\" is not defined under [agents]"
        ))
    })
}
