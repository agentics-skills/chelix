//! Terminal-based onboarding wizard using the shared state machine.

use std::io::{BufRead, Write};

use chelix_config::{AgentConfig, find_or_default_config_path};

use crate::{
    Context, Error, Result,
    state::{AgentIdentityDraft, WizardState},
};

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
    state.agent = AgentIdentityDraft::from_agent(&default_agent);
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

    let default_agent = config
        .agents
        .entries
        .get_mut(&default_id)
        .ok_or_else(|| Error::message("configured default agent disappeared"))?;
    state.agent.apply_to(default_agent);
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
    match config
        .agents
        .resolve_state()
        .map_err(|error| Error::message(error.to_string()))?
    {
        chelix_config::AgentsConfigState::Setup => Err(Error::message(
            "default agent is not configured; configure a provider and model, then complete web onboarding",
        )),
        chelix_config::AgentsConfigState::Configured { default_agent, .. } => Ok(default_agent),
    }
}
