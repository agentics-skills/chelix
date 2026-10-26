use std::{sync::Arc, time::Instant};

use {
    arc_swap::ArcSwap,
    tokio::sync::{mpsc, oneshot},
    toml_edit::DocumentMut,
};

use crate::{
    ConfigValidation, ConfigValidator, Error, Result, snapshot::Snapshot, storage::Storage,
    validation::ValidationScope,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PersistedConfigStatus {
    Valid,
    Missing,
}

pub(crate) type Command = Box<dyn WriteOperation>;

#[async_trait::async_trait]
pub(crate) trait WriteOperation: Send {
    async fn execute(self: Box<Self>, writer: &Writer);
}

pub(crate) struct Writer {
    pub(crate) storage: Storage,
    pub(crate) validation: ConfigValidation,
    pub(crate) snapshot: Arc<ArcSwap<Snapshot>>,
}

impl Writer {
    pub(crate) async fn run(self, mut requests: mpsc::Receiver<Command>) {
        while let Some(request) = requests.recv().await {
            request.execute(&self).await;
        }
    }

    async fn commit<R, F>(
        &self,
        patch: F,
        scope: ValidationScope,
        check: Option<Arc<dyn ConfigValidator>>,
    ) -> Result<R>
    where
        F: FnOnce(&mut DocumentMut) -> chelix_config::Result<R> + Send + 'static,
        R: Send + 'static,
    {
        let raw = self.storage.read().await?;
        let (raw, output) = tokio::task::spawn_blocking(move || {
            let mut document = crate::document::parse(&raw)?;
            let output = patch(&mut document)?;
            Ok::<_, chelix_config::Error>((document.to_string(), output))
        })
        .await
        .map_err(|error| Error::Worker(error.to_string()))??;
        self.publish(raw, scope, check).await?;
        Ok(output)
    }

    async fn publish(
        &self,
        raw: String,
        scope: ValidationScope,
        check: Option<Arc<dyn ConfigValidator>>,
    ) -> Result<()> {
        let candidate = self
            .validation
            .admit(raw, &self.storage.path, scope, check.as_deref())
            .await?;
        self.storage.write(candidate.raw.clone()).await?;
        self.snapshot.store(Arc::new(candidate));
        Ok(())
    }

    async fn replace(
        &self,
        raw: String,
        scope: ValidationScope,
        check: Option<Arc<dyn ConfigValidator>>,
    ) -> Result<()> {
        self.storage.read().await?;
        self.publish(raw, scope, check).await
    }

    async fn validate_persisted(
        &self,
        check: Option<Arc<dyn ConfigValidator>>,
    ) -> Result<PersistedConfigStatus> {
        let raw = match self.storage.read().await {
            Ok(raw) => raw,
            Err(Error::Read { source, .. }) if source.kind() == std::io::ErrorKind::NotFound => {
                return Ok(PersistedConfigStatus::Missing);
            },
            Err(error) => return Err(error),
        };
        self.validation
            .admit(
                raw,
                &self.storage.path,
                ValidationScope::Load,
                check.as_deref(),
            )
            .await?;
        Ok(PersistedConfigStatus::Valid)
    }
}

pub(crate) struct Commit<R, F> {
    pub(crate) patch: F,
    pub(crate) scope: ValidationScope,
    pub(crate) check: Option<Arc<dyn ConfigValidator>>,
    pub(crate) reply: oneshot::Sender<Result<R>>,
}

#[async_trait::async_trait]
impl<R, F> WriteOperation for Commit<R, F>
where
    F: FnOnce(&mut DocumentMut) -> chelix_config::Result<R> + Send + 'static,
    R: Send + 'static,
{
    async fn execute(self: Box<Self>, writer: &Writer) {
        let Self {
            patch,
            scope,
            check,
            reply,
        } = *self;
        let started = Instant::now();
        let result = writer.commit(patch, scope, check).await;
        record("commit", &result, started);
        let _ = reply.send(result);
    }
}

pub(crate) struct Replace {
    pub(crate) raw: String,
    pub(crate) scope: ValidationScope,
    pub(crate) check: Option<Arc<dyn ConfigValidator>>,
    pub(crate) reply: oneshot::Sender<Result<()>>,
}

#[async_trait::async_trait]
impl WriteOperation for Replace {
    async fn execute(self: Box<Self>, writer: &Writer) {
        let started = Instant::now();
        let result = writer.replace(self.raw, self.scope, self.check).await;
        record("commit", &result, started);
        let _ = self.reply.send(result);
    }
}

pub(crate) struct ValidatePersisted {
    pub(crate) check: Option<Arc<dyn ConfigValidator>>,
    pub(crate) reply: oneshot::Sender<Result<PersistedConfigStatus>>,
}

#[async_trait::async_trait]
impl WriteOperation for ValidatePersisted {
    async fn execute(self: Box<Self>, writer: &Writer) {
        let started = Instant::now();
        let result = writer.validate_persisted(self.check).await;
        record("validate_persisted", &result, started);
        let _ = self.reply.send(result);
    }
}

pub(crate) fn record<R>(operation: &'static str, result: &Result<R>, started: Instant) {
    #[cfg(feature = "tracing")]
    if let Err(error) = result {
        match error {
            Error::Config(error) => {
                let reason = crate::error::config_log_reason(error);
                tracing::error!(operation, %reason, "configuration operation rejected")
            },
            Error::Validation(_) => {
                tracing::error!(operation, "configuration operation rejected")
            },
            error => tracing::error!(operation, %error, "configuration operation failed"),
        }
    }
    #[cfg(feature = "metrics")]
    {
        let outcome = if result.is_ok() {
            "accepted"
        } else {
            "rejected"
        };
        chelix_metrics::counter!("config_operations_total", "operation" => operation, "outcome" => outcome).increment(1);
        chelix_metrics::histogram!("config_operation_duration_seconds", "operation" => operation)
            .record(started.elapsed().as_secs_f64());
    }
    let _ = (operation, result, started);
}
