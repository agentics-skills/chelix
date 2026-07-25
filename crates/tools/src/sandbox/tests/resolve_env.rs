use std::sync::{Arc, atomic::Ordering};

use {super::*, crate::sandbox::ExecEnv};

#[tokio::test]
async fn resolve_env_returns_host_when_sandbox_is_off() {
    let backend = Arc::new(TestSandbox::new(
        SandboxBackendId::Docker,
        Some("sandbox preparation must not run"),
        None,
    ));
    let routed_backend: Arc<dyn Sandbox> = backend.clone();
    let config = SandboxConfig {
        mode: SandboxMode::Off,
        ..Default::default()
    };
    let router = SandboxRouter::with_backend(config, routed_backend);

    let env = router.resolve_env("main").await;

    assert!(matches!(env, Ok(ExecEnv::Host)));
    assert_eq!(backend.ensure_ready_calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn resolve_env_returns_sandbox_for_isolated_backend() {
    let backend = Arc::new(TestSandbox::new(SandboxBackendId::Docker, None, None));
    let routed_backend: Arc<dyn Sandbox> = backend.clone();
    let router = SandboxRouter::with_backend(SandboxConfig::default(), routed_backend.clone());

    let env = router.resolve_env("session:isolated").await;

    match env {
        Ok(ExecEnv::Sandbox {
            backend: resolved_backend,
            id,
        }) => {
            assert!(Arc::ptr_eq(&resolved_backend, &routed_backend));
            assert_eq!(id.scope, SandboxScope::Session);
            assert_eq!(id.key, "session-isolated");
        },
        Ok(ExecEnv::Host) => panic!("expected an isolated sandbox environment"),
        Err(error) => panic!("sandbox resolution failed: {error}"),
    }
    assert_eq!(backend.ensure_ready_calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn resolve_env_fails_closed_for_nonisolated_backend() {
    let config = SandboxConfig {
        mode: SandboxMode::On,
        ..Default::default()
    };
    let router = SandboxRouter::with_backend(config, Arc::new(NoSandbox));

    let error = match router.resolve_env("main").await {
        Err(error) => error,
        Ok(_) => panic!("direct host backend must fail closed while sandbox mode is on"),
    };

    let message = error.to_string();
    assert!(message.contains("none"));
    assert!(message.contains("does not provide filesystem isolation"));
}

#[tokio::test]
async fn resolve_env_fails_closed_when_backend_is_unavailable() {
    let backend = Arc::new(TestSandbox::new(
        SandboxBackendId::Docker,
        Some("container runtime unavailable"),
        None,
    ));
    let routed_backend: Arc<dyn Sandbox> = backend.clone();
    let router = SandboxRouter::with_backend(SandboxConfig::default(), routed_backend);

    let error = match router.resolve_env("main").await {
        Err(error) => error,
        Ok(_) => panic!("unavailable backend must fail closed"),
    };

    assert!(error.to_string().contains("container runtime unavailable"));
    assert_eq!(backend.ensure_ready_calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn resolve_env_uses_one_global_policy_for_every_session() {
    let backend: Arc<dyn Sandbox> =
        Arc::new(TestSandbox::new(SandboxBackendId::Docker, None, None));
    let config = SandboxConfig {
        mode: SandboxMode::On,
        ..Default::default()
    };
    let router = SandboxRouter::with_backend(config, backend);

    assert!(matches!(
        router.resolve_env("main").await,
        Ok(ExecEnv::Sandbox { .. })
    ));
    assert!(matches!(
        router.resolve_env("session:other").await,
        Ok(ExecEnv::Sandbox { .. })
    ));
}
