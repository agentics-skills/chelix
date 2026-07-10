#![allow(clippy::unwrap_used, clippy::expect_used)]

#[cfg(feature = "wasm")]
mod wasm_tests {
    use crate::sandbox::wasm::WasmSandbox;

    use super::super::*;

    fn test_config() -> SandboxConfig {
        SandboxConfig {
            home_persistence: HomePersistence::Off,
            ..Default::default()
        }
    }

    #[test]
    fn test_wasm_sandbox_backend_name() {
        let sandbox = WasmSandbox::new(test_config()).unwrap();
        assert_eq!(sandbox.backend_name(), "wasm");
    }

    #[test]
    fn test_wasm_sandbox_is_real() {
        let sandbox = WasmSandbox::new(test_config()).unwrap();
        assert!(sandbox.is_real());
    }

    #[test]
    fn test_wasm_sandbox_fuel_limit_default() {
        let sandbox = WasmSandbox::new(test_config()).unwrap();
        assert_eq!(sandbox.fuel_limit(), 1_000_000_000);
    }

    #[test]
    fn test_wasm_sandbox_fuel_limit_custom() {
        let mut config = test_config();
        config.wasm_fuel_limit = Some(500_000);
        let sandbox = WasmSandbox::new(config).unwrap();
        assert_eq!(sandbox.fuel_limit(), 500_000);
    }

    #[test]
    fn test_wasm_sandbox_epoch_interval_default() {
        let sandbox = WasmSandbox::new(test_config()).unwrap();
        assert_eq!(sandbox.epoch_interval_ms(), 100);
    }

    #[tokio::test]
    async fn test_wasm_sandbox_ensure_ready_creates_dirs() {
        let sandbox = WasmSandbox::new(test_config()).unwrap();
        let id = SandboxId {
            scope: SandboxScope::Session,
            key: "test-wasm-ready".into(),
        };
        sandbox.ensure_ready(&id, None).await.unwrap();
        assert!(sandbox.home_dir(&id).unwrap().exists());
        assert!(sandbox.tmp_dir(&id).exists());
        // Cleanup.
        sandbox.cleanup(&id).await.unwrap();
    }

    #[tokio::test]
    async fn test_wasm_sandbox_cleanup_removes_dirs() {
        let sandbox = WasmSandbox::new(test_config()).unwrap();
        let id = SandboxId {
            scope: SandboxScope::Session,
            key: "test-wasm-cleanup".into(),
        };
        sandbox.ensure_ready(&id, None).await.unwrap();
        let root = sandbox.sandbox_root(&id);
        assert!(root.exists());
        sandbox.cleanup(&id).await.unwrap();
        assert!(!root.exists());
    }

    #[tokio::test]
    async fn test_wasm_sandbox_builtin_echo() {
        let sandbox = WasmSandbox::new(test_config()).unwrap();
        let id = SandboxId {
            scope: SandboxScope::Session,
            key: "test-wasm-echo".into(),
        };
        sandbox.ensure_ready(&id, None).await.unwrap();
        let result = sandbox
            .run_command(&id, "echo hello world", &CommandOptions::default())
            .await
            .unwrap();
        assert_eq!(result.exit_code, 0);
        assert_eq!(result.stdout.trim(), "hello world");
        sandbox.cleanup(&id).await.unwrap();
    }

    #[tokio::test]
    async fn test_wasm_sandbox_builtin_echo_no_newline() {
        let sandbox = WasmSandbox::new(test_config()).unwrap();
        let id = SandboxId {
            scope: SandboxScope::Session,
            key: "test-wasm-echo-n".into(),
        };
        sandbox.ensure_ready(&id, None).await.unwrap();
        let result = sandbox
            .run_command(&id, "echo -n hello", &CommandOptions::default())
            .await
            .unwrap();
        assert_eq!(result.exit_code, 0);
        assert_eq!(result.stdout, "hello");
        sandbox.cleanup(&id).await.unwrap();
    }

