use std::path::Path;

use chelix_config::{AgentConfig, schema::ReasoningEffort};

use crate::{ConfigService, ConfigValidation};

#[tokio::test]
async fn typed_patch_preserves_comments_and_raw_substitutions()
-> Result<(), Box<dyn std::error::Error>> {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("chelix.toml");
    let raw = "# Shared host\n[env]\nHOST = '127.0.0.1'\n\n[server]\nbind = '${HOST}' # Keep this expression\nport = 12000 # Selected port\n";
    tokio::fs::write(&path, raw).await?;
    let service = ConfigService::open_at(
        path.clone(),
        ConfigValidation::default(),
        crate::ConfigInitialization::default(),
    )
    .await?;
    service
        .update(|config| {
            config.server.port = 12001;
            Ok(())
        })
        .await?;
    let document = service.read_document().await?;
    assert_eq!(document.toml, raw.replace("port = 12000", "port = 12001"));
    assert_eq!(document.toml, tokio::fs::read_to_string(path).await?);
    assert_eq!(
        service.read(|config| config.server.bind.clone()).await?,
        "127.0.0.1"
    );
    Ok(())
}

#[test]
fn inserted_agent_map_key_stays_before_memory() -> Result<(), Box<dyn std::error::Error>> {
    let raw = "\
[server]\n\
port = 18789\n\
\n\
[agents]\n\
default = \"main\"\n\
\n\
[agents.main]\n\
name = \"Main\"\n\
model = \"test::model\"\n\
reasoning_effort = \"off\"\n\
max_tools_threshold = 128\n\
compaction_reminder = true\n\
prepend_sender_badge = true\n\
\n\
[memory]\n\
";
    let mut document = crate::document::parse(raw)?;
    crate::document::update(&mut document, Path::new("chelix.toml"), |config| {
        let mut research =
            AgentConfig::new("Research", "test::model", ReasoningEffort::from("off"));
        research.skills.allow = vec!["read_file".to_owned()];
        config
            .agents
            .entries
            .insert("research".to_owned(), research);
        Ok(())
    })?;
    let text = document.to_string();
    let main = text.find("[agents.main]").ok_or("missing [agents.main]")?;
    let research = text
        .find("[agents.research]")
        .ok_or("missing [agents.research]")?;
    let skills = text
        .find("[agents.research.skills]")
        .ok_or("missing [agents.research.skills]")?;
    let memory = text.find("[memory]").ok_or("missing [memory]")?;
    assert!(
        main < research && research < skills && skills < memory,
        "{text}"
    );
    Ok(())
}
