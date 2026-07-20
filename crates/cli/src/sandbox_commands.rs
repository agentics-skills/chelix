use {anyhow::Result, clap::Subcommand};

use chelix_tools::sandbox;

fn sanitize_instance_slug(name: &str) -> String {
    let base = name.to_lowercase();
    let mut out = String::new();
    let mut last_dash = false;
    for ch in base.chars() {
        let mapped = if ch.is_ascii_alphanumeric() {
            ch
        } else {
            '-'
        };
        if mapped == '-' {
            if !last_dash {
                out.push(mapped);
            }
            last_dash = true;
        } else {
            out.push(mapped);
            last_dash = false;
        }
    }
    let out = out.trim_matches('-').to_string();
    if out.is_empty() {
        "chelix".to_string()
    } else {
        out
    }
}

fn instance_sandbox_prefix(config: &chelix_config::ChelixConfig) -> String {
    let mut identity_name = config.identity.name.clone();
    if let Some(file_identity) = chelix_config::load_identity_for_agent("main")
        && file_identity.name.is_some()
    {
        identity_name = file_identity.name;
    }
    let slug = sanitize_instance_slug(identity_name.as_deref().unwrap_or("chelix"));
    format!("chelix-{slug}-sandbox")
}

#[derive(Subcommand)]
pub enum SandboxAction {
    /// List pre-built sandbox images.
    List,
    /// Build a sandbox image from the configured base + packages.
    Build,
    /// Remove a specific sandbox image by tag.
    Remove {
        /// Image tag (e.g. chelix-main-sandbox:abc123).
        tag: String,
    },
    /// Remove all pre-built sandbox images.
    Clean,
}

pub async fn handle_sandbox(action: SandboxAction) -> Result<()> {
    match action {
        SandboxAction::List => list().await,
        SandboxAction::Build => build().await,
        SandboxAction::Remove { tag } => remove(&tag).await,
        SandboxAction::Clean => clean().await,
    }
}

async fn list() -> Result<()> {
    let images = sandbox::list_sandbox_images().await?;
    if images.is_empty() {
        println!("No sandbox images found.");
        return Ok(());
    }
    println!("{:<45} {:>10}  CREATED", "TAG", "SIZE");
    for img in &images {
        println!("{:<45} {:>10}  {}", img.tag, img.size, img.created);
    }
    Ok(())
}

async fn build() -> Result<()> {
    let config = chelix_config::discover_and_load();
    let mut sandbox_config = sandbox::SandboxConfig::from(&config.sandbox);
    sandbox_config.container_prefix = Some(instance_sandbox_prefix(&config));

    let packages = sandbox_config.packages.clone();
    let base = sandbox_config
        .image
        .clone()
        .unwrap_or_else(|| sandbox::DEFAULT_SANDBOX_IMAGE.to_string());
    let repo = sandbox_config
        .container_prefix
        .clone()
        .unwrap_or_else(|| "chelix-sandbox".to_string());
    let tag = sandbox::current_sandbox_image_tag(&repo, &base, &packages)?;
    println!("Base:     {base}");
    println!("Packages: {}", packages.join(", "));
    println!("Tag:      {tag}");
    println!();

    // Force mode on so create_sandbox returns the configured backend.
    let sandbox_config = sandbox::SandboxConfig {
        mode: sandbox::SandboxMode::On,
        ..sandbox_config
    };
    let backend = sandbox::create_sandbox(sandbox_config)?;
    match backend.build_image(&base, &packages).await? {
        Some(result) => {
            if result.built {
                println!("Image built successfully: {}", result.tag);
            } else {
                println!("Image already exists: {}", result.tag);
            }
        },
        None => {
            println!(
                "Backend '{}' does not support image building.",
                backend.backend_id()
            );
        },
    }
    Ok(())
}

async fn remove(tag: &str) -> Result<()> {
    sandbox::remove_sandbox_image(tag).await?;
    println!("Removed: {tag}");
    Ok(())
}

async fn clean() -> Result<()> {
    let count = sandbox::clean_sandbox_images().await?;
    if count == 0 {
        println!("No sandbox images to remove.");
    } else {
        println!(
            "Removed {count} sandbox image{}.",
            if count == 1 {
                ""
            } else {
                "s"
            }
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::sanitize_instance_slug;

    #[test]
    fn slug_lowercases_and_replaces_non_alnum() {
        assert_eq!(sanitize_instance_slug("My Server"), "my-server");
    }

    #[test]
    fn slug_collapses_consecutive_dashes() {
        assert_eq!(sanitize_instance_slug("a--b___c"), "a-b-c");
    }

    #[test]
    fn slug_trims_leading_trailing_dashes() {
        assert_eq!(sanitize_instance_slug("--hello--"), "hello");
    }

    #[test]
    fn slug_empty_falls_back_to_chelix() {
        assert_eq!(sanitize_instance_slug(""), "chelix");
        assert_eq!(sanitize_instance_slug("---"), "chelix");
    }

    #[test]
    fn slug_preserves_alphanumeric() {
        assert_eq!(sanitize_instance_slug("abc123"), "abc123");
    }
}
