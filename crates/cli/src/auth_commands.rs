use {anyhow::Result, clap::Subcommand};

#[derive(Subcommand)]
pub enum AuthAction {
    /// Reset gateway authentication (remove password, sessions, passkeys, API keys).
    ResetPassword,
    /// Reset the user profile (triggers onboarding on next start).
    ResetProfile,
    /// Create a new API key for authenticating with the gateway.
    CreateApiKey {
        /// Label for the API key (e.g. "CLI tool", "CI pipeline").
        #[arg(long)]
        label: String,
        /// Comma-separated list of scopes. If omitted, the key has full access.
        /// Valid scopes: operator.read, operator.write, operator.approvals, operator.pairing
        #[arg(long)]
        scopes: Option<String>,
    },
}

pub async fn handle_auth(action: AuthAction) -> Result<()> {
    match action {
        AuthAction::ResetPassword => reset_password().await,
        AuthAction::ResetProfile => reset_profile(),
        AuthAction::CreateApiKey { label, scopes } => create_api_key(&label, scopes).await,
    }
}

fn reset_profile() -> Result<()> {
    chelix_config::loader::update_config(|cfg| {
        cfg.user = Default::default();
    })?;
    println!("User profile cleared. Onboarding will be required on next load.");
    Ok(())
}

async fn reset_password() -> Result<()> {
    let data_dir = chelix_config::data_dir();
    let db_path = data_dir.join("chelix.db");
    if !db_path.exists() {
        println!("No database found at {}", db_path.display());
        return Ok(());
    }

    chelix_gateway::auth::CredentialStore::reset_from_db_path(&db_path).await?;
    for line in reset_password_success_lines() {
        println!("{line}");
    }
    Ok(())
}

fn reset_password_success_lines() -> [&'static str; 2] {
    [
        "Authentication reset. Password, sessions, passkeys, and API keys removed.",
        "Authentication is now disabled. Open Settings > Security to set a password or passkey to re-enable it.",
    ]
}

async fn create_api_key(label: &str, scopes_str: Option<String>) -> Result<()> {
    let data_dir = chelix_config::data_dir();
    let db_path = data_dir.join("chelix.db");
    if !db_path.exists() {
        anyhow::bail!(
            "No database found at {}. Start the gateway first to initialize it.",
            db_path.display()
        );
    }

    // Parse and validate scopes
    let scopes: Option<Vec<String>> = if let Some(ref s) = scopes_str {
        let parsed: Vec<String> = s.split(',').map(|s| s.trim().to_string()).collect();
        for scope in &parsed {
            if !chelix_gateway::auth::VALID_SCOPES.contains(&scope.as_str()) {
                anyhow::bail!(
                    "Invalid scope: {scope}\nValid scopes: {}",
                    chelix_gateway::auth::VALID_SCOPES.join(", ")
                );
            }
        }
        Some(parsed)
    } else {
        None
    };

    // Connect to database and create the key
    let db_url = format!("sqlite:{}", db_path.display());
    let pool = sqlx::SqlitePool::connect(&db_url).await?;
    let config = chelix_config::discover_and_load()?;
    let store = chelix_gateway::auth::CredentialStore::with_config(
        pool,
        &config.auth,
        chelix_gateway::auth::AuthConfigPersistence::Filesystem,
    )
    .await?;

    let (id, raw_key) = store.create_api_key(label, scopes.as_deref()).await?;

    println!("API key created successfully!");
    println!();
    println!("  ID:     {id}");
    println!("  Label:  {label}");
    if let Some(ref s) = scopes {
        println!("  Scopes: {}", s.join(", "));
    } else {
        println!("  Scopes: Full access (all scopes)");
    }
    println!();
    println!("Key (save this now, it won't be shown again):");
    println!();
    println!("  {raw_key}");
    println!();

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::reset_password_success_lines;

    #[test]
    fn reset_password_message_describes_disabled_auth_state() {
        let lines = reset_password_success_lines();
        assert_eq!(
            lines[0],
            "Authentication reset. Password, sessions, passkeys, and API keys removed."
        );
        assert_eq!(
            lines[1],
            "Authentication is now disabled. Open Settings > Security to set a password or passkey to re-enable it."
        );
    }
}
