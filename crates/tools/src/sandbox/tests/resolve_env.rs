use std::sync::{Arc, atomic::Ordering};

use {super::*, crate::sandbox::ExecEnv};

#[tokio::test]
async fn resolve_env_returns_host_when_sandbox_is_off() {
    let backend = Arc::new(TestSandbox::new(
        "docker",
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
    let backend = Arc::new(TestSandbox::new("docker", None, None));
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
        mode: SandboxMode::All,
        ..Default::default()
    };
    let backends: Vec<Arc<dyn Sandbox>> = vec![
        Arc::new(NoSandbox),
        Arc::new(RestrictedHostSandbox::new(config.clone())),
        #[cfg(target_os = "linux")]
        Arc::new(CgroupSandbox::new(config.clone())),
    ];

    for backend in backends {
        let backend_name = backend.backend_name();
        let router = SandboxRouter::with_backend(config.clone(), backend);

        let error = match router.resolve_env("main").await {
            Err(error) => error,
            Ok(_) => panic!("non-isolated backend {backend_name} must fail closed"),
        };

        let message = error.to_string();
        assert!(message.contains(backend_name));
        assert!(message.contains("does not provide filesystem isolation"));
    }
}

#[tokio::test]
async fn resolve_env_fails_closed_when_backend_is_unavailable() {
    let backend = Arc::new(TestSandbox::new(
        "docker",
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
async fn resolve_env_fails_closed_after_failover_to_nonisolated_backend() {
    let primary = Arc::new(TestSandbox::new(
        "docker",
        Some("cannot connect to the Docker daemon"),
        None,
    ));
    let primary_backend: Arc<dyn Sandbox> = primary.clone();
    let fallback: Arc<dyn Sandbox> = Arc::new(NoSandbox);
    let failover: Arc<dyn Sandbox> = Arc::new(FailoverSandbox::new(primary_backend, fallback));
    let router = SandboxRouter::with_backend(SandboxConfig::default(), failover);

    let error = match router.resolve_env("main").await {
        Err(error) => error,
        Ok(_) => panic!("non-isolated fallback must fail closed"),
    };

    let message = error.to_string();
    assert!(message.contains("none"));
    assert!(message.contains("does not provide filesystem isolation"));
    assert_eq!(primary.ensure_ready_calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn resolve_env_preserves_session_agent_global_override_priority() {
    let backend: Arc<dyn Sandbox> = Arc::new(TestSandbox::new("docker", None, None));
    let config = SandboxConfig {
        mode: SandboxMode::All,
        ..Default::default()
    };
    let router = SandboxRouter::with_backend(config, backend);

    router.set_agent_override("main", true).await;
    router.set_override("main", false).await;
    assert!(matches!(
        router.resolve_env("main").await,
        Ok(ExecEnv::Host)
    ));

    router.remove_override("main").await;
    router.set_agent_override("main", false).await;
    assert!(matches!(
        router.resolve_env("main").await,
        Ok(ExecEnv::Host)
    ));

    router.remove_agent_override("main").await;
    assert!(matches!(
        router.resolve_env("main").await,
        Ok(ExecEnv::Sandbox { .. })
    ));
}
