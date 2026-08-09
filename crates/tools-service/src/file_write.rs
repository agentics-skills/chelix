//! Shared in-place persistence and same-target serialization for write tools.

use {
    anyhow::{Context, Result, anyhow, bail},
    std::{
        collections::{HashMap, HashSet},
        fs::OpenOptions,
        io::{Seek as _, SeekFrom, Write as _},
        path::{Path, PathBuf},
        sync::{Arc, Weak},
    },
    tokio::sync::{Mutex, Semaphore},
};

#[derive(Default)]
pub(crate) struct FileWriteRuntime {
    path_permits: Mutex<HashMap<PathBuf, Weak<Semaphore>>>,
}

impl FileWriteRuntime {
    async fn path_permit(&self, path: &Path) -> Arc<Semaphore> {
        let mut path_permits = self.path_permits.lock().await;
        path_permits.retain(|_, permit| permit.strong_count() > 0);
        if let Some(permit) = path_permits.get(path).and_then(Weak::upgrade) {
            return permit;
        }

        let permit = Arc::new(Semaphore::new(1));
        path_permits.insert(path.to_path_buf(), Arc::downgrade(&permit));
        permit
    }

    pub(crate) async fn run_existing<T, F>(
        &self,
        requested_path: PathBuf,
        operation: F,
    ) -> Result<T>
    where
        T: Send + 'static,
        F: FnOnce(&Path) -> Result<T> + Send + 'static,
    {
        self.run_resolved(requested_path, resolve_existing_target, operation)
            .await
    }

    pub(crate) async fn run_create_or_existing<T, F>(
        &self,
        requested_path: PathBuf,
        operation: F,
    ) -> Result<T>
    where
        T: Send + 'static,
        F: FnOnce(&Path) -> Result<T> + Send + 'static,
    {
        self.run_resolved(requested_path, resolve_overwrite_target, operation)
            .await
    }

    async fn run_resolved<T, F>(
        &self,
        requested_path: PathBuf,
        resolve: fn(&Path) -> Result<PathBuf>,
        operation: F,
    ) -> Result<T>
    where
        T: Send + 'static,
        F: FnOnce(&Path) -> Result<T> + Send + 'static,
    {
        let path_to_resolve = requested_path.clone();
        let resolved_path = tokio::task::spawn_blocking(move || resolve(&path_to_resolve))
            .await
            .context("blocking write target resolution failed")??;
        let path_permit = self.path_permit(&resolved_path).await;
        let _permit = path_permit
            .acquire_owned()
            .await
            .map_err(|_| anyhow!("write serialization closed unexpectedly"))?;

        tokio::task::spawn_blocking(move || {
            let current_path = resolve(&requested_path)?;
            if current_path != resolved_path {
                bail!(
                    "filePath resolved to a different target while waiting to write '{}'.",
                    requested_path.display()
                );
            }
            operation(&resolved_path)
        })
        .await
        .context("blocking file write task failed")?
    }
}

pub(crate) fn persist_in_place(path: &Path, content: &[u8], create: bool) -> Result<()> {
    let mut options = OpenOptions::new();
    options.write(true).create(create);
    let mut file = options
        .open(path)
        .with_context(|| format!("failed to open file '{}' for writing", path.display()))?;
    let metadata = file
        .metadata()
        .with_context(|| format!("failed to inspect opened file '{}'", path.display()))?;
    if !metadata.is_file() {
        bail!("target is not a regular file: {}", path.display());
    }

    file.seek(SeekFrom::Start(0))
        .with_context(|| format!("failed to seek file '{}'", path.display()))?;
    file.write_all(content)
        .with_context(|| format!("failed to write file '{}'", path.display()))?;
    let content_length = u64::try_from(content.len())
        .with_context(|| format!("content is too large for file '{}'", path.display()))?;
    file.set_len(content_length)
        .with_context(|| format!("failed to truncate file '{}'", path.display()))?;
    file.sync_all()
        .with_context(|| format!("failed to sync file '{}'", path.display()))?;
    Ok(())
}

