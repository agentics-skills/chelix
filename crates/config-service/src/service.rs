use std::{
    fmt,
    path::{Path, PathBuf},
    sync::Arc,
};

use {
    arc_swap::ArcSwap,
    chelix_config::ChelixConfig,
    tokio::sync::{mpsc, oneshot},
    toml_edit::DocumentMut,
};

use crate::{
    ConfigDocument, ConfigInitialization, ConfigValidation, ConfigValidator, Error,
    PersistedConfigStatus, Result, reader,
    snapshot::Snapshot,
    storage::Storage,
    validation::ValidationScope,
    writer::{Command, Commit, Replace, ValidatePersisted, Writer},
};

const QUEUE_CAPACITY: usize = 64;

#[derive(Clone)]
pub struct ConfigService {
    location: Arc<PathBuf>,
    reads: mpsc::Sender<reader::Request>,
    pub(crate) writes: mpsc::Sender<Command>,
    validation: ConfigValidation,
}

impl fmt::Debug for ConfigService {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ConfigService")
            .field("location", &self.location)
            .finish_non_exhaustive()
    }
}

impl ConfigService {
    #[cfg_attr(feature = "tracing", tracing::instrument(skip_all))]
    pub async fn open(
        validation: ConfigValidation,
        initialization: ConfigInitialization,
    ) -> Result<Self> {
        Self::start(Storage::discover()?, validation, initialization).await
    }

    #[cfg(test)]
    pub(crate) async fn open_at(
        path: PathBuf,
        validation: ConfigValidation,
        initialization: ConfigInitialization,
    ) -> Result<Self> {
        Self::start(Storage { path }, validation, initialization).await
    }

    async fn start(
        storage: Storage,
        validation: ConfigValidation,
        initialization: ConfigInitialization,
    ) -> Result<Self> {
        let (raw, persist) = initialization.prepare(&storage).await?;
        let candidate = validation
            .admit(raw, &storage.path, ValidationScope::Load, None)
            .await?;
        if persist {
            storage.write(candidate.raw.clone()).await?;
        }
        let snapshot = Arc::new(ArcSwap::from_pointee(candidate));
        let (reads, read_requests) = mpsc::channel(QUEUE_CAPACITY);
        let (writes, write_requests) = mpsc::channel(QUEUE_CAPACITY);
        let service = Self {
            location: Arc::new(storage.path.clone()),
            reads,
            writes,
            validation: validation.clone(),
        };
        tokio::spawn(reader::run(Arc::clone(&snapshot), read_requests));
        tokio::spawn(
            Writer {
                storage,
                validation,
                snapshot,
            }
            .run(write_requests),
        );
        crate::refresh::start(service.writes.downgrade());
        Ok(service)
    }

    #[must_use]
    pub fn location(&self) -> &Path {
        self.location.as_path()
    }

    #[cfg_attr(feature = "tracing", tracing::instrument(skip_all))]
    pub async fn read<R, F>(&self, select: F) -> Result<R>
    where
        F: FnOnce(&Arc<ChelixConfig>) -> R + Send + 'static,
        R: Send + 'static,
    {
        self.select(move |snapshot| select(&snapshot.config)).await
    }

    pub async fn read_document(&self) -> Result<ConfigDocument> {
        let location = self.location.as_ref().clone();
        self.select(move |snapshot| ConfigDocument {
            toml: snapshot.raw.clone(),
            path: location,
        })
        .await
    }

    async fn select<R, F>(&self, select: F) -> Result<R>
    where
        F: FnOnce(&Snapshot) -> R + Send + 'static,
        R: Send + 'static,
    {
        let (reply, result) = oneshot::channel();
        self.reads
            .send(Box::new(move |snapshot| {
                let _ = reply.send(select(snapshot));
            }))
            .await
            .map_err(|_| closed())?;
        result.await.map_err(|_| closed())
    }

    #[cfg_attr(feature = "tracing", tracing::instrument(skip_all))]
    pub async fn update<R, F>(&self, patch: F) -> Result<R>
    where
        F: FnOnce(&mut ChelixConfig) -> chelix_config::Result<R> + Send + 'static,
        R: Send + 'static,
    {
        self.update_with_scope(patch, ValidationScope::Load).await
    }

    #[cfg_attr(feature = "tracing", tracing::instrument(skip_all))]
    pub async fn update_checked<R, F>(&self, patch: F) -> Result<R>
    where
        F: FnOnce(&mut ChelixConfig) -> chelix_config::Result<R> + Send + 'static,
        R: Send + 'static,
    {
        self.update_with_scope(patch, ValidationScope::Full).await
    }

    async fn update_with_scope<R, F>(&self, patch: F, scope: ValidationScope) -> Result<R>
    where
        F: FnOnce(&mut ChelixConfig) -> chelix_config::Result<R> + Send + 'static,
        R: Send + 'static,
    {
        let location = self.location.as_ref().clone();
        self.commit(
            move |document| crate::document::update(document, &location, patch),
            scope,
            None,
        )
        .await
    }

    pub async fn edit_document<R, F>(&self, patch: F) -> Result<R>
    where
        F: FnOnce(&mut DocumentMut) -> chelix_config::Result<R> + Send + 'static,
        R: Send + 'static,
    {
        self.commit(patch, ValidationScope::Load, None).await
    }

    pub async fn edit_document_checked<R, F>(
        &self,
        patch: F,
        check: Arc<dyn ConfigValidator>,
    ) -> Result<R>
    where
        F: FnOnce(&mut DocumentMut) -> chelix_config::Result<R> + Send + 'static,
        R: Send + 'static,
    {
        self.commit(patch, ValidationScope::Load, Some(check)).await
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
        let (reply, result) = oneshot::channel();
        self.writes
            .send(Box::new(Commit {
                patch,
                scope,
                check,
                reply,
            }))
            .await
            .map_err(|_| closed())?;
        result.await.map_err(|_| closed())?
    }

    #[cfg_attr(feature = "tracing", tracing::instrument(skip_all))]
    pub async fn replace_document(
        &self,
        raw: String,
        check: Option<Arc<dyn ConfigValidator>>,
    ) -> Result<()> {
        self.replace(raw, ValidationScope::Load, check).await
    }

    #[cfg_attr(feature = "tracing", tracing::instrument(skip_all))]
    pub async fn replace_document_checked(
        &self,
        raw: String,
        check: Option<Arc<dyn ConfigValidator>>,
    ) -> Result<()> {
        self.replace(raw, ValidationScope::Full, check).await
    }

    async fn replace(
        &self,
        raw: String,
        scope: ValidationScope,
        check: Option<Arc<dyn ConfigValidator>>,
    ) -> Result<()> {
        let (reply, result) = oneshot::channel();
        self.writes
            .send(Box::new(Replace {
                raw,
                scope,
                check,
                reply,
            }))
            .await
            .map_err(|_| closed())?;
        result.await.map_err(|_| closed())?
    }

    pub async fn diagnose(&self, raw: &str) -> chelix_config::ValidationResult {
        self.validation.diagnose(raw, self.location()).await
    }

    pub async fn validate_persisted(
        &self,
        check: Option<Arc<dyn ConfigValidator>>,
    ) -> Result<PersistedConfigStatus> {
        let (reply, result) = oneshot::channel();
        self.writes
            .send(Box::new(ValidatePersisted { check, reply }))
            .await
            .map_err(|_| closed())?;
        result.await.map_err(|_| closed())?
    }
}

pub(crate) fn closed() -> Error {
    #[cfg(feature = "tracing")]
    tracing::error!("configuration service queue closed");
    Error::QueueClosed
}
