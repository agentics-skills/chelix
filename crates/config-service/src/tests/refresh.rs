use std::sync::Arc;

use {
    chelix_config::{ChelixConfig, Diagnostic},
    tokio::sync::Notify,
};

use crate::{ConfigService, ConfigValidation, ConfigValidator};

#[tokio::test]
async fn refresh_accepts_valid_file_and_retains_snapshot_on_rejection()
-> Result<(), Box<dyn std::error::Error>> {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("chelix.toml");
    tokio::fs::write(&path, "[server]\nport = 12000\n").await?;
    let service = ConfigService::open_at(
        path.clone(),
        ConfigValidation::default(),
        crate::ConfigInitialization::default(),
    )
    .await?;
    let changed = "[server]\nport = 12001\n";
    tokio::fs::write(&path, changed).await?;
    assert_eq!(service.read(|config| config.server.port).await?, 12000);
    service.refresh().await?;
    assert_eq!(service.read(|config| config.server.port).await?, 12001);
    assert_eq!(tokio::fs::read_to_string(&path).await?, changed);
    tokio::fs::write(&path, "invalid TOML").await?;
    assert!(service.refresh().await.is_err());
    assert_eq!(service.read(|config| config.server.port).await?, 12001);
    assert_eq!(service.read_document().await?.toml, changed);
    Ok(())
}

#[tokio::test]
async fn refresh_missing_file_keeps_opened_snapshot() -> Result<(), Box<dyn std::error::Error>> {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("chelix.toml");
    tokio::fs::write(&path, "[server]\nport = 12000\n").await?;
    let service = ConfigService::open_at(
        path.clone(),
        ConfigValidation::default(),
        crate::ConfigInitialization::default(),
    )
    .await?;

    tokio::fs::remove_file(&path).await?;
    assert!(service.refresh().await.is_err());
    assert_eq!(service.read(|config| config.server.port).await?, 12000);
    assert!(!tokio::fs::try_exists(&path).await?);
    Ok(())
}

#[tokio::test]
async fn refresh_selects_load_profile() -> Result<(), Box<dyn std::error::Error>> {
    use std::sync::Mutex;

    use super::validation_scope::{opened, take_seen};

    let seen = Arc::new(Mutex::new(Vec::new()));
    let (_directory, service) = opened(&seen, Some(12001), Some(12002)).await?;
    assert_eq!(take_seen(&seen), ["load"]);

    tokio::fs::write(service.location(), "[server]\nport = 12002\n").await?;
    service.refresh().await?;
    assert_eq!(take_seen(&seen), ["load"]);
    assert_eq!(service.read(|config| config.server.port).await?, 12002);

    tokio::fs::write(service.location(), "[server]\nport = 12001\n").await?;
    assert!(service.refresh().await.is_err());
    assert_eq!(take_seen(&seen), ["load"]);
    assert_eq!(service.read(|config| config.server.port).await?, 12002);
    assert_eq!(
        tokio::fs::read_to_string(service.location()).await?,
        "[server]\nport = 12001\n"
    );
    Ok(())
}

struct ObserveRefresh(Arc<Notify>);

#[async_trait::async_trait]
impl ConfigValidator for ObserveRefresh {
    async fn validate(&self, config: Arc<ChelixConfig>) -> Vec<Diagnostic> {
        if config.server.port == 12001 {
            self.0.notify_one();
        }
        Vec::new()
    }
}

#[tokio::test(start_paused = true)]
async fn scheduler_enqueues_file_admission() -> Result<(), Box<dyn std::error::Error>> {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("chelix.toml");
    tokio::fs::write(&path, "[server]\nport = 12000\n").await?;
    let observed = Arc::new(Notify::new());
    let checks: Vec<Arc<dyn ConfigValidator>> = vec![Arc::new(ObserveRefresh(observed.clone()))];
    let validation = ConfigValidation::new(checks.clone(), checks);
    let service = ConfigService::open_at(
        path.clone(),
        validation,
        crate::ConfigInitialization::default(),
    )
    .await?;
    tokio::fs::write(&path, "[server]\nport = 12001\n").await?;
    tokio::time::advance(std::time::Duration::from_secs(60)).await;
    observed.notified().await;
    assert_eq!(service.read(|config| config.server.port).await?, 12001);
    Ok(())
}
