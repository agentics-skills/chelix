//! Portable import/export of Chelix configuration, databases, and session data.
//!
//! Archives are `.tar.gz` files containing a manifest, config files, workspace
//! markdown, SQLite databases, and optionally session media.

mod export;
mod import;
mod manifest;

pub use {
    export::{ExportOptions, export_archive},
    import::{
        ConflictStrategy, ImportOptions, ImportResult, ImportedArchive, ImportedItem,
        import_archive,
    },
    manifest::{ArchiveInventory, ExportManifest, inspect_archive},
};

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod integration_tests {
    use {super::*, std::io::Cursor};

    #[tokio::test]
    async fn round_trip_export_import() {
        let src_config = tempfile::tempdir().unwrap();
        let src_data = tempfile::tempdir().unwrap();

        // Create some config files.
        std::fs::write(
            src_config.path().join("chelix.toml"),
            "[server]\nport = 8080\n",
        )
        .unwrap();
        std::fs::write(
            src_config.path().join("provider_keys.json"),
            r#"{"openai":{"apiKey":"sk-test"}}"#,
        )
        .unwrap();

        // Create workspace files.
        std::fs::write(src_data.path().join("SOUL.md"), "# Soul\nBe helpful.").unwrap();
        std::fs::write(src_data.path().join("IDENTITY.md"), "name: Chelix").unwrap();

        // Create a session JSONL file.
        let sessions_dir = src_data.path().join("sessions");
        std::fs::create_dir_all(&sessions_dir).unwrap();
        std::fs::write(
            sessions_dir.join("main.jsonl"),
            "{\"role\":\"user\",\"content\":\"hello\"}\n",
        )
        .unwrap();

        // Export.
        let opts = ExportOptions {
            include_provider_keys: true,
            include_media: false,
        };
        let mut archive_buf = Vec::new();
        let manifest = export_archive(src_config.path(), src_data.path(), &opts, &mut archive_buf)
            .await
            .unwrap();

        assert_eq!(manifest.format_version, 2);
        assert!(
            manifest
                .inventory
                .config_files
                .contains(&"chelix.toml".to_owned())
        );
        assert!(
            manifest
                .inventory
                .config_files
                .contains(&"provider_keys.json".to_owned())
        );
        assert!(
            manifest
                .inventory
                .workspace_files
                .contains(&"SOUL.md".to_owned())
        );
        assert_eq!(manifest.inventory.session_count(), 0);

        // Inspect.
        let inspected = inspect_archive(Cursor::new(&archive_buf)).unwrap();
        assert_eq!(inspected.format_version, manifest.format_version);

        // Import into a fresh destination.
        let dst_config = tempfile::tempdir().unwrap();
        let dst_data = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dst_data.path().join("sessions")).unwrap();

        let import_opts = ImportOptions {
            conflict: ConflictStrategy::Skip,
            dry_run: false,
        };
        let result = import_archive(
            dst_config.path(),
            dst_data.path(),
            &import_opts,
            Cursor::new(&archive_buf),
        )
        .await
        .unwrap()
        .result;

        // Verify files were created.
        assert!(dst_config.path().join("chelix.toml").exists());
        assert!(dst_config.path().join("provider_keys.json").exists());
        assert!(dst_data.path().join("SOUL.md").exists());
        assert!(dst_data.path().join("IDENTITY.md").exists());
        assert!(!dst_data.path().join("sessions/main.jsonl").exists());

        // Verify content.
        let toml = std::fs::read_to_string(dst_config.path().join("chelix.toml")).unwrap();
        assert!(toml.contains("port = 8080"));

        let soul = std::fs::read_to_string(dst_data.path().join("SOUL.md")).unwrap();
        assert!(soul.contains("Be helpful"));

        assert!(!result.imported.is_empty());
        assert!(
            !manifest
                .inventory
                .session_files
                .iter()
                .any(|file| file.ends_with(".jsonl"))
        );
        assert!(result.warnings.is_empty());
    }

    #[tokio::test]
    async fn import_skip_existing() {
        let src_config = tempfile::tempdir().unwrap();
        let src_data = tempfile::tempdir().unwrap();

        std::fs::write(src_config.path().join("chelix.toml"), "original").unwrap();
        std::fs::write(src_data.path().join("SOUL.md"), "original soul").unwrap();

        // Export.
        let mut buf = Vec::new();
        export_archive(
            src_config.path(),
            src_data.path(),
            &ExportOptions::default(),
            &mut buf,
        )
        .await
        .unwrap();

        // Create destination with pre-existing files.
        let dst_config = tempfile::tempdir().unwrap();
        let dst_data = tempfile::tempdir().unwrap();
        std::fs::write(dst_config.path().join("chelix.toml"), "modified").unwrap();
        std::fs::write(dst_data.path().join("SOUL.md"), "modified soul").unwrap();

        // Import with Skip strategy.
        let result = import_archive(
            dst_config.path(),
            dst_data.path(),
            &ImportOptions {
                conflict: ConflictStrategy::Skip,
                dry_run: false,
            },
            Cursor::new(&buf),
        )
        .await
        .unwrap()
        .result;

        // Existing files should NOT be overwritten.
        let toml = std::fs::read_to_string(dst_config.path().join("chelix.toml")).unwrap();
        assert_eq!(toml, "modified");
        assert!(!result.skipped.is_empty());
    }

    #[tokio::test]
    async fn import_overwrite_existing() {
        let src_config = tempfile::tempdir().unwrap();
        let src_data = tempfile::tempdir().unwrap();

        std::fs::write(src_config.path().join("chelix.toml"), "from-export").unwrap();

        let mut buf = Vec::new();
        export_archive(
            src_config.path(),
            src_data.path(),
            &ExportOptions::default(),
            &mut buf,
        )
        .await
        .unwrap();

        let dst_config = tempfile::tempdir().unwrap();
        let dst_data = tempfile::tempdir().unwrap();
        std::fs::write(dst_config.path().join("chelix.toml"), "local-version").unwrap();

        let result = import_archive(
            dst_config.path(),
            dst_data.path(),
            &ImportOptions {
                conflict: ConflictStrategy::Overwrite,
                dry_run: false,
            },
            Cursor::new(&buf),
        )
        .await
        .unwrap()
        .result;

        // File should be overwritten.
        let toml = std::fs::read_to_string(dst_config.path().join("chelix.toml")).unwrap();
        assert_eq!(toml, "from-export");
        assert!(result.imported.iter().any(|i| i.action == "overwritten"));
    }

    #[tokio::test]
    async fn dry_run_does_not_write() {
        let src_config = tempfile::tempdir().unwrap();
        let src_data = tempfile::tempdir().unwrap();

        std::fs::write(src_config.path().join("chelix.toml"), "content").unwrap();

        let mut buf = Vec::new();
        export_archive(
            src_config.path(),
            src_data.path(),
            &ExportOptions::default(),
            &mut buf,
        )
        .await
        .unwrap();

        let dst_config = tempfile::tempdir().unwrap();
        let dst_data = tempfile::tempdir().unwrap();

        let result = import_archive(
            dst_config.path(),
            dst_data.path(),
            &ImportOptions {
                conflict: ConflictStrategy::Skip,
                dry_run: true,
            },
            Cursor::new(&buf),
        )
        .await
        .unwrap()
        .result;

        // Nothing should be written.
        assert!(!dst_config.path().join("chelix.toml").exists());
        assert!(result.imported.is_empty());
        assert!(!result.skipped.is_empty());
    }

    fn archive_with_config_then_manifest(manifest: Option<&[u8]>) -> Vec<u8> {
        let toml = b"replacement";
        let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        {
            let mut builder = tar::Builder::new(&mut encoder);
            let mut header = tar::Header::new_gnu();
            header.set_size(toml.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();
            builder
                .append_data(
                    &mut header,
                    "chelix-backup/config/chelix.toml",
                    Cursor::new(&toml[..]),
                )
                .unwrap();
            if let Some(manifest) = manifest {
                let mut header = tar::Header::new_gnu();
                header.set_size(manifest.len() as u64);
                header.set_mode(0o644);
                header.set_cksum();
                builder
                    .append_data(
                        &mut header,
                        "chelix-backup/manifest.json",
                        Cursor::new(manifest),
                    )
                    .unwrap();
            }
            builder.finish().unwrap();
        }
        encoder.finish().unwrap()
    }

    #[tokio::test]
    async fn old_archive_is_rejected_before_any_file_is_written() {
        let archive = archive_with_config_then_manifest(Some(
            br#"{"format_version":1,"chelix_version":"old","created_at":"t","inventory":{"config_files":[],"workspace_files":[],"has_chelix_db":false,"has_memory_db":false,"session_files":[],"media_files":[]}}"#,
        ));
        let dst_config = tempfile::tempdir().unwrap();
        let dst_data = tempfile::tempdir().unwrap();
        std::fs::write(dst_config.path().join("chelix.toml"), "original").unwrap();
        let error = import_archive(
            dst_config.path(),
            dst_data.path(),
            &ImportOptions::default(),
            Cursor::new(archive),
        )
        .await
        .unwrap_err();
        assert!(error.to_string().contains("older than supported version"));
        assert_eq!(
            std::fs::read_to_string(dst_config.path().join("chelix.toml")).unwrap(),
            "original"
        );
    }

    #[tokio::test]
    async fn archive_without_manifest_is_rejected_before_any_file_is_written() {
        let archive = archive_with_config_then_manifest(None);
        let dst_config = tempfile::tempdir().unwrap();
        let dst_data = tempfile::tempdir().unwrap();
        std::fs::write(dst_config.path().join("chelix.toml"), "original").unwrap();
        let error = import_archive(
            dst_config.path(),
            dst_data.path(),
            &ImportOptions::default(),
            Cursor::new(archive),
        )
        .await
        .unwrap_err();
        assert!(error.to_string().contains("missing manifest.json"));
        assert_eq!(
            std::fs::read_to_string(dst_config.path().join("chelix.toml")).unwrap(),
            "original"
        );
    }
}
