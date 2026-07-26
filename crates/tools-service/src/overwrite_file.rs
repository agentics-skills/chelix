//! Atomic whole-file replacement for the managed tools service.

use {
    anyhow::{Context, Result, bail},
    chelix_protocol::{OverwriteFileRequest, OverwriteFileResponse},
    std::{
        io::{ErrorKind, Write as _},
        path::{Path, PathBuf},
    },
    tracing::instrument,
};

#[instrument(
    skip_all,
    fields(path = %request.file_path, bytes = request.content.len())
)]
pub(crate) async fn run_tool(request: OverwriteFileRequest) -> Result<OverwriteFileResponse> {
    request.validate().map_err(anyhow::Error::from)?;
    let path = PathBuf::from(&request.file_path);
    if !path.is_absolute() {
        bail!("filePath must be absolute.");
    }

    let bytes = request.content.into_bytes();
    tokio::task::spawn_blocking(move || overwrite(path, bytes))
        .await
        .context("blocking overwrite task failed")?
}

fn overwrite(path: PathBuf, bytes: Vec<u8>) -> Result<OverwriteFileResponse> {
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

    let target = canonical_parent.join(file_name);
    let target_string = target
        .to_str()
        .context("resolved filePath contains invalid UTF-8.")?
        .to_owned();
    let mut temporary = tempfile::NamedTempFile::new_in(&canonical_parent).with_context(|| {
        format!(
            "failed to create temporary file in '{}'",
            canonical_parent.display()
        )
    })?;
    temporary
        .write_all(&bytes)
        .with_context(|| format!("failed to write temporary file for '{target_string}'"))?;
    temporary
        .as_file()
        .sync_all()
        .with_context(|| format!("failed to sync temporary file for '{target_string}'"))?;

    reject_invalid_target(&target)?;
    temporary
        .persist(&target)
        .map_err(|error| error.error)
        .with_context(|| format!("failed to replace file '{target_string}'"))?;

    Ok(OverwriteFileResponse {
        file_path: target_string,
        bytes_written: bytes.len(),
    })
}

fn reject_invalid_target(target: &Path) -> Result<()> {
    match std::fs::symlink_metadata(target) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            bail!("refusing to overwrite symbolic link '{}'", target.display())
        },
        Ok(metadata) if !metadata.is_file() => {
            bail!("target is not a regular file: {}", target.display())
        },
        Ok(_) => Ok(()),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error)
            .with_context(|| format!("failed to inspect target file '{}'", target.display())),
    }
}

#[allow(clippy::unwrap_used, clippy::expect_used)]
#[cfg(test)]
mod tests {
    use super::*;

    fn request(path: &Path, content: impl Into<String>) -> OverwriteFileRequest {
        OverwriteFileRequest {
            file_path: path.to_string_lossy().into_owned(),
            content: content.into(),
        }
    }

    #[tokio::test]
    async fn creates_overwrites_and_truncates_files() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("sample.txt");
        let resolved_path = std::fs::canonicalize(directory.path())
            .unwrap()
            .join("sample.txt");

        let created = run_tool(request(&path, "first value\n")).await.unwrap();
        assert_eq!(Path::new(&created.file_path), resolved_path);
        assert_eq!(created.bytes_written, 12);
        assert_eq!(
            tokio::fs::read_to_string(&path).await.unwrap(),
            "first value\n"
        );

        let overwritten = run_tool(request(&path, "replacement")).await.unwrap();
        assert_eq!(overwritten.bytes_written, 11);
        assert_eq!(
            tokio::fs::read_to_string(&path).await.unwrap(),
            "replacement"
        );

        let truncated = run_tool(request(&path, "")).await.unwrap();
        assert_eq!(truncated.bytes_written, 0);
        assert_eq!(tokio::fs::read(&path).await.unwrap(), Vec::<u8>::new());
    }

    #[tokio::test]
    async fn concurrent_overwrites_never_expose_combined_content() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("sample.txt");
        let first = "a".repeat(512 * 1024);
        let second = "b".repeat(512 * 1024);

        let (first_result, second_result) = tokio::join!(
            run_tool(request(&path, first.clone())),
            run_tool(request(&path, second.clone()))
        );
        first_result.unwrap();
        second_result.unwrap();

        let actual = tokio::fs::read_to_string(path).await.unwrap();
        assert!(actual == first || actual == second);
    }

    #[tokio::test]
    async fn rejects_relative_missing_parent_and_directory_paths() {
        let relative = OverwriteFileRequest {
            file_path: "relative.txt".into(),
            content: "value".into(),
        };
        assert_eq!(
            run_tool(relative).await.unwrap_err().to_string(),
            "filePath must be absolute."
        );

        let directory = tempfile::tempdir().unwrap();
        let missing = directory.path().join("missing").join("file.txt");
        assert!(
            run_tool(request(&missing, "value"))
                .await
                .unwrap_err()
                .to_string()
                .contains("failed to resolve parent directory")
        );
        assert!(
            run_tool(request(directory.path(), "value"))
                .await
                .unwrap_err()
                .to_string()
                .contains("target is not a regular file")
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn rejects_symlink_without_modifying_its_target() {
        let directory = tempfile::tempdir().unwrap();
        let target = directory.path().join("target.txt");
        let link = directory.path().join("link.txt");
        tokio::fs::write(&target, "original").await.unwrap();
        std::os::unix::fs::symlink(&target, &link).unwrap();

        let error = run_tool(request(&link, "replacement")).await.unwrap_err();

        assert!(error.to_string().contains("symbolic link"));
        assert_eq!(
            tokio::fs::read_to_string(&target).await.unwrap(),
            "original"
        );
        assert!(
            tokio::fs::symlink_metadata(&link)
                .await
                .unwrap()
                .file_type()
                .is_symlink()
        );
    }
}
