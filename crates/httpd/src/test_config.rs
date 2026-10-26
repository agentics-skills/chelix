use std::{
    path::Path,
    sync::{Arc, OnceLock},
};

use tokio::sync::{Mutex, OwnedMutexGuard};

pub(crate) struct ConfigDirGuard {
    directory: OwnedMutexGuard<Option<tempfile::TempDir>>,
}

impl ConfigDirGuard {
    pub(crate) async fn install(raw: &str) -> Self {
        static DIRECTORY: OnceLock<Arc<Mutex<Option<tempfile::TempDir>>>> = OnceLock::new();
        let mut directory = DIRECTORY
            .get_or_init(|| Arc::new(Mutex::new(None)))
            .clone()
            .lock_owned()
            .await;
        *directory = Some(
            tempfile::tempdir().unwrap_or_else(|error| panic!("config test directory: {error}")),
        );
        let guard = Self { directory };
        std::fs::write(guard.path().join("chelix.toml"), raw)
            .unwrap_or_else(|error| panic!("config test document: {error}"));
        chelix_config::set_config_dir(guard.path().to_path_buf());
        guard
    }

    pub(crate) fn path(&self) -> &Path {
        self.directory
            .as_ref()
            .unwrap_or_else(|| panic!("config directory is installed"))
            .path()
    }
}

impl Drop for ConfigDirGuard {
    fn drop(&mut self) {
        chelix_config::clear_config_dir();
        self.directory.take();
    }
}
