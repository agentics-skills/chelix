//! Exact text replacement for the managed tools service.

use {
    anyhow::{Result, bail},
    chelix_protocol::{EditFileRequest, EditFileResponse},
    std::path::PathBuf,
    tracing::instrument,
};

use crate::{
    file_edit::{apply_edit, read_utf8, response_path},
    file_write::{FileWriteRuntime, persist_in_place},
};

#[instrument(
    skip_all,
    fields(path = %request.file_path, replace_all = request.edit.replace_all())
)]
pub(crate) async fn run_tool(
    request: EditFileRequest,
    runtime: &FileWriteRuntime,
) -> Result<EditFileResponse> {
    request.validate().map_err(anyhow::Error::from)?;
    let requested_path = PathBuf::from(&request.file_path);
    if !requested_path.is_absolute() {
        bail!("filePath must be absolute.");
    }

    runtime
        .run_existing(requested_path, move |canonical_path| {
            let content = read_utf8(canonical_path)?;
            let replace_all = request.edit.replace_all();
            let outcome = apply_edit(
                &content,
                request.edit.old_string(),
                request.edit.new_string(),
                replace_all,
            )?;
            persist_in_place(canonical_path, outcome.content.as_bytes(), false)?;

            Ok(EditFileResponse {
                file_path: response_path(canonical_path)?,
                replacements: outcome.replacements,
                replace_all,
                recovery: outcome.recovery,
            })
        })
        .await
}

#[allow(clippy::unwrap_used, clippy::expect_used)]
#[cfg(test)]
mod tests {
    use {
        super::*,
        chelix_protocol::{
            EditFileOperation, EditFileRecovery, EditFileReplaceAllOperation,
            EditFileUniqueOperation,
        },
        std::path::Path,
    };

    fn request(
        path: &Path,
        old_string: impl Into<String>,
        new_string: impl Into<String>,
        replace_all: bool,
    ) -> EditFileRequest {
        EditFileRequest {
            file_path: path.to_string_lossy().into_owned(),
            edit: EditFileOperation::WithReplaceAll(EditFileReplaceAllOperation {
                old_string: old_string.into(),
                new_string: new_string.into(),
                replace_all,
            }),
        }
    }

    fn unique_request(
        path: &Path,
        old_string: impl Into<String>,
        new_string: impl Into<String>,
    ) -> EditFileRequest {
        EditFileRequest {
            file_path: path.to_string_lossy().into_owned(),
            edit: EditFileOperation::Unique(EditFileUniqueOperation {
                old_string: old_string.into(),
                new_string: new_string.into(),
            }),
        }
    }

    #[tokio::test]
    async fn replaces_one_unique_literal_match() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("sample.txt");
        tokio::fs::write(&path, "alpha beta gamma").await.unwrap();

        let response = run_tool(
            unique_request(&path, "beta", "BETA"),
            &FileWriteRuntime::default(),
        )
        .await
        .unwrap();