    #[tokio::test]
    async fn test_wasm_sandbox_builtin_pwd() {
        let sandbox = WasmSandbox::new(test_config()).unwrap();
        let id = SandboxId {
            scope: SandboxScope::Session,
            key: "test-wasm-pwd".into(),
        };
        sandbox.ensure_ready(&id, None).await.unwrap();
        let result = sandbox
            .run_command(&id, "pwd", &CommandOptions::default())
            .await
            .unwrap();
        assert_eq!(result.exit_code, 0);
        assert_eq!(result.stdout.trim(), "/home/sandbox");
        sandbox.cleanup(&id).await.unwrap();
    }

    #[tokio::test]
    async fn test_wasm_sandbox_builtin_true_false() {
        let sandbox = WasmSandbox::new(test_config()).unwrap();
        let id = SandboxId {
            scope: SandboxScope::Session,
            key: "test-wasm-tf".into(),
        };
        sandbox.ensure_ready(&id, None).await.unwrap();

        let result = sandbox
            .run_command(&id, "true", &CommandOptions::default())
            .await
            .unwrap();
        assert_eq!(result.exit_code, 0);

        let result = sandbox
            .run_command(&id, "false", &CommandOptions::default())
            .await
            .unwrap();
        assert_eq!(result.exit_code, 1);
        sandbox.cleanup(&id).await.unwrap();
    }

    #[tokio::test]
    async fn test_wasm_sandbox_builtin_mkdir_ls() {
        let sandbox = WasmSandbox::new(test_config()).unwrap();
        let id = SandboxId {
            scope: SandboxScope::Session,
            key: "test-wasm-mkdir-ls".into(),
        };
        sandbox.ensure_ready(&id, None).await.unwrap();

        let result = sandbox
            .run_command(
                &id,
                "mkdir /home/sandbox/testdir",
                &CommandOptions::default(),
            )
            .await
            .unwrap();
        assert_eq!(result.exit_code, 0);

        let result = sandbox
            .run_command(&id, "ls /home/sandbox", &CommandOptions::default())
            .await
            .unwrap();
        assert_eq!(result.exit_code, 0);
        assert!(result.stdout.contains("testdir"));
        sandbox.cleanup(&id).await.unwrap();
    }

    #[tokio::test]
    async fn test_wasm_sandbox_builtin_touch_cat() {
        let sandbox = WasmSandbox::new(test_config()).unwrap();
        let id = SandboxId {
            scope: SandboxScope::Session,
            key: "test-wasm-touch-cat".into(),
        };
        sandbox.ensure_ready(&id, None).await.unwrap();

        // Write a file using echo with redirect.
        let result = sandbox
            .run_command(
                &id,
                "echo hello > /home/sandbox/test.txt",
                &CommandOptions::default(),
            )
            .await
            .unwrap();
        assert_eq!(result.exit_code, 0);

        // Read it back.
        let result = sandbox
            .run_command(
                &id,
                "cat /home/sandbox/test.txt",
                &CommandOptions::default(),
            )
            .await
            .unwrap();
        assert_eq!(result.exit_code, 0);
        assert_eq!(result.stdout.trim(), "hello");
        sandbox.cleanup(&id).await.unwrap();
    }

    #[tokio::test]
    async fn test_wasm_sandbox_builtin_rm() {
        let sandbox = WasmSandbox::new(test_config()).unwrap();
        let id = SandboxId {
            scope: SandboxScope::Session,
            key: "test-wasm-rm".into(),
        };
        sandbox.ensure_ready(&id, None).await.unwrap();

        sandbox
            .run_command(
                &id,
                "echo data > /home/sandbox/to_delete.txt",
                &CommandOptions::default(),
            )
            .await
            .unwrap();

        let result = sandbox
            .run_command(
                &id,
                "rm /home/sandbox/to_delete.txt",
                &CommandOptions::default(),
            )
            .await
            .unwrap();
        assert_eq!(result.exit_code, 0);

        let result = sandbox
            .run_command(
                &id,
                "cat /home/sandbox/to_delete.txt",
                &CommandOptions::default(),
            )
            .await
            .unwrap();
        assert_eq!(result.exit_code, 1);
        sandbox.cleanup(&id).await.unwrap();
    }

