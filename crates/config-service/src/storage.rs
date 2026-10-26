use std::{
    io::Write,
    path::{Path, PathBuf},
};

use crate::{Error, Result};

#[derive(Clone)]
pub(crate) struct Storage {
    pub(crate) path: PathBuf,
}

impl Storage {
    pub(crate) fn discover() -> Result<Self> {
        Ok(Self {
            path: chelix_config::config_dir()?.join("chelix.toml"),
        })
    }

    pub(crate) async fn read(&self) -> Result<String> {
        tokio::fs::read_to_string(&self.path)
            .await
            .map_err(|source| Error::Read {
                path: self.path.clone(),
                source,
            })
    }

    pub(crate) async fn write(&self, raw: String) -> Result<()> {
        let path = self.path.clone();
        tokio::task::spawn_blocking(move || {
            atomic_write(&path, &raw).map_err(|source| Error::Write { path, source })
        })
        .await
        .map_err(|error| Error::Worker(error.to_string()))?
    }
}

fn atomic_write(path: &Path, raw: &str) -> std::io::Result<()> {
    let parent = path.parent().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "configuration path has no parent",
        )
    })?;
    std::fs::create_dir_all(parent)?;
    let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        temporary
            .as_file()
            .set_permissions(std::fs::Permissions::from_mode(0o600))?;
    }
    temporary.write_all(raw.as_bytes())?;
    temporary.persist(path).map_err(|error| error.error)?;
    Ok(())
}
