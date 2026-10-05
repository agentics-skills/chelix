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
    let router = SandboxRouter::with_backend(config, backend, test_owner_resolver()).unwrap();
    assert_eq!(router.backend_id(), SandboxBackendId::AppleContainer);
}

#[cfg(target_os = "macos")]
#[test]
fn test_apple_container_name_is_stable() {
    let sandbox = AppleContainerSandbox::new(SandboxConfig::default());
    let id = SandboxId {
        scope: SandboxScope::Session,
        key: "session-abc".into(),
    };

    assert_eq!(sandbox.container_name(&id), "chelix-sandbox-session-abc");
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

#[test]
fn test_apple_container_run_args_launch_tools_service() {
    let args = apple_container_run_args(
        "chelix-sandbox-test",
        "ubuntu:26.04",
        Some("UTC"),
        &[],
        "test-token",
        43123,
        115,
        58,
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
        "--working-dir",
        "/home/sandbox",
        "--terminal-cols",
        "115",
        "--terminal-rows",
        "58",
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
        115,
        58,
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
        "--working-dir",
        "/home/sandbox",
        "--terminal-cols",
        "115",
        "--terminal-rows",
        "58",
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
            .all(|spec| !spec.contains("credentials.json") && !spec.contains("chelix.toml"))
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
    let running = r#"[{
        "id": "abc",
        "configuration": {
            "initProcess": {"environment": ["CHELIX_TOOLS_SERVICE_TOKEN=test-token"]},
            "publishedPorts": [{
                "hostAddress": "127.0.0.1", "hostPort": 43123,
                "containerPort": 43271, "proto": "tcp", "count": 1
            }],
            "image": {"reference": "ubuntu:26.04"},
            "resources": {"cpus": 4, "memoryInBytes": 1073741824}
        },
        "status": {
            "state": "running",
            "networks": [],
            "startedDate": "2026-10-02T00:00:00Z"
        }
    }]"#;
    let parsed: Vec<crate::sandbox::containers::AppleManagedContainer> =
        serde_json::from_str(running).unwrap();
    assert_eq!(
        parsed[0].status.started_date.as_deref(),
        Some("2026-10-02T00:00:00Z")
    );
    assert_eq!(
        apple_container_status_from_inspect(running).unwrap(),
        Some(AppleContainerState::Running)
    );
    assert_eq!(
        apple_container_status_from_inspect(&running.replace("running", "stopped")).unwrap(),
        Some(AppleContainerState::Stopped)
    );
    assert_eq!(
        apple_container_status_from_inspect(&running.replace("running", "stopping")).unwrap(),
        Some(AppleContainerState::Stopping)
    );
    assert_eq!(
        apple_container_status_from_inspect(&running.replace("running", "unknown")).unwrap(),
        Some(AppleContainerState::Unknown)
    );
    assert_eq!(apple_container_status_from_inspect("[]").unwrap(), None);
    assert!(apple_container_status_from_inspect("").is_err());
    assert!(apple_container_status_from_inspect(r#"[{"id":"abc","status":"running"}]"#).is_err());
}