        assert_eq!(response.replacements, 1);
        assert!(!response.replace_all);
        assert_eq!(response.recovery, None);
        assert_eq!(
            tokio::fs::read_to_string(&path).await.unwrap(),
            "alpha BETA gamma"
        );
    }

    #[tokio::test]
    async fn rejects_non_unique_and_missing_matches_without_modifying_file() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("sample.txt");
        tokio::fs::write(&path, "foo foo").await.unwrap();
        let runtime = FileWriteRuntime::default();

        let duplicate_error = run_tool(request(&path, "foo", "bar", false), &runtime)
            .await
            .unwrap_err();
        assert!(duplicate_error.to_string().contains("matches 2 locations"));
        assert_eq!(tokio::fs::read_to_string(&path).await.unwrap(), "foo foo");

        let missing_error = run_tool(request(&path, "missing", "value", false), &runtime)
            .await
            .unwrap_err();
        assert!(missing_error.to_string().contains("not found"));
        assert_eq!(tokio::fs::read_to_string(&path).await.unwrap(), "foo foo");
    }

    #[tokio::test]
    async fn replaces_all_literal_matches() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("sample.txt");
        tokio::fs::write(&path, "foo foo foo").await.unwrap();

        let response = run_tool(
            request(&path, "foo", "bar", true),
            &FileWriteRuntime::default(),
        )
        .await
        .unwrap();

        assert_eq!(response.replacements, 3);
        assert!(response.replace_all);
        assert_eq!(
            tokio::fs::read_to_string(&path).await.unwrap(),
            "bar bar bar"
        );
    }

    #[tokio::test]
    async fn recovers_lf_input_for_crlf_file() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("sample.txt");
        tokio::fs::write(&path, "one\r\ntwo\r\nthree\r\n")
            .await
            .unwrap();

        let response = run_tool(
            request(&path, "one\ntwo", "ONE\nTWO", false),
            &FileWriteRuntime::default(),
        )
        .await
        .unwrap();

        assert_eq!(response.recovery, Some(EditFileRecovery::Crlf));
        assert_eq!(
            tokio::fs::read_to_string(&path).await.unwrap(),
            "ONE\r\nTWO\r\nthree\r\n"
        );
    }

    #[tokio::test]
    async fn recovers_smart_quotes_without_changing_surrounding_quotes() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("sample.txt");
        tokio::fs::write(&path, "Keep ‘this’ and replace “quoted text”.")
            .await
            .unwrap();

        let response = run_tool(
            request(&path, "\"quoted text\"", "the value", false),
            &FileWriteRuntime::default(),
        )
        .await
        .unwrap();

        assert_eq!(response.recovery, Some(EditFileRecovery::SmartQuotes));
        assert_eq!(
            tokio::fs::read_to_string(&path).await.unwrap(),
            "Keep ‘this’ and replace the value."
        );
    }

    #[tokio::test]
    async fn serializes_edits_for_the_same_file() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("sample.txt");
        tokio::fs::write(&path, "alpha beta").await.unwrap();
        let runtime = FileWriteRuntime::default();

        let (first, second) = tokio::join!(
            run_tool(request(&path, "alpha", "ALPHA", false), &runtime),
            run_tool(request(&path, "beta", "BETA", false), &runtime),
        );
        first.unwrap();
        second.unwrap();

        assert_eq!(
            tokio::fs::read_to_string(&path).await.unwrap(),
            "ALPHA BETA"
        );
    }

    #[tokio::test]
    async fn rejects_relative_directory_and_non_utf8_targets() {
        let runtime = FileWriteRuntime::default();
        let relative = EditFileRequest {
            file_path: "relative.txt".into(),
            edit: EditFileOperation::Unique(EditFileUniqueOperation {
                old_string: "old".into(),
                new_string: "new".into(),
            }),
        };
        assert_eq!(
            run_tool(relative, &runtime).await.unwrap_err().to_string(),
            "filePath must be absolute."
        );

        let directory = tempfile::tempdir().unwrap();
        assert!(
            run_tool(request(directory.path(), "old", "new", false), &runtime)
                .await
                .unwrap_err()
                .to_string()
                .contains("not a regular file")
        );

        let binary = directory.path().join("binary.bin");
        tokio::fs::write(&binary, [0xff, 0xfe]).await.unwrap();
        assert!(
            run_tool(request(&binary, "old", "new", false), &runtime)
                .await
                .unwrap_err()
                .to_string()
                .contains("UTF-8")
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn writes_through_symlink_without_replacing_it() {
        let directory = tempfile::tempdir().unwrap();
        let target = directory.path().join("target.txt");
        let link = directory.path().join("link.txt");
        tokio::fs::write(&target, "old value").await.unwrap();
        std::os::unix::fs::symlink(&target, &link).unwrap();

        run_tool(
            request(&link, "old", "new", false),
            &FileWriteRuntime::default(),
        )
        .await
        .unwrap();

        assert_eq!(
            tokio::fs::read_to_string(&target).await.unwrap(),
            "new value"
        );
        assert!(
            tokio::fs::symlink_metadata(&link)
                .await
                .unwrap()
                .file_type()
                .is_symlink()
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn preserves_inode_and_permissions() {
        use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};

        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("script.sh");
        tokio::fs::write(&path, "#!/bin/sh\necho old\n")
            .await
            .unwrap();
        tokio::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
            .await
            .unwrap();
        let before = tokio::fs::metadata(&path).await.unwrap();

        run_tool(
            request(&path, "old", "new value", false),
            &FileWriteRuntime::default(),
        )
        .await
        .unwrap();

        let after = tokio::fs::metadata(&path).await.unwrap();
        assert_eq!(after.ino(), before.ino());
        assert_eq!(after.permissions().mode(), before.permissions().mode());
        assert_eq!(
            tokio::fs::read_to_string(&path).await.unwrap(),
            "#!/bin/sh\necho new value\n"
        );
    }
}
