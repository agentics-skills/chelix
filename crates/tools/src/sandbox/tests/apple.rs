#![allow(clippy::unwrap_used, clippy::expect_used)]
use super::*;

#[cfg(target_os = "macos")]
#[test]
fn test_backend_id_apple_container() {
    let sandbox = AppleContainerSandbox::new(SandboxConfig::default());
    assert_eq!(sandbox.backend_id(), SandboxBackendId::AppleContainer);
}

#[cfg(target_os = "macos")]
#[test]
fn test_sandbox_router_explicit_apple_container_backend() {
    let config = SandboxConfig {
        backend: SandboxBackend::AppleContainer,
        ..Default::default()
    };
    let backend: Arc<dyn Sandbox> = Arc::new(TestSandbox::new(
        SandboxBackendId::AppleContainer,
        None,
        None,
    ));
    let router = SandboxRouter::with_backend(config, backend);
    assert_eq!(router.backend_id(), SandboxBackendId::AppleContainer);
}

#[cfg(target_os = "macos")]
#[tokio::test]
async fn test_apple_container_name_generation_rotation() {
    let sandbox = AppleContainerSandbox::new(SandboxConfig::default());
    let id = SandboxId {
        scope: SandboxScope::Session,
        key: "session-abc".into(),
    };

    let first_name = sandbox.container_name(&id).await;
    assert_eq!(first_name, "chelix-sandbox-session-abc");

    let rotated_name = sandbox.bump_container_generation(&id).await;
    assert_eq!(rotated_name, "chelix-sandbox-session-abc-g1");

    let current_name = sandbox.container_name(&id).await;
    assert_eq!(current_name, "chelix-sandbox-session-abc-g1");
}

/// When both Docker and Apple Container are available, test that we can
/// explicitly select each one.
#[test]
fn test_select_backend_explicit_choices() {
    // Docker backend
    if should_use_docker_backend(is_cli_available("docker"), is_docker_daemon_available()) {
        let config = SandboxConfig {
            backend: SandboxBackend::Docker,
            ..Default::default()
        };
        let backend = select_backend(config).unwrap();
        assert_eq!(backend.backend_id(), SandboxBackendId::Docker);
    }

    // Podman backend
    if is_cli_available("podman") {
        let config = SandboxConfig {
            backend: SandboxBackend::Podman,
            ..Default::default()
        };
        let backend = select_backend(config).unwrap();
        assert_eq!(backend.backend_id(), SandboxBackendId::Podman);
    }

    // Apple Container backend (macOS only)
    #[cfg(target_os = "macos")]
    if is_cli_available("container") && ensure_apple_container_service() {
        let config = SandboxConfig {
            backend: SandboxBackend::AppleContainer,
            ..Default::default()
        };
        let backend = select_backend(config).unwrap();
        assert_eq!(backend.backend_id(), SandboxBackendId::AppleContainer);
    }
}

#[tokio::test]
async fn test_runtime_oci_file_transfers_with_docker() {
    if !runtime_container_e2e_enabled("docker") {
        eprintln!(
            "skipping Docker OCI runtime e2e test, set {}=1 and ensure docker is available",
            OCI_RUNTIME_E2E_ENV
        );
        return;
    }

    assert_runtime_oci_file_transfers("docker").await.unwrap();
}

#[tokio::test]
async fn test_runtime_oci_file_transfers_with_podman() {
    if !runtime_container_e2e_enabled("podman") {
        eprintln!(
            "skipping Podman OCI runtime e2e test, set {}=1 and ensure podman is available",
            OCI_RUNTIME_E2E_ENV
        );
        return;
    }

    assert_runtime_oci_file_transfers("podman").await.unwrap();
}

#[test]
fn test_is_apple_container_service_error() {
    assert!(is_apple_container_service_error(
        "Error: internalError: \"XPC connection error\""
    ));
    assert!(is_apple_container_service_error(
        "Error: Connection invalid while contacting service"
    ));
    assert!(!is_apple_container_service_error(
        "Error: something else happened"
    ));
}

#[test]
fn test_is_apple_container_exists_error() {
    assert!(is_apple_container_exists_error(
        "Error: exists: \"container with id chelix-sandbox-main already exists\""
    ));
    assert!(is_apple_container_exists_error(
        "Error: container already exists"
    ));
    assert!(!is_apple_container_exists_error("Error: no such container"));
}

