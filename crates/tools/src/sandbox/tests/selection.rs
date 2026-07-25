#![allow(clippy::unwrap_used, clippy::expect_used)]

use super::*;

#[test]
fn explicit_unavailable_backend_fails_closed() {
    let backend = if is_cli_available("podman") {
        SandboxBackend::Docker
    } else {
        SandboxBackend::Podman
    };
    let runtime_available = match backend {
        SandboxBackend::Docker => {
            should_use_docker_backend(is_cli_available("docker"), is_docker_daemon_available())
        },
        SandboxBackend::Podman => is_cli_available("podman"),
        _ => unreachable!(),
    };
    if runtime_available {
        return;
    }

    let error = match select_backend(SandboxConfig {
        backend,
        ..Default::default()
    }) {
        Ok(selected) => panic!(
            "unavailable explicit backend unexpectedly selected {}",
            selected.backend_id()
        ),
        Err(error) => error,
    };

    assert!(error.to_string().contains("unavailable"));
}

#[test]
fn failover_rejects_nonisolated_backend() {
    let isolated: Arc<dyn Sandbox> =
        Arc::new(TestSandbox::new(SandboxBackendId::Docker, None, None));
    let host: Arc<dyn Sandbox> = Arc::new(NoSandbox);

    let error = match FailoverSandbox::new(isolated, host) {
        Ok(_) => panic!("non-isolated failover backend must be rejected"),
        Err(error) => error,
    };

    assert!(error.to_string().contains("filesystem-isolated"));
}

#[test]
fn should_use_docker_backend_requires_cli_and_daemon() {
    assert!(should_use_docker_backend(true, true));
    assert!(!should_use_docker_backend(true, false));
    assert!(!should_use_docker_backend(false, true));
    assert!(!should_use_docker_backend(false, false));
}

#[test]
fn container_run_state_serializes_lowercase() {
    assert_eq!(
        serde_json::to_value(ContainerRunState::Running)
            .unwrap()
            .as_str(),
        Some("running")
    );
    assert_eq!(
        serde_json::to_value(ContainerRunState::Stopped)
            .unwrap()
            .as_str(),
        Some("stopped")
    );
    assert_eq!(
        serde_json::to_value(ContainerRunState::Exited)
            .unwrap()
            .as_str(),
        Some("exited")
    );
    assert_eq!(
        serde_json::to_value(ContainerRunState::Unknown)
            .unwrap()
            .as_str(),
        Some("unknown")
    );
}
