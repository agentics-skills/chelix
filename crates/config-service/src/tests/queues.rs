use std::sync::Arc;

use {
    chelix_config::{ChelixConfig, Diagnostic, DiagnosticCategory, Severity},
    tokio::sync::Notify,
};

use crate::{ConfigService, ConfigValidation, ConfigValidator, PersistedConfigStatus};

struct PauseUpdate {
    entered: Arc<Notify>,
    resume: Arc<Notify>,
}

#[async_trait::async_trait]
impl ConfigValidator for PauseUpdate {
    async fn validate(&self, candidate: Arc<ChelixConfig>) -> Vec<Diagnostic> {
        if candidate.server.port == 12001 {
            self.entered.notify_one();
            self.resume.notified().await;
        }
        Vec::new()
    }
}

struct RejectPort;

#[async_trait::async_trait]
impl ConfigValidator for RejectPort {
    async fn validate(&self, candidate: Arc<ChelixConfig>) -> Vec<Diagnostic> {
        if candidate.server.port == 12001 {
            return vec![Diagnostic {
                severity: Severity::Error,
                category: DiagnosticCategory::InvalidValue,
                path: "server.port".into(),
                message: "port rejected by owner".into(),
                summary_message: None,
            }];
        }
        Vec::new()
    }
}

async fn fixture(
    validation: ConfigValidation,
) -> Result<(tempfile::TempDir, ConfigService), Box<dyn std::error::Error>> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("chelix.toml");
    tokio::fs::write(&path, "[server]\nport = 12000\n").await?;
    let service =
        ConfigService::open_at(path, validation, crate::ConfigInitialization::default()).await?;
    Ok((dir, service))
}

#[tokio::test]
async fn whole_config_reads_share_the_published_arc() -> Result<(), Box<dyn std::error::Error>> {
    let (_dir, service) = fixture(ConfigValidation::default()).await?;
    let first = service.read(Arc::clone).await?;
    let second = service.read(Arc::clone).await?;
    assert!(Arc::ptr_eq(&first, &second));
    service
        .update(|config| {
            config.server.port = 12001;
            Ok(())
        })
        .await?;
    let published = service.read(Arc::clone).await?;
    assert!(!Arc::ptr_eq(&first, &published));
    assert_eq!(first.server.port, 12000);
    assert_eq!(published.server.port, 12001);
    Ok(())
}

#[tokio::test]
async fn reads_progress_during_validation_and_observe_completed_write()
-> Result<(), Box<dyn std::error::Error>> {
    let entered = Arc::new(Notify::new());
    let resume = Arc::new(Notify::new());
    let checks: Vec<Arc<dyn ConfigValidator>> = vec![Arc::new(PauseUpdate {
        entered: entered.clone(),
        resume: resume.clone(),
    })];
    let validation = ConfigValidation::new(checks.clone(), checks);
    let (_dir, service) = fixture(validation).await?;
    let writer = service.clone();
    let writing = tokio::spawn(async move {
        writer
            .update(|config| {
                config.server.port = 12001;
                Ok(config.server.port)
            })
            .await
    });
    entered.notified().await;
    let port = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        service.read(|config| config.server.port),
    )
    .await??;
    assert_eq!(port, 12000);
    resume.notify_one();
    assert_eq!(writing.await??, 12001);
    assert_eq!(service.read(|config| config.server.port).await?, 12001);
    Ok(())
}

