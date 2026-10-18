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
        // Create workspace files.
        std::fs::write(src_data.path().join("SOUL.md"), "# Soul\nBe helpful.").unwrap();
        std::fs::write(src_data.path().join("IDENTITY.md"), "name: Chelix").unwrap();

        // Create a session JSONL file.
        let sessions_dir = src_data.path().join("sessions");
        std::fs::create_dir_all(&sessions_dir).unwrap();
        std::fs::write(
            sessions_dir.join("t_1.jsonl"),
            "{\"role\":\"user\",\"content\":\"hello\"}\n",
        )
        .unwrap();

        // Export.
        let opts = ExportOptions {
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
        assert!(dst_data.path().join("SOUL.md").exists());
        assert!(dst_data.path().join("IDENTITY.md").exists());
        assert!(!dst_data.path().join("sessions/t_1.jsonl").exists());

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

    #[tokio::test]
    async fn export_strips_secrets_from_a_quoted_path() {
        let archive = secret_session_archive().await;
        let db_bytes = exported_chelix_db(&archive);
        for secret in [
            "SECRET-PASSWORD",
            "SECRET-SESSION",
            "SECRET-API-KEY",
            "SECRET-PASSKEY",
            "SECRET-SHARE",
        ] {
            assert!(
                !db_bytes
                    .windows(secret.len())
                    .any(|window| window == secret.as_bytes()),
                "{secret} remains in the exported database"
            );
        }
        let check_dir = tempfile::tempdir().unwrap();
        let check_path = check_dir.path().join("chelix.db");
        std::fs::write(&check_path, &db_bytes).unwrap();
        let check = sqlx::sqlite::SqlitePoolOptions::new()
            .connect_with(
                sqlx::sqlite::SqliteConnectOptions::new()
                    .filename(&check_path)
                    .read_only(true),
            )
            .await
            .unwrap();
        for statement in [
            "SELECT COUNT(*) FROM auth_password",
            "SELECT COUNT(*) FROM auth_sessions",
            "SELECT COUNT(*) FROM api_keys",
            "SELECT COUNT(*) FROM passkeys",
            "SELECT COUNT(*) FROM session_shares",
        ] {
            let count: i64 = sqlx::query_scalar(statement)
                .fetch_one(&check)
                .await
                .unwrap();
            assert_eq!(count, 0, "{statement}");
        }
        let exported_id: String = sqlx::query_scalar("SELECT id FROM sessions WHERE key = 'keep'")
            .fetch_one(&check)
            .await
            .unwrap();
        assert_eq!(exported_id, "exported");
        check.close().await;
    }

    #[tokio::test]
    async fn import_merges_sessions_for_a_quoted_path() {
        let archive = secret_session_archive().await;
        let dst_parent = tempfile::tempdir().unwrap();
        let dst_data = dst_parent.path().join("o'quote");
        std::fs::create_dir_all(&dst_data).unwrap();
        let dst_config = tempfile::tempdir().unwrap();
        let live_path = dst_data.join("chelix.db");
        let live = sqlx::sqlite::SqlitePoolOptions::new()
            .connect_with(
                sqlx::sqlite::SqliteConnectOptions::new()
                    .filename(&live_path)
                    .create_if_missing(true),
            )
            .await
            .unwrap();
        sqlx::query("CREATE TABLE sessions (key TEXT PRIMARY KEY, id TEXT NOT NULL)")
            .execute(&live)
            .await
            .unwrap();
        sqlx::query("INSERT INTO sessions (key, id) VALUES ('keep', 'local')")
            .execute(&live)
            .await
            .unwrap();
        live.close().await;

        let skipped = import_archive(
            dst_config.path(),
            &dst_data,
            &ImportOptions {
                conflict: ConflictStrategy::Skip,
                dry_run: false,
            },
            Cursor::new(archive.clone()),
        )
        .await
        .unwrap()
        .result;
        assert!(
            !skipped
                .warnings
                .iter()
                .any(|warning| warning.contains("failed to merge chelix.db"))
        );
        let live = sqlx::sqlite::SqlitePoolOptions::new()
            .connect_with(
                sqlx::sqlite::SqliteConnectOptions::new()
                    .filename(&live_path)
                    .read_only(true),
            )
            .await
            .unwrap();
        let id: String = sqlx::query_scalar("SELECT id FROM sessions WHERE key = 'keep'")
            .fetch_one(&live)
            .await
            .unwrap();
        assert_eq!(id, "local");
        live.close().await;

        let overwritten = import_archive(
            dst_config.path(),
            &dst_data,
            &ImportOptions {
                conflict: ConflictStrategy::Overwrite,
                dry_run: false,
            },
            Cursor::new(archive),
        )
        .await
        .unwrap()
        .result;
        assert!(
            !overwritten
                .warnings
                .iter()
                .any(|warning| warning.contains("failed to merge chelix.db"))
        );
        let live = sqlx::sqlite::SqlitePoolOptions::new()
            .connect_with(
                sqlx::sqlite::SqliteConnectOptions::new()
                    .filename(&live_path)
                    .read_only(true),
            )
            .await
            .unwrap();
        let id: String = sqlx::query_scalar("SELECT id FROM sessions WHERE key = 'keep'")
            .fetch_one(&live)
            .await
            .unwrap();
        assert_eq!(id, "exported");
        live.close().await;
    }

    async fn secret_session_archive() -> Vec<u8> {
        let parent = tempfile::tempdir().unwrap();
        let src_data = parent.path().join("o'quote");
        std::fs::create_dir_all(&src_data).unwrap();
        let src_config = tempfile::tempdir().unwrap();
        let db_path = src_data.join("chelix.db");
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .connect_with(
                sqlx::sqlite::SqliteConnectOptions::new()
                    .filename(&db_path)
                    .create_if_missing(true),
            )
            .await
            .unwrap();
        sqlx::query("CREATE TABLE sessions (key TEXT PRIMARY KEY, id TEXT NOT NULL)")
            .execute(&pool)
            .await
            .unwrap();
        create_auth_tables(&pool).await;
        sqlx::query("INSERT INTO sessions (key, id) VALUES ('keep', 'exported')")
            .execute(&pool)
            .await
            .unwrap();
        for (statement, secret) in [
            (
                "INSERT INTO auth_password (secret) VALUES (?)",
                "SECRET-PASSWORD",
            ),
            (
                "INSERT INTO auth_sessions (secret) VALUES (?)",
                "SECRET-SESSION",
            ),
            ("INSERT INTO api_keys (secret) VALUES (?)", "SECRET-API-KEY"),
            ("INSERT INTO passkeys (secret) VALUES (?)", "SECRET-PASSKEY"),
            (
                "INSERT INTO session_shares (secret) VALUES (?)",
                "SECRET-SHARE",
            ),
        ] {
            sqlx::query(statement)
                .bind(secret)
                .execute(&pool)
                .await
                .unwrap();
        }
        pool.close().await;
        let mut archive = Vec::new();
        export_archive(
            src_config.path(),
            &src_data,
            &ExportOptions::default(),
            &mut archive,
        )
        .await
        .unwrap();
        archive
    }

    #[tokio::test]
    async fn import_rolls_back_sessions_when_a_later_table_mismatches() {
        let parent = tempfile::tempdir().unwrap();
        let src_data = parent.path().join("o'quote");
        std::fs::create_dir_all(&src_data).unwrap();
        let src_config = tempfile::tempdir().unwrap();
        let db_path = src_data.join("chelix.db");
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .connect_with(
                sqlx::sqlite::SqliteConnectOptions::new()
                    .filename(&db_path)
                    .create_if_missing(true),
            )
            .await
            .unwrap();
        sqlx::query("CREATE TABLE sessions (key TEXT PRIMARY KEY, id TEXT NOT NULL)")
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("CREATE TABLE channel_sessions (id TEXT PRIMARY KEY, extra TEXT NOT NULL)")
            .execute(&pool)
            .await
            .unwrap();
        create_auth_tables(&pool).await;
        sqlx::query("INSERT INTO sessions (key, id) VALUES ('keep', 'exported')")
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("INSERT INTO channel_sessions (id, extra) VALUES ('row', 'extra')")
            .execute(&pool)
            .await
            .unwrap();
        pool.close().await;
        let mut archive = Vec::new();
        export_archive(
            src_config.path(),
            &src_data,
            &ExportOptions::default(),
            &mut archive,
        )
        .await
        .unwrap();

        let dst_parent = tempfile::tempdir().unwrap();
        let dst_data = dst_parent.path().join("o'quote");
        std::fs::create_dir_all(&dst_data).unwrap();
        let dst_config = tempfile::tempdir().unwrap();
        let live_path = dst_data.join("chelix.db");
        let live = sqlx::sqlite::SqlitePoolOptions::new()
            .connect_with(
                sqlx::sqlite::SqliteConnectOptions::new()
                    .filename(&live_path)
                    .create_if_missing(true),
            )
            .await
            .unwrap();
        sqlx::query("CREATE TABLE sessions (key TEXT PRIMARY KEY, id TEXT NOT NULL)")
            .execute(&live)
            .await
            .unwrap();
        sqlx::query("INSERT INTO sessions (key, id) VALUES ('keep', 'local')")
            .execute(&live)
            .await
            .unwrap();
        sqlx::query("CREATE TABLE channel_sessions (id TEXT PRIMARY KEY)")
            .execute(&live)
            .await
            .unwrap();
        live.close().await;

        let imported = import_archive(
            dst_config.path(),
            &dst_data,
            &ImportOptions {
                conflict: ConflictStrategy::Overwrite,
                dry_run: false,
            },
            Cursor::new(archive),
        )
        .await
        .unwrap()
        .result;
        assert!(
            imported
                .warnings
                .iter()
                .any(|warning| warning.contains("failed to merge chelix.db"))
        );
        let live = sqlx::sqlite::SqlitePoolOptions::new()
            .connect_with(
                sqlx::sqlite::SqliteConnectOptions::new()
                    .filename(&live_path)
                    .read_only(true),
            )
            .await
            .unwrap();
        let id: String = sqlx::query_scalar("SELECT id FROM sessions WHERE key = 'keep'")
            .fetch_one(&live)
            .await
            .unwrap();
        assert_eq!(id, "local");
        live.close().await;
    }

    async fn create_auth_tables(pool: &sqlx::SqlitePool) {
        for statement in [
            "CREATE TABLE auth_password (id INTEGER PRIMARY KEY, secret TEXT NOT NULL)",
            "CREATE TABLE auth_sessions (id INTEGER PRIMARY KEY, secret TEXT NOT NULL)",
            "CREATE TABLE api_keys (id INTEGER PRIMARY KEY, secret TEXT NOT NULL)",
            "CREATE TABLE passkeys (id INTEGER PRIMARY KEY, secret TEXT NOT NULL)",
            "CREATE TABLE session_shares (id INTEGER PRIMARY KEY, secret TEXT NOT NULL)",
        ] {
            sqlx::query(statement).execute(pool).await.unwrap();
        }
    }

    fn exported_chelix_db(archive: &[u8]) -> Vec<u8> {
        let decoder = flate2::read::GzDecoder::new(Cursor::new(archive));
        let mut tar = tar::Archive::new(decoder);
        for entry in tar.entries().unwrap() {
            let mut entry = entry.unwrap();
            let path = entry.path().unwrap().to_string_lossy().into_owned();
            if path.ends_with("/db/chelix.db") {
                let mut bytes = Vec::new();
                std::io::Read::read_to_end(&mut entry, &mut bytes).unwrap();
                return bytes;
            }
        }
        panic!("archive has no chelix.db snapshot");
    }
}