#[test]
fn test_apple_container_run_args_launch_tools_service() {
    let args = apple_container_run_args(
        "chelix-sandbox-test",
        "ubuntu:26.04",
        Some("UTC"),
        &[],
        "test-token",
        43123,
    );
    let expected = vec![
        "run",
        "-d",
        "--name",
        "chelix-sandbox-test",
        "--workdir",
        "/tmp",
        "-e",
        "TZ=UTC",
        "-e",
        "CHELIX_TOOLS_SERVICE_TOKEN=test-token",
        "-p",
        "127.0.0.1:43123:43271",
        "ubuntu:26.04",
        "chelix-tools-service",
        "--listen",
        "0.0.0.0:43271",
    ]
    .into_iter()
    .map(str::to_string)
    .collect::<Vec<_>>();
    assert_eq!(args, expected);
}

#[test]
fn test_apple_container_run_args_with_declarative_mounts() {
    let args = apple_container_run_args(
        "chelix-sandbox-test",
        "ubuntu:26.04",
        Some("UTC"),
        &[
            "source=/tmp/data,target=/tmp/data".to_string(),
            "source=/tmp/home,target=/home/sandbox,readonly".to_string(),
        ],
        "test-token",
        43123,
    );
    let expected = vec![
        "run",
        "-d",
        "--name",
        "chelix-sandbox-test",
        "--workdir",
        "/tmp",
        "-e",
        "TZ=UTC",
        "-e",
        "CHELIX_TOOLS_SERVICE_TOKEN=test-token",
        "-p",
        "127.0.0.1:43123:43271",
        "--mount",
        "source=/tmp/data,target=/tmp/data",
        "--mount",
        "source=/tmp/home,target=/home/sandbox,readonly",
        "ubuntu:26.04",
        "chelix-tools-service",
        "--listen",
        "0.0.0.0:43271",
    ]
    .into_iter()
    .map(str::to_string)
    .collect::<Vec<_>>();
    assert_eq!(args, expected);
}

#[cfg(target_os = "macos")]
#[test]
fn test_apple_container_mount_specs_use_resolved_plan_and_modes() {
    let host_data = tempfile::tempdir().unwrap();
    let custom = tempfile::tempdir().unwrap();
    let custom_file = tempfile::NamedTempFile::new().unwrap();
    let sandbox = AppleContainerSandbox::new(SandboxConfig {
        host_data_dir: Some(host_data.path().to_path_buf()),
        mounts: vec![
            chelix_config::container_mounts::SandboxMount {
                host: custom.path().to_path_buf(),
                guest: "/mnt/reference".into(),
                mode: chelix_config::container_mounts::MountMode::Ro,
            },
            chelix_config::container_mounts::SandboxMount {
                host: custom_file.path().to_path_buf(),
                guest: "/mnt/single-file".into(),
                mode: chelix_config::container_mounts::MountMode::Ro,
            },
        ],
        ..Default::default()
    });
    let id = SandboxId {
        scope: SandboxScope::Session,
        key: "apple-mount-plan".into(),
    };

    let resolved_plan =
        crate::sandbox::paths::resolved_sandbox_mount_plan(&sandbox.config, Some("container"), &id)
            .unwrap();
    assert!(
        resolved_plan
            .iter()
            .any(|mount| mount.host == custom_file.path())
    );

    let specs = sandbox.mount_specs(&id).unwrap();
    assert!(specs.contains(&format!(
        "source={},target={}",
        host_data.path().display(),
        chelix_config::data_dir().display()
    )));
    assert!(specs.contains(&format!(
        "source={},target=/home/sandbox",
        host_data.path().join("sandbox/home/shared").display()
    )));
    assert!(specs.contains(&format!(
        "source={},target=/mnt/reference,readonly",
        custom.path().display()
    )));
    assert!(
        specs
            .iter()
            .all(|spec| !spec.contains(&custom_file.path().display().to_string()))
    );
    assert!(
        specs
            .iter()
            .all(|spec| !spec.contains("credentials.json") && !spec.contains("provider_keys.json"))
    );
}

#[test]
fn test_apple_container_exec_args_pin_workdir_and_bootstrap_home() {
    let args = apple_container_exec_args("chelix-sandbox-test", "true".to_string());
    let expected = vec![
        "exec",
        "--workdir",
        "/tmp",
        "chelix-sandbox-test",
        "bash",
        "-c",
        "mkdir -p /home/sandbox && true",
    ]
    .into_iter()
    .map(str::to_string)
    .collect::<Vec<_>>();
    assert_eq!(args, expected);
}

#[test]
fn test_container_exec_shell_args_apple_container_uses_safe_wrapper() {
    let args = container_exec_shell_args("container", "chelix-sandbox-test", "echo hi".into());
    let expected = vec![
        "exec",
        "--workdir",
        "/tmp",
        "chelix-sandbox-test",
        "bash",
        "-c",
        "mkdir -p /home/sandbox && echo hi",
    ]
    .into_iter()
    .map(str::to_string)
    .collect::<Vec<_>>();
    assert_eq!(args, expected);
}

