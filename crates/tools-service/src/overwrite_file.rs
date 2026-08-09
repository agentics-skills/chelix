//! Whole-file in-place writes for the managed tools service.

use {
    anyhow::{Context, Result, bail},
    chelix_protocol::{OverwriteFileRequest, OverwriteFileResponse},
    std::path::PathBuf,
    tracing::instrument,
};

use crate::file_write::{FileWriteRuntime, persist_in_place};

#[instrument(
    skip_all,
    fields(path = %request.file_path, bytes = request.content.len())
)]
pub(crate) async fn run_tool(
    request: OverwriteFileRequest,
    runtime: &FileWriteRuntime,
) -> Result<OverwriteFileResponse> {
    request.validate().map_err(anyhow::Error::from)?;
    let requested_path = PathBuf::from(&request.file_path);
    if !requested_path.is_absolute() {
        bail!("filePath must be absolute.");
    }

    let bytes = request.content.into_bytes();
    runtime
        .run_create_or_existing(requested_path, move |resolved_path| {
            persist_in_place(resolved_path, &bytes, true)?;
            Ok(OverwriteFileResponse {
                file_path: resolved_path
                    .to_str()
                    .context("resolved filePath contains invalid UTF-8.")?
                    .to_owned(),
                bytes_written: bytes.len(),
            })
        })
        .await
}

#[allow(clippy::unwrap_used, clippy::expect_used)]
#[cfg(test)]
mod tests {
    use {super::*, std::path::Path};

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
        let runtime = FileWriteRuntime::default();

        let created = run_tool(request(&path, "first value\n"), &runtime)
            .await
            .unwrap();
        assert_eq!(Path::new(&created.file_path), resolved_path);
        assert_eq!(created.bytes_written, 12);
        assert_eq!(
            tokio::fs::read_to_string(&path).await.unwrap(),
            "first value\n"
        );

        let overwritten = run_tool(request(&path, "replacement"), &runtime)
            .await
            .unwrap();
        assert_eq!(overwritten.bytes_written, 11);
        assert_eq!(
            tokio::fs::read_to_string(&path).await.unwrap(),
            "replacement"
        );

        let truncated = run_tool(request(&path, ""), &runtime).await.unwrap();
        assert_eq!(truncated.bytes_written, 0);
        assert_eq!(tokio::fs::read(&path).await.unwrap(), Vec::<u8>::new());
    }

    #[tokio::test]
    async fn serializes_concurrent_overwrites() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("sample.txt");
        let first = "a".repeat(512 * 1024);
        let second = "b".repeat(512 * 1024);
        let runtime = FileWriteRuntime::default();

        let (first_result, second_result) = tokio::join!(
            run_tool(request(&path, first.clone()), &runtime),
            run_tool(request(&path, second.clone()), &runtime)
        );
        first_result.unwrap();
        second_result.unwrap();

        let actual = tokio::fs::read_to_string(path).await.unwrap();
        assert!(actual == first || actual == second);
    }

    #[tokio::test]
    async fn rejects_relative_missing_parent_and_directory_paths() {
        let runtime = FileWriteRuntime::default();
        let relative = OverwriteFileRequest {
            file_path: "relative.txt".into(),
            content: "value".into(),
        };
        assert_eq!(
            run_tool(relative, &runtime).await.unwrap_err().to_string(),
            "filePath must be absolute."
        );

        let directory = tempfile::tempdir().unwrap();
        let missing = directory.path().join("missing").join("file.txt");
        assert!(
            run_tool(request(&missing, "value"), &runtime)
                .await
                .unwrap_err()
                .to_string()
                .contains("failed to resolve parent directory")
        );
        assert!(
            run_tool(request(directory.path(), "value"), &runtime)
                .await
                .unwrap_err()
                .to_string()
                .contains("target is not a regular file")
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn writes_through_symlinks_and_preserves_inode_and_permissions() {
        use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};

        let directory = tempfile::tempdir().unwrap();
        let target = directory.path().join("target.txt");
        let existing_link = directory.path().join("existing-link.txt");
        let dangling_target = directory.path().join("created.txt");
        let dangling_link = directory.path().join("dangling-link.txt");
        tokio::fs::write(&target, "original").await.unwrap();
        tokio::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o755))
            .await
            .unwrap();
        std::os::unix::fs::symlink("target.txt", &existing_link).unwrap();
        std::os::unix::fs::symlink("created.txt", &dangling_link).unwrap();
        let runtime = FileWriteRuntime::default();
        let before = tokio::fs::metadata(&target).await.unwrap();

        run_tool(request(&existing_link, "replacement"), &runtime)
            .await
            .unwrap();
        run_tool(request(&dangling_link, "created"), &runtime)
            .await
            .unwrap();

        let after = tokio::fs::metadata(&target).await.unwrap();
        assert_eq!(after.ino(), before.ino());
        assert_eq!(after.permissions().mode(), before.permissions().mode());
        assert_eq!(
            tokio::fs::read_to_string(&target).await.unwrap(),
            "replacement"
        );
        assert_eq!(
            tokio::fs::read_to_string(&dangling_target).await.unwrap(),
            "created"
        );
        assert!(
            tokio::fs::symlink_metadata(&existing_link)
                .await
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert!(
            tokio::fs::symlink_metadata(&dangling_link)
                .await
                .unwrap()
                .file_type()
                .is_symlink()
        );
    }
}
