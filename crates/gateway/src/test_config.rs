#![allow(clippy::expect_used)]

use std::{
    path::Path,
    sync::{Arc, OnceLock},
};

pub(crate) struct ConfigFixture {
    pub(crate) directories: ConfigDirGuard,
    pub(crate) service: chelix_config_service::ConfigService,
}

impl ConfigFixture {
    pub(crate) async fn new() -> Self {
        Self::open(ConfigDirGuard::new().await).await
    }

    pub(crate) async fn with_config(raw: &str) -> Self {
        let directories = ConfigDirGuard::new().await;
        std::fs::write(directories.config_dir().join("chelix.toml"), raw).expect("config document");
        Self::open(directories).await
    }

    pub(crate) async fn open(directories: ConfigDirGuard) -> Self {
        let service = directories.open_service().await;
        Self {
            directories,
            service,
        }
    }
}

use tokio::sync::{Mutex, OwnedMutexGuard};

struct Directories {
    config: tempfile::TempDir,
    data: tempfile::TempDir,
}

pub(crate) struct ConfigDirGuard {
    directories: OwnedMutexGuard<Option<Directories>>,
}

impl ConfigDirGuard {
    pub(crate) async fn new() -> Self {
        Self::install(
            tempfile::tempdir().expect("config directory"),
            tempfile::tempdir().expect("data directory"),
        )
        .await
    }

    pub(crate) async fn install(config: tempfile::TempDir, data: tempfile::TempDir) -> Self {
        static DIRECTORIES: OnceLock<Arc<Mutex<Option<Directories>>>> = OnceLock::new();
        let mut directories = DIRECTORIES
            .get_or_init(|| Arc::new(Mutex::new(None)))
            .clone()
            .lock_owned()
            .await;
        chelix_config::set_config_dir(config.path().to_path_buf());
        chelix_config::set_data_dir(data.path().to_path_buf());
        *directories = Some(Directories { config, data });
        Self { directories }
    }

    pub(crate) fn config_dir(&self) -> &Path {
        self.directories
            .as_ref()
            .expect("installed directories")
            .config
            .path()
    }

    pub(crate) fn data_dir(&self) -> &Path {
        self.directories
            .as_ref()
            .expect("installed directories")
            .data
            .path()
    }

    pub(crate) async fn open_service(&self) -> chelix_config_service::ConfigService {
        crate::config_document::open_test_service().await
    }
}

impl Drop for ConfigDirGuard {
    fn drop(&mut self) {
        chelix_config::clear_config_dir();
        chelix_config::clear_data_dir();
        self.directories.take();
    }
}