    #[tokio::test]
    async fn test_wasm_sandbox_unknown_command_127() {
        let sandbox = WasmSandbox::new(test_config()).unwrap();
        let id = SandboxId {
            scope: SandboxScope::Session,
            key: "test-wasm-unknown".into(),
        };
        sandbox.ensure_ready(&id, None).await.unwrap();
        let result = sandbox
            .run_command(&id, "nonexistent_cmd", &CommandOptions::default())
            .await
            .unwrap();
        assert_eq!(result.exit_code, 127);
        assert!(result.stderr.contains("command not found"));
        sandbox.cleanup(&id).await.unwrap();
    }

    #[tokio::test]
    async fn test_wasm_sandbox_path_escape_blocked() {
        let sandbox = WasmSandbox::new(test_config()).unwrap();
        let id = SandboxId {
            scope: SandboxScope::Session,
            key: "test-wasm-escape".into(),
        };
        sandbox.ensure_ready(&id, None).await.unwrap();

        // Try to cat a file outside sandbox.
        let result = sandbox
            .run_command(&id, "cat /etc/passwd", &CommandOptions::default())
            .await
            .unwrap();
        assert_eq!(result.exit_code, 1);
        assert!(result.stderr.contains("outside sandbox"));
        sandbox.cleanup(&id).await.unwrap();
    }

    #[tokio::test]
    async fn test_wasm_sandbox_and_connector() {
        let sandbox = WasmSandbox::new(test_config()).unwrap();
        let id = SandboxId {
            scope: SandboxScope::Session,
            key: "test-wasm-and".into(),
        };
        sandbox.ensure_ready(&id, None).await.unwrap();
        let result = sandbox
            .run_command(&id, "true && echo yes", &CommandOptions::default())
            .await
            .unwrap();
        assert_eq!(result.exit_code, 0);
        assert_eq!(result.stdout.trim(), "yes");

        let result = sandbox
            .run_command(&id, "false && echo no", &CommandOptions::default())
            .await
            .unwrap();
        // The echo shouldn't run, so stdout should be empty.
        assert!(result.stdout.is_empty());
        sandbox.cleanup(&id).await.unwrap();
    }

    #[tokio::test]
    async fn test_wasm_sandbox_or_connector() {
        let sandbox = WasmSandbox::new(test_config()).unwrap();
        let id = SandboxId {
            scope: SandboxScope::Session,
            key: "test-wasm-or".into(),
        };
        sandbox.ensure_ready(&id, None).await.unwrap();
        let result = sandbox
            .run_command(&id, "false || echo fallback", &CommandOptions::default())
            .await
            .unwrap();
        assert_eq!(result.exit_code, 0);
        assert_eq!(result.stdout.trim(), "fallback");
        sandbox.cleanup(&id).await.unwrap();
    }

    #[tokio::test]
    async fn test_wasm_sandbox_builtin_test_file() {
        let sandbox = WasmSandbox::new(test_config()).unwrap();
        let id = SandboxId {
            scope: SandboxScope::Session,
            key: "test-wasm-testcmd".into(),
        };
        sandbox.ensure_ready(&id, None).await.unwrap();

        sandbox
            .run_command(
                &id,
                "echo x > /home/sandbox/exists.txt",
                &CommandOptions::default(),
            )
            .await
            .unwrap();

        let result = sandbox
            .run_command(
                &id,
                "test -f /home/sandbox/exists.txt",
                &CommandOptions::default(),
            )
            .await
            .unwrap();
        assert_eq!(result.exit_code, 0);

        let result = sandbox
            .run_command(
                &id,
                "test -f /home/sandbox/nope.txt",
                &CommandOptions::default(),
            )
            .await
            .unwrap();
        assert_eq!(result.exit_code, 1);
        sandbox.cleanup(&id).await.unwrap();
    }

