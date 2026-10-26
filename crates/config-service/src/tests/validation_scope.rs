use std::sync::{Arc, Mutex};

use chelix_config::{ChelixConfig, Diagnostic, DiagnosticCategory, Severity};

use crate::{ConfigService, ConfigValidation, ConfigValidator, PersistedConfigStatus};

struct Mark {
    label: &'static str,
    seen: Arc<Mutex<Vec<&'static str>>>,
    reject_port: Option<u16>,
}

#[async_trait::async_trait]
impl ConfigValidator for Mark {
    async fn validate(&self, candidate: Arc<ChelixConfig>) -> Vec<Diagnostic> {
        self.seen
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .push(self.label);
        if self.reject_port == Some(candidate.server.port) {
            return vec![Diagnostic {
                severity: Severity::Error,
                category: DiagnosticCategory::InvalidValue,
                path: "server.port".into(),
                message: "profile rejected the port".into(),
                summary_message: None,
            }];
        }
        Vec::new()
    }
}

pub(super) fn take_seen(seen: &Mutex<Vec<&'static str>>) -> Vec<&'static str> {
    std::mem::take(&mut *seen.lock().unwrap_or_else(|error| error.into_inner()))
}

fn mark(
    label: &'static str,
    seen: &Arc<Mutex<Vec<&'static str>>>,
    reject_port: Option<u16>,
) -> Arc<dyn ConfigValidator> {
    Arc::new(Mark {
        label,
        seen: Arc::clone(seen),
        reject_port,
    })
}

pub(super) async fn opened(
    seen: &Arc<Mutex<Vec<&'static str>>>,
    reject_load: Option<u16>,
    reject_full: Option<u16>,
) -> Result<(tempfile::TempDir, ConfigService), Box<dyn std::error::Error>> {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("chelix.toml");
    tokio::fs::write(&path, "[server]\nport = 12000\n").await?;
    let service = ConfigService::open_at(
        path,
        ConfigValidation::new(vec![mark("full", seen, reject_full)], vec![mark(
            "load",
            seen,
            reject_load,
        )]),
        crate::ConfigInitialization::default(),
    )
    .await?;
    Ok((directory, service))
}

#[tokio::test]
async fn load_operations_select_only_the_load_profile() -> Result<(), Box<dyn std::error::Error>> {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let (_directory, service) = opened(&seen, None, None).await?;
    assert_eq!(take_seen(&seen), ["load"]);

    service
        .update(|config| {
            config.server.port = 12002;
            Ok(())
        })
        .await?;
    assert_eq!(take_seen(&seen), ["load"]);
    assert_eq!(service.read(|config| config.server.port).await?, 12002);

    service
        .edit_document(|document| {
            document["server"]["port"] = toml_edit::value(12003_i64);
            Ok(())
        })
        .await?;
    assert_eq!(take_seen(&seen), ["load"]);
    assert_eq!(service.read(|config| config.server.port).await?, 12003);

    service
        .replace_document("[server]\nport = 12004\n".into(), None)
        .await?;
    assert_eq!(take_seen(&seen), ["load"]);

    assert_eq!(
        service.validate_persisted(None).await?,
        PersistedConfigStatus::Valid
    );
    assert_eq!(take_seen(&seen), ["load"]);
    Ok(())
}

#[tokio::test]
async fn full_operations_select_only_the_full_profile() -> Result<(), Box<dyn std::error::Error>> {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let (_directory, service) = opened(&seen, None, None).await?;
    assert_eq!(take_seen(&seen), ["load"]);

    service
        .update_checked(|config| {
            config.server.port = 12002;
            Ok(())
        })
        .await?;
    assert_eq!(take_seen(&seen), ["full"]);

    service
        .replace_document_checked("[server]\nport = 12006\n".into(), None)
        .await?;
    assert_eq!(take_seen(&seen), ["full"]);
    assert_eq!(service.read(|config| config.server.port).await?, 12006);
    assert_eq!(
        tokio::fs::read_to_string(service.location()).await?,
        "[server]\nport = 12006\n"
    );

    let diagnosed = service.diagnose("[server]\nport = 12007\n").await;
    assert!(!diagnosed.has_errors());
    assert_eq!(take_seen(&seen), ["full"]);
    assert_eq!(service.read(|config| config.server.port).await?, 12006);
    assert_eq!(
        tokio::fs::read_to_string(service.location()).await?,
        "[server]\nport = 12006\n"
    );
    Ok(())
}

#[tokio::test]
async fn open_rejection_runs_only_the_load_profile() -> Result<(), Box<dyn std::error::Error>> {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("chelix.toml");
    let raw = "[server]\nport = 12000\n";
    tokio::fs::write(&path, raw).await?;
    let opened = ConfigService::open_at(
        path.clone(),
        ConfigValidation::new(vec![mark("full", &seen, Some(12000))], vec![mark(
            "load",
            &seen,
            Some(12000),
        )]),
        crate::ConfigInitialization::default(),
    )
    .await;
    assert!(opened.is_err());
    assert_eq!(take_seen(&seen), ["load"]);
    assert_eq!(tokio::fs::read_to_string(path).await?, raw);
    Ok(())
}

#[tokio::test]
async fn selected_profile_error_rejects_without_the_other_profile()
-> Result<(), Box<dyn std::error::Error>> {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let (_directory, service) = opened(&seen, Some(12001), Some(12002)).await?;
    assert_eq!(take_seen(&seen), ["load"]);
    let original = tokio::fs::read_to_string(service.location()).await?;

    assert!(
        service
            .update(|config| {
                config.server.port = 12001;
                Ok(())
            })
            .await
            .is_err()
    );
    assert_eq!(take_seen(&seen), ["load"]);
    assert_eq!(
        tokio::fs::read_to_string(service.location()).await?,
        original
    );
    assert_eq!(service.read(|config| config.server.port).await?, 12000);

    assert!(
        service
            .update_checked(|config| {
                config.server.port = 12002;
                Ok(())
            })
            .await
            .is_err()
    );
    assert_eq!(take_seen(&seen), ["full"]);
    assert_eq!(
        tokio::fs::read_to_string(service.location()).await?,
        original
    );
    assert_eq!(service.read(|config| config.server.port).await?, 12000);

    service
        .update_checked(|config| {
            config.server.port = 12001;
            Ok(())
        })
        .await?;
    assert_eq!(take_seen(&seen), ["full"]);
    assert_eq!(service.read(|config| config.server.port).await?, 12001);

    service
        .update(|config| {
            config.server.port = 12002;
            Ok(())
        })
        .await?;
    assert_eq!(take_seen(&seen), ["load"]);
    assert_eq!(service.read(|config| config.server.port).await?, 12002);

    tokio::fs::write(service.location(), "[server]\nport = 12001\n").await?;
    assert_eq!(service.read(|config| config.server.port).await?, 12002);
    assert_eq!(
        tokio::fs::read_to_string(service.location()).await?,
        "[server]\nport = 12001\n"
    );

    assert!(service.validate_persisted(None).await.is_err());
    assert_eq!(take_seen(&seen), ["load"]);
    assert_eq!(service.read(|config| config.server.port).await?, 12002);
    Ok(())
}

#[tokio::test]
async fn extra_follows_only_the_selected_profile() -> Result<(), Box<dyn std::error::Error>> {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let (_directory, service) = opened(&seen, None, None).await?;
    assert_eq!(take_seen(&seen), ["load"]);
    let extra = mark("extra", &seen, None);

    service
        .edit_document_checked(
            |document| {
                document["server"]["port"] = toml_edit::value(12003_i64);
                Ok(())
            },
            Arc::clone(&extra),
        )
        .await?;
    assert_eq!(take_seen(&seen), ["load", "extra"]);

    service
        .replace_document("[server]\nport = 12004\n".into(), Some(Arc::clone(&extra)))
        .await?;
    assert_eq!(take_seen(&seen), ["load", "extra"]);

    service
        .replace_document_checked("[server]\nport = 12005\n".into(), Some(Arc::clone(&extra)))
        .await?;
    assert_eq!(take_seen(&seen), ["full", "extra"]);
    assert_eq!(service.read(|config| config.server.port).await?, 12005);

    assert_eq!(
        service.validate_persisted(Some(Arc::clone(&extra))).await?,
        PersistedConfigStatus::Valid
    );
    assert_eq!(take_seen(&seen), ["load", "extra"]);

    assert!(
        service
            .validate_persisted(Some(mark("extra", &seen, Some(12005))))
            .await
            .is_err()
    );
    assert_eq!(take_seen(&seen), ["load", "extra"]);
    assert_eq!(service.read(|config| config.server.port).await?, 12005);
    assert_eq!(
        tokio::fs::read_to_string(service.location()).await?,
        "[server]\nport = 12005\n"
    );
    Ok(())
}
