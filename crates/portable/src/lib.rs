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

    fn valid_config_toml(port: u16) -> String {
        format!(
            "[server]\nport = {port}\n\n[tools.execute_command]\nterminal_size = \"115x58\"\n\n[sandbox]\narchived_session_retention_days = 3\n"
        )
    }

    fn write_valid_config(dir: &std::path::Path, port: u16) {
        std::fs::write(dir.join("chelix.toml"), valid_config_toml(port)).unwrap();
    }

    struct ConfigDirGuard {
        lock: std::sync::MutexGuard<'static, Vec<tempfile::TempDir>>,
    }

    impl ConfigDirGuard {
        fn install(dir: tempfile::TempDir) -> Self {
            static LOCK: std::sync::Mutex<Vec<tempfile::TempDir>> =
                std::sync::Mutex::new(Vec::new());
            let mut lock = LOCK.lock().unwrap_or_else(|error| error.into_inner());
            lock.push(dir);
            let guard = Self { lock };
            chelix_config::set_config_dir(guard.path().to_path_buf());
            guard
        }

        fn path(&self) -> &std::path::Path {
            self.lock
                .last()
                .unwrap_or_else(|| panic!("config override directory is not installed"))
                .path()
        }

        fn retarget(&mut self, dir: tempfile::TempDir) {
            self.lock.push(dir);
            chelix_config::set_config_dir(self.path().to_path_buf());
        }
    }

    impl Drop for ConfigDirGuard {
        fn drop(&mut self) {
            chelix_config::clear_config_dir();
            self.lock.clear();
        }
    }

    async fn open_config_service() -> chelix_config_service::ConfigService {
        match chelix_config_service::ConfigService::open(
            chelix_config_service::ConfigValidation::default(),
            chelix_config_service::ConfigInitialization::default(),
        )
        .await
        {
            Ok(service) => service,
            Err(error) => panic!("config service should open: {error}"),
        }
    }

    #[tokio::test]
    async fn round_trip_export_import() {
        let mut config_guard = ConfigDirGuard::install(tempfile::tempdir().unwrap());
        let src_config = config_guard.path().to_path_buf();
        let src_data = tempfile::tempdir().unwrap();
        write_valid_config(&src_config, 8080);
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
        let service = open_config_service().await;
        let manifest = export_archive(
            &service,
            src_config.as_path(),
            src_data.path(),
            &opts,
            &mut archive_buf,
        )
        .await
        .unwrap();
        drop(service);

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
        let dst_data = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dst_data.path().join("sessions")).unwrap();
        config_guard.retarget(tempfile::tempdir().unwrap());
        let dst_config = config_guard.path().to_path_buf();
        write_valid_config(&dst_config, 9090);
        let service = open_config_service().await;

        let import_opts = ImportOptions {
            conflict: ConflictStrategy::Overwrite,
            dry_run: false,
        };
        let result = import_archive(
            &service,
            dst_config.as_path(),
            dst_data.path(),
            &import_opts,
            Cursor::new(archive_buf.clone()),
        )
        .await
        .unwrap()
        .result;

        // Verify files were created.
        assert!(dst_config.as_path().join("chelix.toml").exists());
        assert!(dst_data.path().join("SOUL.md").exists());
        assert!(dst_data.path().join("IDENTITY.md").exists());
        assert!(!dst_data.path().join("sessions/t_1.jsonl").exists());

        // Verify content.
        let toml = std::fs::read_to_string(dst_config.as_path().join("chelix.toml")).unwrap();
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
        let mut config_guard = ConfigDirGuard::install(tempfile::tempdir().unwrap());
        let src_config = config_guard.path().to_path_buf();
        let src_data = tempfile::tempdir().unwrap();

        write_valid_config(&src_config, 8080);
        std::fs::write(src_data.path().join("SOUL.md"), "original soul").unwrap();

        // Export.
        let mut buf = Vec::new();
        let service = open_config_service().await;
        export_archive(
            &service,
            src_config.as_path(),
            src_data.path(),
            &ExportOptions::default(),
            &mut buf,
        )
        .await
        .unwrap();
        drop(service);

        // Create destination with pre-existing files.
        let dst_data = tempfile::tempdir().unwrap();
        config_guard.retarget(tempfile::tempdir().unwrap());
        let dst_config = config_guard.path().to_path_buf();
        write_valid_config(&dst_config, 9090);
        std::fs::write(dst_data.path().join("SOUL.md"), "modified soul").unwrap();
        let service = open_config_service().await;

        // Import with Skip strategy.
        let result = import_archive(
            &service,
            dst_config.as_path(),
            dst_data.path(),
            &ImportOptions {
                conflict: ConflictStrategy::Skip,
                dry_run: false,
            },
            Cursor::new(buf.clone()),
        )
        .await
        .unwrap()
        .result;

        // Existing files should NOT be overwritten.
        let toml = std::fs::read_to_string(dst_config.as_path().join("chelix.toml")).unwrap();
        assert_eq!(toml, valid_config_toml(9090));
        assert!(!result.skipped.is_empty());
    }

    #[tokio::test]
    async fn import_overwrite_existing() {
        let mut config_guard = ConfigDirGuard::install(tempfile::tempdir().unwrap());
        let src_config = config_guard.path().to_path_buf();
        let src_data = tempfile::tempdir().unwrap();

        write_valid_config(&src_config, 8080);

        let mut buf = Vec::new();
        let service = open_config_service().await;
        export_archive(
            &service,
            src_config.as_path(),
            src_data.path(),
            &ExportOptions::default(),
            &mut buf,
        )
        .await
        .unwrap();
        drop(service);

        let dst_data = tempfile::tempdir().unwrap();
        config_guard.retarget(tempfile::tempdir().unwrap());
        let dst_config = config_guard.path().to_path_buf();
        write_valid_config(&dst_config, 9090);
        let service = open_config_service().await;

        let result = import_archive(
            &service,
            dst_config.as_path(),
            dst_data.path(),
            &ImportOptions {
                conflict: ConflictStrategy::Overwrite,
                dry_run: false,
            },
            Cursor::new(buf.clone()),
        )
        .await
        .unwrap()
        .result;

        // File should be overwritten.
        let toml = std::fs::read_to_string(dst_config.as_path().join("chelix.toml")).unwrap();
        assert_eq!(toml, valid_config_toml(8080));
        assert!(result.imported.iter().any(|i| i.action == "overwritten"));
    }

    #[tokio::test]
    async fn dry_run_does_not_write() {
        let mut config_guard = ConfigDirGuard::install(tempfile::tempdir().unwrap());
        let src_config = config_guard.path().to_path_buf();
        let src_data = tempfile::tempdir().unwrap();

        write_valid_config(&src_config, 8080);

        let mut buf = Vec::new();
        let service = open_config_service().await;
        export_archive(
            &service,
            src_config.as_path(),
            src_data.path(),
            &ExportOptions::default(),
            &mut buf,
        )
        .await
        .unwrap();
        drop(service);

        let dst_data = tempfile::tempdir().unwrap();
        config_guard.retarget(tempfile::tempdir().unwrap());
        let dst_config = config_guard.path().to_path_buf();
        write_valid_config(&dst_config, 9090);
        let service = open_config_service().await;

        let result = import_archive(
            &service,
            dst_config.as_path(),
            dst_data.path(),
            &ImportOptions {
                conflict: ConflictStrategy::Skip,
                dry_run: true,
            },
            Cursor::new(buf.clone()),
        )
        .await
        .unwrap()
        .result;

        // Nothing should be written.
        let toml = std::fs::read_to_string(dst_config.as_path().join("chelix.toml")).unwrap();
        assert_eq!(toml, valid_config_toml(9090));
        assert!(result.imported.is_empty());
        assert!(!result.skipped.is_empty());
    }

    fn archive_with_config_then_manifest(manifest: Option<&[u8]>) -> Vec<u8> {
        let toml = valid_config_toml(7070);
        let toml = toml.as_bytes();
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
                    Cursor::new(toml),
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
        let dst_data = tempfile::tempdir().unwrap();
        let config_guard = ConfigDirGuard::install(tempfile::tempdir().unwrap());
        let dst_config = config_guard.path().to_path_buf();
        write_valid_config(&dst_config, 9090);
        let service = open_config_service().await;
        let error = import_archive(
            &service,
            dst_config.as_path(),
            dst_data.path(),
            &ImportOptions::default(),
            Cursor::new(archive),
        )
        .await
        .unwrap_err();
        assert!(error.to_string().contains("older than supported version"));
        assert_eq!(
            std::fs::read_to_string(dst_config.join("chelix.toml")).unwrap(),
            valid_config_toml(9090)
        );
    }

    #[tokio::test]
    async fn archive_without_manifest_is_rejected_before_any_file_is_written() {
        let archive = archive_with_config_then_manifest(None);
        let dst_data = tempfile::tempdir().unwrap();
        let config_guard = ConfigDirGuard::install(tempfile::tempdir().unwrap());
        let dst_config = config_guard.path().to_path_buf();
        write_valid_config(&dst_config, 9090);
        let service = open_config_service().await;
        let error = import_archive(
            &service,
            dst_config.as_path(),
            dst_data.path(),
            &ImportOptions::default(),
            Cursor::new(archive),
        )
        .await
        .unwrap_err();
        assert!(error.to_string().contains("missing manifest.json"));
        assert_eq!(
            std::fs::read_to_string(dst_config.join("chelix.toml")).unwrap(),
            valid_config_toml(9090)
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
        let config_guard = ConfigDirGuard::install(tempfile::tempdir().unwrap());
        let dst_config = config_guard.path().to_path_buf();
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

        write_valid_config(&dst_config, 9090);
        let service = open_config_service().await;
        let skipped = import_archive(
            &service,
            dst_config.as_path(),
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
            &service,
            dst_config.as_path(),
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
        let config_guard = ConfigDirGuard::install(tempfile::tempdir().unwrap());
        let src_config = config_guard.path().to_path_buf();
        write_valid_config(&src_config, 8080);
        let service = open_config_service().await;
        let mut archive = Vec::new();
        export_archive(
            &service,
            src_config.as_path(),
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
        let mut config_guard = ConfigDirGuard::install(tempfile::tempdir().unwrap());
        let src_config = config_guard.path().to_path_buf();
        write_valid_config(&src_config, 8080);
        let service = open_config_service().await;
        let mut archive = Vec::new();
        export_archive(
            &service,
            src_config.as_path(),
            &src_data,
            &ExportOptions::default(),
            &mut archive,
        )
        .await
        .unwrap();
        drop(service);

        let dst_parent = tempfile::tempdir().unwrap();
        let dst_data = dst_parent.path().join("o'quote");
        std::fs::create_dir_all(&dst_data).unwrap();
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

        config_guard.retarget(tempfile::tempdir().unwrap());
        let dst_config = config_guard.path().to_path_buf();
        write_valid_config(&dst_config, 9090);
        let service = open_config_service().await;
        let imported = import_archive(
            &service,
            dst_config.as_path(),
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