    #[tokio::test]
    async fn test_wasm_sandbox_builtin_basename_dirname() {
        let sandbox = WasmSandbox::new(test_config()).unwrap();
        let id = SandboxId {
            scope: SandboxScope::Session,
            key: "test-wasm-pathops".into(),
        };
        sandbox.ensure_ready(&id, None).await.unwrap();

        let result = sandbox
            .run_command(
                &id,
                "basename /home/sandbox/foo/bar.txt",
                &CommandOptions::default(),
            )
            .await
            .unwrap();
        assert_eq!(result.stdout.trim(), "bar.txt");

        let result = sandbox
            .run_command(
                &id,
                "dirname /home/sandbox/foo/bar.txt",
                &CommandOptions::default(),
            )
            .await
            .unwrap();
        assert_eq!(result.stdout.trim(), "/home/sandbox/foo");
        sandbox.cleanup(&id).await.unwrap();
    }

    #[tokio::test]
    async fn test_wasm_sandbox_builtin_which() {
        let sandbox = WasmSandbox::new(test_config()).unwrap();
        let id = SandboxId {
            scope: SandboxScope::Session,
            key: "test-wasm-which".into(),
        };
        sandbox.ensure_ready(&id, None).await.unwrap();

        let result = sandbox
            .run_command(&id, "which echo", &CommandOptions::default())
            .await
            .unwrap();
        assert_eq!(result.exit_code, 0);
        assert!(result.stdout.contains("built-in"));

        let result = sandbox
            .run_command(&id, "which nonexistent", &CommandOptions::default())
            .await
            .unwrap();
        assert_eq!(result.exit_code, 1);
        sandbox.cleanup(&id).await.unwrap();
    }

    #[test]
    fn test_wasm_sandbox_maps_data_dir_at_identical_guest_path() {
        let data_dir = chelix_config::data_dir();
        let sandbox = WasmSandbox::new(test_config()).unwrap();
        let id = SandboxId {
            scope: SandboxScope::Session,
            key: "test-wasm-data-mount".into(),
        };
        let mounts = sandbox.runtime_mounts(&id).unwrap();
        let data_mount = mounts
            .iter()
            .find(|mount| mount.guest == data_dir)
            .expect("data_dir mount");

        assert_eq!(data_mount.host, data_dir);
        assert_eq!(
            data_mount.mode,
            chelix_config::container_mounts::MountMode::Rw
        );
    }

    #[test]
    fn test_wasm_sandbox_does_not_shadow_declarative_tmp_mount() {
        let host = tempfile::tempdir().unwrap();
        let sandbox = WasmSandbox::new(SandboxConfig {
            home_persistence: HomePersistence::Off,
            mounts: vec![chelix_config::container_mounts::SandboxMount {
                host: host.path().to_path_buf(),
                guest: "/tmp".into(),
                mode: chelix_config::container_mounts::MountMode::Ro,
            }],
            ..Default::default()
        })
        .unwrap();
        let id = SandboxId {
            scope: SandboxScope::Session,
            key: "test-wasm-custom-tmp".into(),
        };
        let mounts = sandbox.runtime_mounts(&id).unwrap();
        let tmp_mounts: Vec<_> = mounts
            .iter()
            .filter(|mount| mount.guest == std::path::Path::new("/tmp"))
            .collect();

        assert_eq!(tmp_mounts.len(), 1);
        assert_eq!(tmp_mounts[0].host, host.path());
        assert_eq!(
            tmp_mounts[0].mode,
            chelix_config::container_mounts::MountMode::Ro
        );
    }

