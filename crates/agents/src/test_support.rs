//! Shared configuration fixture for runtime consumers.

use std::sync::{Arc, OnceLock};

use {
    chelix_config::ChelixConfig,
    chelix_config_service::ConfigService,
    tempfile::TempDir,
    tokio::sync::{Mutex, OwnedMutexGuard},
};

/// Serialized configuration service with a temporary configuration directory.
pub struct ConfigFixture {
    pub service: ConfigService,
    _directory: ConfigDirectory,
}

struct ConfigDirectory {
    directory: OwnedMutexGuard<Option<TempDir>>,
}

impl Drop for ConfigDirectory {
    fn drop(&mut self) {
        chelix_config::clear_config_dir();
        self.directory.take();
    }
}

impl ConfigFixture {
    pub async fn new(config: ChelixConfig) -> anyhow::Result<Self> {
        static DIRECTORY: OnceLock<Arc<Mutex<Option<TempDir>>>> = OnceLock::new();
        let mut directory = ConfigDirectory {
            directory: DIRECTORY
                .get_or_init(|| Arc::new(Mutex::new(None)))
                .clone()
                .lock_owned()
                .await,
        };
        let path = directory
            .directory
            .insert(tempfile::tempdir()?)
            .path()
            .to_path_buf();
        chelix_config::set_config_dir(path);
        let service = ConfigService::open(Default::default(), Default::default()).await?;
        service
            .update(move |current| {
                *current = config;
                Ok(())
            })
            .await?;
        Ok(Self {
            service,
            _directory: directory,
        })
    }
}
