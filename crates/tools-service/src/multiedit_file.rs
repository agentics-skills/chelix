//! Atomic sequential text edits for the managed tools service.

use {
    anyhow::{Result, anyhow, bail},
    chelix_protocol::{MultieditFileRequest, MultieditFileResponse},
    std::path::PathBuf,
    tracing::instrument,
};

use crate::file_edit::{FileEditRuntime, apply_edit, persist_atomic, read_utf8, response_path};

#[instrument(
    skip_all,
    fields(path = %request.file_path, edit_count = request.edits.len())
)]
pub(crate) async fn run_tool(
    request: MultieditFileRequest,
    runtime: &FileEditRuntime,
) -> Result<MultieditFileResponse> {
    request.validate().map_err(anyhow::Error::from)?;
    let requested_path = PathBuf::from(&request.file_path);
    if !requested_path.is_absolute() {
        bail!("filePath must be absolute.");
    }

    runtime
        .run(requested_path, move |canonical_path| {
            let mut content = read_utf8(canonical_path)?;
            let mut replacements_per_edit = Vec::with_capacity(request.edits.len());
            let mut recoveries_per_edit = Vec::with_capacity(request.edits.len());

            for (index, edit) in request.edits.iter().enumerate() {
                let outcome = apply_edit(
                    &content,
                    edit.old_string(),
                    edit.new_string(),
                    edit.replace_all(),
                )
                .map_err(|error| anyhow!("edit #{}: {error}", index + 1))?;
                replacements_per_edit.push(outcome.replacements);
                recoveries_per_edit.push(outcome.recovery);
                content = outcome.content;
            }

            persist_atomic(canonical_path, content.as_bytes())?;
            Ok(MultieditFileResponse {
                file_path: response_path(canonical_path)?,
                edits_applied: request.edits.len(),
                replacements_per_edit,
                recoveries_per_edit,
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
            EditFileOperation, EditFileRecovery, EditFileReplaceAllOperation, EditFileRequest,
            EditFileUniqueOperation,
        },
        std::path::Path,
    };

    fn unique(old_string: &str, new_string: &str) -> EditFileOperation {
        EditFileOperation::Unique(EditFileUniqueOperation {
            old_string: old_string.into(),
            new_string: new_string.into(),
        })
    }

    fn replace_all(old_string: &str, new_string: &str) -> EditFileOperation {
        EditFileOperation::WithReplaceAll(EditFileReplaceAllOperation {
            old_string: old_string.into(),
            new_string: new_string.into(),
            replace_all: true,
        })
    }

    fn request(path: &Path, edits: Vec<EditFileOperation>) -> MultieditFileRequest {
        MultieditFileRequest {
            file_path: path.to_string_lossy().into_owned(),
            edits,
        }
    }

    #[tokio::test]
    async fn applies_edits_sequentially_and_reports_each_outcome() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("sample.txt");
        tokio::fs::write(&path, "one foo foo\r\nthree\r\n")
            .await
            .unwrap();

        let response = run_tool(
            request(&path, vec![
                unique("one", "two"),
                unique("two", "three"),
                replace_all("foo", "bar"),
                unique("three bar bar\nthree", "THREE bar bar\nTHREE"),
            ]),
            &FileEditRuntime::default(),
        )
        .await
        .unwrap();

        assert_eq!(response.edits_applied, 4);
        assert_eq!(response.replacements_per_edit, vec![1, 1, 2, 1]);
        assert_eq!(response.recoveries_per_edit, vec![
            None,
            None,
            None,
            Some(EditFileRecovery::Crlf)
        ]);
        assert_eq!(
            tokio::fs::read_to_string(&path).await.unwrap(),
            "THREE bar bar\r\nTHREE\r\n"
        );
    }

    #[tokio::test]
    async fn rolls_back_entire_batch_when_any_edit_fails() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("sample.txt");
        tokio::fs::write(&path, "alpha beta").await.unwrap();

        let error = run_tool(
            request(&path, vec![
                unique("alpha", "ALPHA"),
                unique("missing", "value"),
            ]),
            &FileEditRuntime::default(),
        )
        .await
        .unwrap_err();

        assert!(error.to_string().contains("edit #2"));
        assert!(error.to_string().contains("not found"));
        assert_eq!(
            tokio::fs::read_to_string(&path).await.unwrap(),
            "alpha beta"
        );
    }

    #[tokio::test]
    async fn rejects_non_unique_edit_without_modifying_file() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("sample.txt");
        tokio::fs::write(&path, "foo foo").await.unwrap();

        let error = run_tool(
            request(&path, vec![unique("foo", "bar")]),
            &FileEditRuntime::default(),
        )
        .await
        .unwrap_err();

        assert!(error.to_string().contains("edit #1"));
        assert!(error.to_string().contains("matches 2 locations"));
        assert_eq!(tokio::fs::read_to_string(&path).await.unwrap(), "foo foo");
    }

    #[tokio::test]
    async fn reports_smart_quote_recovery_per_edit() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("sample.txt");
        tokio::fs::write(&path, "Keep ‘this’ and replace “quoted text”.")
            .await
            .unwrap();

        let response = run_tool(
            request(&path, vec![unique("\"quoted text\"", "the value")]),
            &FileEditRuntime::default(),
        )
        .await
        .unwrap();

        assert_eq!(response.recoveries_per_edit, vec![Some(
            EditFileRecovery::SmartQuotes
        )]);
        assert_eq!(
            tokio::fs::read_to_string(&path).await.unwrap(),
            "Keep ‘this’ and replace the value."
        );
    }

    #[tokio::test]
    async fn shares_same_target_serialization_with_edit_file() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("sample.txt");
        tokio::fs::write(&path, "alpha beta gamma").await.unwrap();
        let runtime = FileEditRuntime::default();
        let single = EditFileRequest {
            file_path: path.to_string_lossy().into_owned(),
            edit: unique("alpha", "ALPHA"),
        };
        let batch = request(&path, vec![
            unique("beta", "BETA"),
            unique("gamma", "GAMMA"),
        ]);

        let (single_result, batch_result) = tokio::join!(
            crate::edit_file::run_tool(single, &runtime),
            run_tool(batch, &runtime),
        );
        single_result.unwrap();
        batch_result.unwrap();

        assert_eq!(
            tokio::fs::read_to_string(&path).await.unwrap(),
            "ALPHA BETA GAMMA"
        );
    }

    #[tokio::test]
    async fn rejects_invalid_targets_and_requests_without_modifying_files() {
        let runtime = FileEditRuntime::default();
        let relative = MultieditFileRequest {
            file_path: "relative.txt".into(),
            edits: vec![unique("old", "new")],
        };
        assert_eq!(
            run_tool(relative, &runtime).await.unwrap_err().to_string(),
            "filePath must be absolute."
        );

        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("sample.txt");
        tokio::fs::write(&path, "old").await.unwrap();
        assert!(
            run_tool(
                request(directory.path(), vec![unique("old", "new")]),
                &runtime
            )
            .await
            .unwrap_err()
            .to_string()
            .contains("not a regular file")
        );
        let missing = directory.path().join("missing.txt");
        assert!(
            run_tool(request(&missing, vec![unique("old", "new")]), &runtime)
                .await
                .unwrap_err()
                .to_string()
                .contains("failed to inspect file")
        );
        assert_eq!(
            run_tool(request(&path, Vec::new()), &runtime)
                .await
                .unwrap_err()
                .to_string(),
            "edits must contain at least one edit."
        );
        assert_eq!(tokio::fs::read_to_string(&path).await.unwrap(), "old");

        let binary = directory.path().join("binary.bin");
        tokio::fs::write(&binary, [0xff, 0xfe]).await.unwrap();
        assert!(
            run_tool(request(&binary, vec![unique("old", "new")]), &runtime)
                .await
                .unwrap_err()
                .to_string()
                .contains("UTF-8")
        );

        #[cfg(unix)]
        {
            let link = directory.path().join("link.txt");
            std::os::unix::fs::symlink(&path, &link).unwrap();
            assert!(
                run_tool(request(&link, vec![unique("old", "new")]), &runtime)
                    .await
                    .unwrap_err()
                    .to_string()
                    .contains("symbolic link")
            );
            assert_eq!(tokio::fs::read_to_string(&path).await.unwrap(), "old");
        }
    }
}