    #[tokio::test]
    async fn test_wasm_sandbox_custom_read_only_mount_blocks_writes() {
        let host = tempfile::tempdir().unwrap();
        std::fs::write(host.path().join("visible.txt"), "read only").unwrap();
        let sandbox = WasmSandbox::new(SandboxConfig {
            home_persistence: HomePersistence::Off,
            mounts: vec![chelix_config::container_mounts::SandboxMount {
                host: host.path().to_path_buf(),
                guest: "/mnt/custom".into(),
                mode: chelix_config::container_mounts::MountMode::Ro,
            }],
            ..Default::default()
        })
        .unwrap();
        let id = SandboxId {
            scope: SandboxScope::Session,
            key: "test-wasm-ro-mount".into(),
        };
        sandbox.ensure_ready(&id, None).await.unwrap();

        let read = sandbox
            .run_command(
                &id,
                "cat /mnt/custom/visible.txt",
                &CommandOptions::default(),
            )
            .await
            .unwrap();
        assert_eq!(read.exit_code, 0);
        assert_eq!(read.stdout, "read only");

        let write = sandbox
            .run_command(
                &id,
                "echo changed > /mnt/custom/new.txt",
                &CommandOptions::default(),
            )
            .await
            .unwrap();
        assert_eq!(write.exit_code, 1);
        assert!(!host.path().join("new.txt").exists());
        sandbox.cleanup(&id).await.unwrap();
    }

    #[tokio::test]
    async fn test_wasm_sandbox_custom_rw_mount_allows_writes() {
        let host = tempfile::tempdir().unwrap();
        let sandbox = WasmSandbox::new(SandboxConfig {
            home_persistence: HomePersistence::Off,
            mounts: vec![chelix_config::container_mounts::SandboxMount {
                host: host.path().to_path_buf(),
                guest: "/mnt/custom".into(),
                mode: chelix_config::container_mounts::MountMode::Rw,
            }],
            ..Default::default()
        })
        .unwrap();
        let id = SandboxId {
            scope: SandboxScope::Session,
            key: "test-wasm-rw-mount".into(),
        };
        sandbox.ensure_ready(&id, None).await.unwrap();

        let write = sandbox
            .run_command(
                &id,
                "echo changed > /mnt/custom/new.txt",
                &CommandOptions::default(),
            )
            .await
            .unwrap();
        assert_eq!(write.exit_code, 0);
        assert_eq!(
            std::fs::read_to_string(host.path().join("new.txt")).unwrap(),
            "changed\n"
        );
        sandbox.cleanup(&id).await.unwrap();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn test_wasm_sandbox_mount_rejects_symlink_escape() {
        let host = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("secret.txt"), "not visible").unwrap();
        std::os::unix::fs::symlink(outside.path(), host.path().join("escape")).unwrap();
        let sandbox = WasmSandbox::new(SandboxConfig {
            home_persistence: HomePersistence::Off,
            mounts: vec![chelix_config::container_mounts::SandboxMount {
                host: host.path().to_path_buf(),
                guest: "/mnt/custom".into(),
                mode: chelix_config::container_mounts::MountMode::Rw,
            }],
            ..Default::default()
        })
        .unwrap();
        let id = SandboxId {
            scope: SandboxScope::Session,
            key: "test-wasm-symlink-escape".into(),
        };
        sandbox.ensure_ready(&id, None).await.unwrap();

        let read = sandbox
            .run_command(
                &id,
                "cat /mnt/custom/escape/secret.txt",
                &CommandOptions::default(),
            )
            .await
            .unwrap();
        assert_eq!(read.exit_code, 1);
        assert!(!read.stdout.contains("not visible"));
        sandbox.cleanup(&id).await.unwrap();
    }

    #[tokio::test]
    async fn test_wasm_sandbox_build_image_returns_none() {
        let sandbox = WasmSandbox::new(test_config()).unwrap();
        let result = sandbox
            .build_image("ubuntu:latest", &["curl".to_string()])
            .await
            .unwrap();
        assert!(result.is_none());
    }
}
