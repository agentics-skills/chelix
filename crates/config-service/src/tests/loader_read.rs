//! File reads and startup creation for `ConfigService`.

use std::path::{Path, PathBuf};

use crate::{ConfigInitialization, ConfigOrigin, ConfigService, ConfigValidation};

fn initialize_fixture(
    config: &mut chelix_config::ChelixConfig,
    origin: ConfigOrigin,
) -> chelix_config::Result<()> {
    match origin {
        ConfigOrigin::Created => config.server.port = 12001,
        ConfigOrigin::Existing if config.server.port == 0 => config.server.port = 12002,
        ConfigOrigin::Existing => {},
    }
    Ok(())
}

fn config_path(dir: &Path) -> PathBuf {
    dir.join("chelix.toml")
}

async fn open_file(path: PathBuf) -> crate::Result<ConfigService> {
    ConfigService::open_at(
        path,
        ConfigValidation::default(),
        ConfigInitialization::new(vec![initialize_fixture]),
    )
    .await
}

#[tokio::test]
async fn open_at_creates_default_only_when_missing() -> Result<(), Box<dyn std::error::Error>> {
    let missing_dir = tempfile::tempdir()?;
    let missing = config_path(missing_dir.path());
    let created = open_file(missing.clone()).await?;
    assert_eq!(created.location(), missing.as_path());
    assert!(tokio::fs::try_exists(&missing).await?);
    let mut expected = chelix_config::ChelixConfig::default();
    initialize_fixture(&mut expected, ConfigOrigin::Created)?;
    assert_eq!(
        tokio::fs::read_to_string(&missing).await?,
        toml::to_string_pretty(&expected)?,
    );
    assert_eq!(created.read(|config| config.server.port).await?, 12001);
    let reloaded = open_file(missing).await?;
    assert_eq!(reloaded.read(|config| config.server.port).await?, 12001);

    let existing_dir = tempfile::tempdir()?;
    let existing = config_path(existing_dir.path());
    let raw = "[server]\nport = 12000\n";
    tokio::fs::write(&existing, raw).await?;
    let opened = open_file(existing.clone()).await?;
    assert_eq!(tokio::fs::read_to_string(&existing).await?, raw);
    assert_eq!(opened.read(|config| config.server.port).await?, 12000);
    Ok(())
}

#[tokio::test]
async fn existing_initializer_patch_preserves_document() -> Result<(), Box<dyn std::error::Error>> {
    let directory = tempfile::tempdir()?;
    let path = config_path(directory.path());
    let raw = "# Configuration\n[server]\nport = 0 # Selected port\n\n[voice.tts.coqui]\nenabled = true\nendpoint = \"http://localhost:5002\"\n";
    tokio::fs::write(&path, raw).await?;

    let service = open_file(path.clone()).await?;
    let port = service.read(|config| config.server.port).await?;
    assert_eq!(port, 12002);
    let expected = raw.replace("port = 0", &format!("port = {port}"));
    assert_eq!(tokio::fs::read_to_string(&path).await?, expected);
    drop(service);

    let reopened = open_file(path.clone()).await?;
    assert_eq!(reopened.read(|config| config.server.port).await?, port);
    assert_eq!(tokio::fs::read_to_string(path).await?, expected);
    Ok(())
}

#[tokio::test]
async fn existing_initializer_patch_adds_missing_table() -> Result<(), Box<dyn std::error::Error>> {
    let directory = tempfile::tempdir()?;
    let path = config_path(directory.path());
    let raw = "[tools]\nagent_timeout_secs = 999\n";
    tokio::fs::write(&path, raw).await?;

    let service = open_file(path.clone()).await?;
    let port = service.read(|config| config.server.port).await?;
    assert_eq!(port, 12002);
    let saved = tokio::fs::read_to_string(path).await?;
    let document = crate::document::parse(&saved)?;
    let original = crate::document::parse(raw)?;
    assert!(document["server"].is_table());
    assert_eq!(
        document["server"]["port"].as_integer(),
        Some(i64::from(port))
    );
    assert_eq!(document["tools"].to_string(), original["tools"].to_string());
    Ok(())
}