#[test]
fn test_container_exec_shell_args_docker_keeps_standard_exec_shape() {
    let args = container_exec_shell_args("docker", "chelix-sandbox-test", "echo hi".into());
    let expected = vec!["exec", "chelix-sandbox-test", "bash", "-c", "echo hi"]
        .into_iter()
        .map(str::to_string)
        .collect::<Vec<_>>();
    assert_eq!(args, expected);
}

#[test]
fn test_apple_container_status_from_inspect() {
    assert_eq!(
        apple_container_status_from_inspect(
            r#"[{"id":"abc","status":"running","configuration":{}}]"#
        ),
        Some("running")
    );
    assert_eq!(
        apple_container_status_from_inspect(r#"[{"id":"abc","status":"stopped"}]"#),
        Some("stopped")
    );
    assert_eq!(apple_container_status_from_inspect("[]"), None);
    assert_eq!(apple_container_status_from_inspect(""), None);
}

#[test]
fn test_is_apple_container_daemon_stale_error() {
    // Full EINVAL pattern from container logs
    assert!(is_apple_container_daemon_stale_error(
        "Error: internalError: \" Error Domain=NSPOSIXErrorDomain Code=22 \"Invalid argument\"\""
    ));
    // Both patterns required — neither alone should match
    assert!(!is_apple_container_daemon_stale_error(
        "NSPOSIXErrorDomain Code=22"
    ));
    assert!(!is_apple_container_daemon_stale_error("Invalid argument"));
    // Log-fetching errors with NSPOSIXErrorDomain Code=2 must NOT match
    assert!(!is_apple_container_daemon_stale_error(
        "Error Domain=NSPOSIXErrorDomain Code=2 \"No such file or directory\""
    ));
    assert!(!is_apple_container_daemon_stale_error(
        "container is not running"
    ));
    assert!(!is_apple_container_daemon_stale_error("permission denied"));
}

#[cfg(target_os = "macos")]
#[test]
fn test_is_apple_container_boot_failure() {
    // No logs at all — VM never booted
    assert!(is_apple_container_boot_failure(None));
    // Empty logs
    assert!(is_apple_container_boot_failure(Some("")));
    assert!(is_apple_container_boot_failure(Some("  \n  ")));
    // stdio.log doesn't exist — VM never produced output
    assert!(is_apple_container_boot_failure(Some(
        r#"Error: invalidArgument: "failed to fetch container logs: internalError: "failed to open container logs: Error Domain=NSCocoaErrorDomain Code=4 "The file "stdio.log" doesn't exist."""#
    )));
    // Real logs present — not a boot failure
    assert!(!is_apple_container_boot_failure(Some(
        "sleep: invalid time interval 'infinity'"
    )));
    // Daemon-stale EINVAL is NOT a boot failure (different handler)
    assert!(!is_apple_container_boot_failure(Some(
        "Error Domain=NSPOSIXErrorDomain Code=22 \"Invalid argument\""
    )));
}

#[test]
fn test_is_apple_container_corruption_error() {
    assert!(is_apple_container_corruption_error(
        "failed to bootstrap container because config.json is missing"
    ));
    // Daemon-stale errors should also trigger corruption/failover
    assert!(is_apple_container_corruption_error(
        "Error: internalError: \" Error Domain=NSPOSIXErrorDomain Code=22 \"Invalid argument\"\""
    ));
    assert!(!is_apple_container_corruption_error(
        "cannot exec: container is not running"
    ));
    assert!(!is_apple_container_corruption_error(
        "invalidState: \"no sandbox client exists: container is stopped\""
    ));
    assert!(!is_apple_container_corruption_error("permission denied"));
    // Boot failure "VM never booted" should trigger corruption/failover
    assert!(is_apple_container_corruption_error(
        "apple container test did not become exec-ready (VM never booted): timeout"
    ));
}

#[tokio::test]
async fn test_failover_sandbox_switches_from_apple_to_docker() {
    let primary = Arc::new(TestSandbox::new(
        SandboxBackendId::AppleContainer,
        Some("failed to bootstrap container: config.json missing"),
        None,
    ));
    let fallback = Arc::new(TestSandbox::new(SandboxBackendId::Docker, None, None));
    let sandbox = FailoverSandbox::new(primary.clone(), fallback.clone()).unwrap();
    let id = SandboxId {
        scope: SandboxScope::Session,
        key: "session-abc".into(),
    };

    sandbox.ensure_ready(&id).await.unwrap();
    sandbox.ensure_ready(&id).await.unwrap();

    assert_eq!(primary.ensure_ready_calls(), 1);
    assert_eq!(fallback.ensure_ready_calls(), 2);
}

#[tokio::test]
async fn test_failover_sandbox_switches_on_boot_failure() {
    let primary = Arc::new(TestSandbox::new(
        SandboxBackendId::AppleContainer,
        Some("apple container test did not become exec-ready (VM never booted): timeout"),
        None,
    ));
    let fallback = Arc::new(TestSandbox::new(SandboxBackendId::Docker, None, None));
    let sandbox = FailoverSandbox::new(primary.clone(), fallback.clone()).unwrap();
    let id = SandboxId {
        scope: SandboxScope::Session,
        key: "session-boot".into(),
    };

    sandbox.ensure_ready(&id).await.unwrap();

    assert_eq!(primary.ensure_ready_calls(), 1);
    assert_eq!(fallback.ensure_ready_calls(), 1);
}

#[tokio::test]
async fn test_failover_sandbox_does_not_switch_on_unrelated_error() {
    let primary = Arc::new(TestSandbox::new(
        SandboxBackendId::AppleContainer,
        Some("permission denied"),
        None,
    ));
    let fallback = Arc::new(TestSandbox::new(SandboxBackendId::Docker, None, None));
    let sandbox = FailoverSandbox::new(primary.clone(), fallback.clone()).unwrap();
    let id = SandboxId {
        scope: SandboxScope::Session,
        key: "session-abc".into(),
    };

    let error = sandbox.ensure_ready(&id).await.unwrap_err();
    assert!(format!("{error:#}").contains("permission denied"));
    assert_eq!(primary.ensure_ready_calls(), 1);
    assert_eq!(fallback.ensure_ready_calls(), 0);
}

#[tokio::test]
async fn test_failover_sandbox_switches_command_path() {
    let primary = Arc::new(TestSandbox::new(
        SandboxBackendId::AppleContainer,
        None,
        Some("failed to bootstrap container: config.json missing"),
    ));
    let fallback = Arc::new(TestSandbox::new(SandboxBackendId::Docker, None, None));
    let sandbox = FailoverSandbox::new(primary.clone(), fallback.clone()).unwrap();
    let id = SandboxId {
        scope: SandboxScope::Session,
        key: "session-abc".into(),
    };

    let result = sandbox
        .run_command(&id, "uname -a", &CommandOptions::default())
        .await
        .unwrap();
    assert_eq!(result.exit_code, 0);
    assert_eq!(primary.command_calls(), 1);
    assert_eq!(fallback.ensure_ready_calls(), 1);
    assert_eq!(fallback.command_calls(), 1);
}

#[tokio::test]
async fn test_failover_sandbox_switches_on_daemon_stale_error() {
    let primary = Arc::new(TestSandbox::new(
        SandboxBackendId::AppleContainer,
        Some(
            "Error: internalError: \" Error Domain=NSPOSIXErrorDomain Code=22 \"Invalid argument\"\"",
        ),
        None,
    ));
    let fallback = Arc::new(TestSandbox::new(SandboxBackendId::Docker, None, None));
    let sandbox = FailoverSandbox::new(primary.clone(), fallback.clone()).unwrap();
    let id = SandboxId {
        scope: SandboxScope::Session,
        key: "session-abc".into(),
    };

    sandbox.ensure_ready(&id).await.unwrap();
    sandbox.ensure_ready(&id).await.unwrap();

    assert_eq!(primary.ensure_ready_calls(), 1);
    assert_eq!(fallback.ensure_ready_calls(), 2);
}

#[tokio::test]
async fn test_failover_sandbox_docker_to_podman() {
    let primary = Arc::new(TestSandbox::new(
        SandboxBackendId::Docker,
        Some("cannot connect to the docker daemon"),
        None,
    ));
    let fallback = Arc::new(TestSandbox::new(SandboxBackendId::Podman, None, None));
    let sandbox = FailoverSandbox::new(primary.clone(), fallback.clone()).unwrap();
    let id = SandboxId {
        scope: SandboxScope::Session,
        key: "session-docker-podman".into(),
    };

    sandbox.ensure_ready(&id).await.unwrap();

    assert_eq!(primary.ensure_ready_calls(), 1);
    assert_eq!(fallback.ensure_ready_calls(), 1);
}

#[tokio::test]
async fn test_failover_docker_does_not_switch_on_unrelated_error() {
    let primary = Arc::new(TestSandbox::new(
        SandboxBackendId::Docker,
        Some("image not found"),
        None,
    ));
    let fallback = Arc::new(TestSandbox::new(SandboxBackendId::Podman, None, None));
    let sandbox = FailoverSandbox::new(primary.clone(), fallback.clone()).unwrap();
    let id = SandboxId {
        scope: SandboxScope::Session,
        key: "session-docker-no-failover".into(),
    };

    let error = sandbox.ensure_ready(&id).await.unwrap_err();
    assert!(format!("{error:#}").contains("image not found"));
    assert_eq!(primary.ensure_ready_calls(), 1);
    assert_eq!(fallback.ensure_ready_calls(), 0);
}