#[tokio::test]
async fn sequential_writers_receive_their_results_and_patch_latest_file()
-> Result<(), Box<dyn std::error::Error>> {
    let entered = Arc::new(Notify::new());
    let resume = Arc::new(Notify::new());
    let checks: Vec<Arc<dyn ConfigValidator>> = vec![Arc::new(PauseUpdate {
        entered: entered.clone(),
        resume: resume.clone(),
    })];
    let validation = ConfigValidation::new(checks.clone(), checks);
    let (_dir, service) = fixture(validation).await?;
    let writer = service.clone();
    let first = tokio::spawn(async move {
        writer
            .update(|config| {
                config.server.port = 12001;
                Ok(config.server.port)
            })
            .await
    });
    entered.notified().await;
    let writer = service.clone();
    let second = tokio::spawn(async move {
        writer
            .update(|config| {
                let previous = config.server.port;
                config.server.port = 12002;
                Ok((previous, config.server.port))
            })
            .await
    });
    resume.notify_one();
    assert_eq!(first.await??, 12001);
    assert_eq!(second.await??, (12001, 12002));
    assert_eq!(service.read(|config| config.server.port).await?, 12002);
    Ok(())
}

#[tokio::test]
async fn owner_rejection_preserves_file_and_snapshot() -> Result<(), Box<dyn std::error::Error>> {
    let checks: Vec<Arc<dyn ConfigValidator>> = vec![Arc::new(RejectPort)];
    let (_dir, service) = fixture(ConfigValidation::new(checks.clone(), checks)).await?;
    let before = tokio::fs::read_to_string(service.location()).await?;
    let result = service
        .update(|config| {
            config.server.port = 12001;
            Ok(())
        })
        .await;
    assert!(result.is_err());
    assert_eq!(tokio::fs::read_to_string(service.location()).await?, before);
    assert_eq!(service.read(|config| config.server.port).await?, 12000);
    Ok(())
}

#[tokio::test]
async fn failed_file_replacement_preserves_snapshot() -> Result<(), Box<dyn std::error::Error>> {
    let entered = Arc::new(Notify::new());
    let resume = Arc::new(Notify::new());
    let checks: Vec<Arc<dyn ConfigValidator>> = vec![Arc::new(PauseUpdate {
        entered: entered.clone(),
        resume: resume.clone(),
    })];
    let validation = ConfigValidation::new(checks.clone(), checks);
    let (_dir, service) = fixture(validation).await?;
    let writer = service.clone();
    let writing = tokio::spawn(async move {
        writer
            .update(|config| {
                config.server.port = 12001;
                Ok(())
            })
            .await
    });
    entered.notified().await;
    tokio::fs::remove_file(service.location()).await?;
    tokio::fs::create_dir(service.location()).await?;
    resume.notify_one();
    assert!(matches!(writing.await?, Err(crate::Error::Write { .. })));
    assert_eq!(service.read(|config| config.server.port).await?, 12000);
    Ok(())
}

#[tokio::test]
async fn persisted_validation_reports_missing_and_leaves_snapshot_current()
-> Result<(), Box<dyn std::error::Error>> {
    let (_dir, service) = fixture(ConfigValidation::default()).await?;
    tokio::fs::write(service.location(), "[server]\nport = 12002\n").await?;
    assert_eq!(
        service.validate_persisted(None).await?,
        PersistedConfigStatus::Valid
    );
    assert_eq!(service.read(|config| config.server.port).await?, 12000);
    tokio::fs::remove_file(service.location()).await?;
    assert_eq!(
        service.validate_persisted(None).await?,
        PersistedConfigStatus::Missing
    );
    assert!(
        service
            .update(|config| {
                config.server.port = 12003;
                Ok(())
            })
            .await
            .is_err()
    );
    Ok(())
}

#[tokio::test]
async fn external_invalid_file_rejects_update_and_keeps_prior_snapshot()
-> Result<(), Box<dyn std::error::Error>> {
    let (_dir, service) = fixture(ConfigValidation::default()).await?;
    let invalid = "[server]\nremoved_option = true\n";
    tokio::fs::write(service.location(), invalid).await?;
    assert!(
        service
            .update(|config| {
                config.server.port = 12001;
                Ok(())
            })
            .await
            .is_err()
    );
    assert_eq!(
        tokio::fs::read_to_string(service.location()).await?,
        invalid
    );
    assert_eq!(service.read(|config| config.server.port).await?, 12000);
    Ok(())
}