#[tokio::test]
async fn open_at_rejects_malformed_document_without_overwrite()
-> Result<(), Box<dyn std::error::Error>> {
    let dir = tempfile::tempdir()?;
    let path = config_path(dir.path());
    let invalid = "[server\nport = 18789\n";
    tokio::fs::write(&path, invalid).await?;

    assert!(open_file(path.clone()).await.is_err());
    assert_eq!(tokio::fs::read_to_string(&path).await?, invalid);
    Ok(())
}

#[tokio::test]
async fn open_at_rejects_unknown_field_without_overwrite() -> Result<(), Box<dyn std::error::Error>>
{
    let dir = tempfile::tempdir()?;
    let path = config_path(dir.path());
    let invalid = "[server]\nremoved_option = true\n";
    tokio::fs::write(&path, invalid).await?;

    let error = match open_file(path.clone()).await {
        Ok(_) => panic!("unknown field must fail"),
        Err(error) => error,
    };
    assert!(error.to_string().contains("removed_option"), "{error}");
    assert_eq!(tokio::fs::read_to_string(&path).await?, invalid);
    Ok(())
}

#[tokio::test]
async fn initializer_failure_leaves_created_and_existing_files_untouched()
-> Result<(), Box<dyn std::error::Error>> {
    fn reject(_: &mut chelix_config::ChelixConfig, _: ConfigOrigin) -> chelix_config::Result<()> {
        Err(chelix_config::Error::message(
            "initializer rejected configuration",
        ))
    }

    let dir = tempfile::tempdir()?;
    let path = config_path(dir.path());
    for original in [None, Some("[server]\nport = 12000\n")] {
        if let Some(raw) = original {
            tokio::fs::write(&path, raw).await?;
        }
        let result = ConfigService::open_at(
            path.clone(),
            ConfigValidation::default(),
            ConfigInitialization::new(vec![reject]),
        )
        .await;
        assert!(matches!(result, Err(crate::Error::Config(_))));
        match original {
            Some(raw) => assert_eq!(tokio::fs::read_to_string(&path).await?, raw),
            None => assert!(!tokio::fs::try_exists(&path).await?),
        }
    }
    Ok(())
}

#[tokio::test]
async fn initialized_candidate_is_validated_before_persistence()
-> Result<(), Box<dyn std::error::Error>> {
    fn reject(config: &chelix_config::ChelixConfig) -> Vec<chelix_config::Diagnostic> {
        assert!(matches!(config.server.port, 12001 | 12002));
        vec![chelix_config::Diagnostic {
            severity: chelix_config::Severity::Error,
            category: chelix_config::DiagnosticCategory::InvalidValue,
            path: "server.port".into(),
            message: "initialized candidate rejected".into(),
            summary_message: None,
        }]
    }

    let dir = tempfile::tempdir()?;
    let path = config_path(dir.path());
    for original in [None, Some("[server]\nport = 0\n")] {
        if let Some(raw) = original {
            tokio::fs::write(&path, raw).await?;
        }
        let result = ConfigService::open_at(
            path.clone(),
            ConfigValidation::new(vec![], vec![std::sync::Arc::new(crate::SyncValidator(
                reject,
            ))]),
            ConfigInitialization::new(vec![initialize_fixture]),
        )
        .await;
        assert!(matches!(result, Err(crate::Error::Validation(_))));
        match original {
            Some(raw) => assert_eq!(tokio::fs::read_to_string(&path).await?, raw),
            None => assert!(!tokio::fs::try_exists(&path).await?),
        }
    }
    Ok(())
}

#[tokio::test]
async fn read_returns_explicit_port_and_agent_timeout() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempfile::tempdir()?;
    let path = config_path(dir.path());
    tokio::fs::write(
        &path,
        "[server]\nport = 11111\n\n[tools]\nagent_timeout_secs = 999\n",
    )
    .await?;

    let service = open_file(path.clone()).await?;
    assert_eq!(service.location(), path.as_path());
    let read = service
        .read(|config| (config.server.port, config.tools.agent_timeout_secs))
        .await?;
    assert_eq!(read, (11111, 999));
    Ok(())
}
