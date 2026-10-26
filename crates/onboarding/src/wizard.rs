//! Terminal-based onboarding wizard using the shared state machine.

use std::io::{BufRead, Write};

use chelix_config_service::ConfigService;

use crate::{
    Context, Result,
    service::patch_wizard_identity,
    state::{AgentIdentityDraft, WizardState},
};

/// Run the interactive onboarding wizard in the terminal.
pub async fn run_onboarding(config: ConfigService) -> Result<()> {
    let loaded = config.read(Clone::clone).await?;
    let (_, agent) = crate::service::configured_default_agent(&loaded)?;
    let agent = AgentIdentityDraft::from_agent(agent);
    let user = chelix_config::resolve_user_profile_from_config(&loaded);

    if !agent.name.trim().is_empty() && user.name.is_some() {
        println!(
            "Already onboarded as {} with agent {}.",
            user.name.as_deref().unwrap_or(""),
            agent.name,
        );
        return Ok(());
    }

    let mut state = WizardState::new();
    state.agent = agent;
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

    let user = state.user;
    let agent = state.agent;
    let persisted = config
        .update(move |config| patch_wizard_identity(config, user, agent))
        .await?;
    chelix_config::save_user_with_mode(&persisted.user, persisted.write_mode)
        .context("failed to save user profile")?;
    println!("Config saved to {}", config.location().display());
    println!("Onboarding complete!");
    Ok(())
}