fn resolve_existing_target(path: &Path) -> Result<PathBuf> {
    let metadata = std::fs::metadata(path)
        .with_context(|| format!("failed to inspect file '{}'", path.display()))?;
    if !metadata.is_file() {
        bail!("target is not a regular file: {}", path.display());
    }

    canonical_regular_file(path)
}

fn resolve_overwrite_target(path: &Path) -> Result<PathBuf> {
    resolve_overwrite_target_inner(path.to_path_buf(), &mut HashSet::new())
}

fn resolve_overwrite_target_inner(
    path: PathBuf,
    visited_links: &mut HashSet<PathBuf>,
) -> Result<PathBuf> {
    match std::fs::symlink_metadata(&path) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            let resolved_link = resolve_parent_and_join(&path)?;
            if !visited_links.insert(resolved_link.clone()) {
                bail!(
                    "symbolic link cycle while resolving file '{}'.",
                    path.display()
                );
            }
            let link_target = std::fs::read_link(&resolved_link).with_context(|| {
                format!("failed to read symbolic link '{}'", resolved_link.display())
            })?;
            let next_path = if link_target.is_absolute() {
                link_target
            } else {
                resolved_link
                    .parent()
                    .context("symbolic link path must have a parent directory.")?
                    .join(link_target)
            };
            resolve_overwrite_target_inner(next_path, visited_links)
        },
        Ok(metadata) if metadata.is_file() => canonical_regular_file(&path),
        Ok(_) => bail!("target is not a regular file: {}", path.display()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            resolve_missing_target(&path, visited_links)
        },
        Err(error) => Err(error)
            .with_context(|| format!("failed to inspect target file '{}'", path.display())),
    }
}

fn resolve_missing_target(path: &Path, visited_links: &mut HashSet<PathBuf>) -> Result<PathBuf> {
    let resolved_path = resolve_parent_and_join(path)?;
    match std::fs::symlink_metadata(&resolved_path) {
        Ok(_) => resolve_overwrite_target_inner(resolved_path, visited_links),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            ensure_utf8_path(&resolved_path)?;
            Ok(resolved_path)
        },
        Err(error) => Err(error).with_context(|| {
            format!(
                "failed to inspect target file '{}'",
                resolved_path.display()
            )
        }),
    }
}

fn resolve_parent_and_join(path: &Path) -> Result<PathBuf> {
    let file_name = path.file_name().context("filePath must identify a file.")?;
    let parent = path
        .parent()
        .context("filePath must have a parent directory.")?;
    let canonical_parent = std::fs::canonicalize(parent).with_context(|| {
        format!(
            "failed to resolve parent directory for '{}'",
            path.display()
        )
    })?;
    let parent_metadata = std::fs::metadata(&canonical_parent).with_context(|| {
        format!(
            "failed to inspect parent directory '{}'",
            canonical_parent.display()
        )
    })?;
    if !parent_metadata.is_dir() {
        bail!(
            "parent path is not a directory: {}",
            canonical_parent.display()
        );
    }

    Ok(canonical_parent.join(file_name))
}

fn canonical_regular_file(path: &Path) -> Result<PathBuf> {
    let canonical_path = std::fs::canonicalize(path)
        .with_context(|| format!("failed to resolve file '{}'", path.display()))?;
    let metadata = std::fs::metadata(&canonical_path).with_context(|| {
        format!(
            "failed to inspect resolved file '{}'",
            canonical_path.display()
        )
    })?;
    if !metadata.is_file() {
        bail!(
            "resolved target is not a regular file: {}",
            canonical_path.display()
        );
    }
    ensure_utf8_path(&canonical_path)?;
    Ok(canonical_path)
}

fn ensure_utf8_path(path: &Path) -> Result<()> {
    if path.to_str().is_none() {
        bail!("resolved filePath contains invalid UTF-8.");
    }
    Ok(())
}
