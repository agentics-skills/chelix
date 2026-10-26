//! Typed updates of an opened `chelix.toml`.

use std::path::{Path, PathBuf};

use crate::{ConfigService, ConfigValidation};

fn config_path(dir: &Path) -> PathBuf {
    dir.join("chelix.toml")
}

async fn open_file(path: PathBuf) -> crate::Result<ConfigService> {
    ConfigService::open_at(
        path,
        ConfigValidation::default(),
        crate::ConfigInitialization::default(),
    )
    .await
}

#[tokio::test]
async fn update_persists_changed_fields() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempfile::tempdir()?;
    let path = config_path(dir.path());
    tokio::fs::write(&path, "[server]\nport = 54321\n\n[auth]\ndisabled = true\n").await?;
    let service = open_file(path.clone()).await?;

    service
        .update(|config| {
            config.server.http_request_logs = true;
            Ok(())
        })
        .await?;

    let saved = tokio::fs::read_to_string(&path).await?;
    assert!(saved.contains("http_request_logs = true"), "{saved}");
    assert!(saved.contains("disabled = true"), "{saved}");
    assert!(saved.contains("port = 54321"), "{saved}");
    assert!(
        service
            .read(|config| config.server.http_request_logs)
            .await?
    );
    Ok(())
}

#[tokio::test]
async fn update_removes_cleared_agent_field() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempfile::tempdir()?;
    let path = config_path(dir.path());
    tokio::fs::write(
        &path,
        r#"[server]
bind = "127.0.0.1"
port = 18789

[agents]
default = "main"

[agents.main]
name = "Rex"
emoji = "🐶"
model = "test::model"
reasoning_effort = "off"
max_tools_threshold = 128
compaction_reminder = true
prepend_sender_badge = true
"#,
    )
    .await?;
    let service = open_file(path.clone()).await?;

    service
        .update(|config| {
            let agent = config
                .agents
                .entries
                .get_mut("main")
                .ok_or_else(|| chelix_config::Error::message("main agent is not configured"))?;
            agent.emoji = None;
            Ok(())
        })
        .await?;

    let saved = tokio::fs::read_to_string(&path).await?;
    assert!(!saved.contains("emoji"), "{saved}");
    let emoji = service
        .read(|config| {
            config
                .agents
                .entries
                .get("main")
                .map(|agent| agent.emoji.clone())
        })
        .await?;
    assert_eq!(emoji, Some(None));
    Ok(())
}

#[tokio::test]
async fn update_does_not_restore_deleted_agent() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempfile::tempdir()?;
    let path = config_path(dir.path());
    tokio::fs::write(
        &path,
        r#"[server]
port = 44444

[agents]
default = "main"

[agents.main]
name = "Main"
model = "test::model"
reasoning_effort = "off"
max_tools_threshold = 128
compaction_reminder = true
prepend_sender_badge = true

[agents.research]
name = "Research"
model = "test::research"
reasoning_effort = "off"
max_tools_threshold = 7
compaction_reminder = true
prepend_sender_badge = true
"#,
    )
    .await?;
    let service = open_file(path.clone()).await?;

    service
        .update(|config| {
            let _ = config.agents.entries.remove("research");
            Ok(())
        })
        .await?;

    let saved = tokio::fs::read_to_string(&path).await?;
    assert!(!saved.contains("[agents.research]"), "{saved}");
    assert!(saved.contains("[agents.main]"), "{saved}");
    assert!(saved.contains("port = 44444"), "{saved}");
    let agents = service
        .read(|config| {
            (
                config.agents.entries.contains_key("main"),
                config.agents.entries.contains_key("research"),
            )
        })
        .await?;
    assert_eq!(agents, (true, false));
    Ok(())
}

#[tokio::test]
async fn agent_update_persists_empty_skill_lists() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempfile::tempdir()?;
    let path = config_path(dir.path());
    tokio::fs::write(&path, "[server]\nport = 12000\n").await?;
    let service = open_file(path.clone()).await?;

    service
        .update(|config| {
            config.agents.default = "main".into();
            config.agents.entries.insert(
                "main".into(),
                chelix_config::AgentConfig::new(
                    "Main",
                    "test::model",
                    chelix_config::schema::ReasoningEffort::from("off"),
                ),
            );
            Ok(())
        })
        .await?;

    let saved = tokio::fs::read_to_string(&path).await?;
    let document = service.read_document().await?;
    assert_eq!(document.path, path);
    assert_eq!(document.toml, saved);
    assert!(saved.contains("[agents.main.skills]"), "{saved}");
    let parsed: toml::Value = toml::from_str(&saved)?;
    let policy = &parsed["agents"]["main"]["skills"];
    let Some(allow) = policy.get("allow").and_then(toml::Value::as_array) else {
        panic!("skills.allow missing from {saved}");
    };
    let Some(deny) = policy.get("deny").and_then(toml::Value::as_array) else {
        panic!("skills.deny missing from {saved}");
    };
    assert!(allow.is_empty(), "{saved}");
    assert!(deny.is_empty(), "{saved}");

    let skills = service
        .read(|config| {
            config
                .agents
                .entries
                .get("main")
                .map(|agent| (agent.skills.allow.clone(), agent.skills.deny.clone()))
        })
        .await?;
    assert_eq!(skills, Some((Vec::new(), Vec::new())));
    Ok(())
}

#[tokio::test]
async fn update_preserves_template_comments_and_section_order()
-> Result<(), Box<dyn std::error::Error>> {
    let dir = tempfile::tempdir()?;
    let path = config_path(dir.path());
    let template = chelix_config::template::default_config_template(18789);
    tokio::fs::write(&path, &template).await?;
    let template_sections: Vec<String> = template
        .lines()
        .filter(|line| line.starts_with('['))
        .map(str::to_string)
        .collect();
    let service = open_file(path.clone()).await?;

    service
        .update(|config| {
            config.auth.disabled = true;
            config.server.http_request_logs = true;
            Ok(())
        })
        .await?;

    let document = service.read_document().await?;
    assert_eq!(document.path, path);
    assert!(
        document.toml.contains("# Chelix Configuration"),
        "{}",
        document.toml
    );
    assert!(
        document.toml.contains("disabled = true"),
        "{}",
        document.toml
    );
    assert!(
        document.toml.contains("http_request_logs = true"),
        "{}",
        document.toml
    );
    let saved_sections: Vec<String> = document
        .toml
        .lines()
        .filter(|line| line.starts_with('['))
        .map(str::to_string)
        .collect();
    for section in &template_sections {
        assert!(
            saved_sections.contains(section),
            "template section {section} missing from saved file"
        );
    }
    let positions: Vec<usize> = template_sections
        .iter()
        .map(|section| {
            saved_sections
                .iter()
                .position(|saved| saved == section)
                .unwrap_or_else(|| panic!("section {section} not found in saved file"))
        })
        .collect();
    for window in positions.windows(2) {
        assert!(
            window[0] < window[1],
            "template sections reordered: {window:?}"
        );
    }
    Ok(())
}
