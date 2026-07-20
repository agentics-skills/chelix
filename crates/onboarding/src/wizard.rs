//! Terminal-based onboarding wizard using the shared state machine.

use std::io::{BufRead, Write};

use chelix_config::{ChelixConfig, find_or_default_config_path, save_config};

use crate::{Context, Result, state::WizardState};

/// Run the interactive onboarding wizard in the terminal.
pub async fn run_onboarding() -> Result<()> {
    let config_path = find_or_default_config_path();

    // Check if already onboarded.
    let mut identity_name: Option<String> = None;
    let mut user_name: Option<String> = None;
    if config_path.exists() {
        let cfg = chelix_config::loader::load_config(&config_path)
            .context("failed to load existing onboarding config")?;
        identity_name = cfg.identity.name;
        user_name = cfg.user.name;
    }
    if let Some(id) = chelix_config::load_identity_for_agent("main")
        && id.name.is_some()
    {
        identity_name = id.name;
    }
    if let Some(user) = chelix_config::load_user()
        && user.name.is_some()
    {
        user_name = user.name;
    }

    if identity_name.is_some() && user_name.is_some() {
        println!(
            "Already onboarded as {} with agent {}.",
            user_name.as_deref().unwrap_or("?"),
            identity_name.as_deref().unwrap_or("?"),
        );
        return Ok(());
    }

    let mut state = WizardState::new();
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

    // Merge into existing config or create new one.
    let mut config = if config_path.exists() {
        chelix_config::loader::load_config(&config_path)
            .context("failed to load existing onboarding config")?
    } else {
        ChelixConfig::default()
    };
    config.identity = state.identity;
    config.user = state.user;

    let path = save_config(&config).context("failed to save onboarding config")?;
    chelix_config::save_identity_for_agent("main", &config.identity)
        .context("failed to save identity")?;
    chelix_config::save_user_with_mode(&config.user, config.memory.user_profile_write_mode)
        .context("failed to save user")?;
    println!("Config saved to {}", path.display());
    println!("Onboarding complete!");
    Ok(())
}
