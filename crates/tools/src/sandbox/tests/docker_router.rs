#![allow(clippy::unwrap_used, clippy::expect_used)]
use std::env;

use chelix_protocol::{TOOLS_SERVICE_HEALTH_PATH, TOOLS_SERVICE_PROTOCOL_VERSION};

use {
    super::*,
    crate::sandbox::docker::{
        force_remove_container, parse_container_addresses, select_reachable_tools_service_endpoint,
        tools_service_endpoint_candidates, tools_service_inspect_template,
    },
};

const TEST_TOOLS_SERVICE_BYTES: &[u8] = b"test-tools-service";

fn test_sandbox_image_tag(repo: &str, base: &str, packages: &[String]) -> String {
    sandbox_image_tag(repo, base, packages, TEST_TOOLS_SERVICE_BYTES)
}

#[test]
fn test_create_sandbox_off_uses_no_sandbox() {
    let config = SandboxConfig {
        mode: SandboxMode::Off,
        ..Default::default()
    };
    let sandbox = create_sandbox(config);
    assert_eq!(sandbox.backend_name(), "none");
    assert!(!sandbox.is_real());
    let id = SandboxId {
        scope: SandboxScope::Session,
        key: "test".into(),
    };
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        sandbox.ensure_ready(&id, None).await.unwrap();
        sandbox.cleanup(&id).await.unwrap();
    });
}

#[tokio::test]
async fn test_no_sandbox_command() {
    let sandbox = NoSandbox;
    let id = SandboxId {
        scope: SandboxScope::Session,
        key: "test".into(),
    };
    let opts = CommandOptions::default();
    let result = sandbox
        .run_command(&id, "echo sandbox-test", &opts)
        .await
        .unwrap();
    assert_eq!(result.stdout.trim(), "sandbox-test");
    assert_eq!(result.exit_code, 0);
}

#[tokio::test]
async fn test_no_sandbox_read_file_native() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("note.txt");
    std::fs::write(&file, "native read").unwrap();

    let sandbox = NoSandbox;
    let id = SandboxId {
        scope: SandboxScope::Session,
        key: "test-read".into(),
    };

    let result = sandbox
        .read_file(&id, &file.display().to_string(), 1024)
        .await
        .unwrap();
    match result {
        SandboxReadResult::Ok(bytes) => assert_eq!(bytes, b"native read"),
        other => panic!("expected Ok, got {other:?}"),
    }
}

#[tokio::test]
async fn test_no_sandbox_write_file_native() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("note.txt");

    let sandbox = NoSandbox;
    let id = SandboxId {
        scope: SandboxScope::Session,
        key: "test-write".into(),
    };

    let result = sandbox
        .write_file(&id, &file.display().to_string(), b"native write")
        .await
        .unwrap();
    assert!(result.is_none());
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "native write");
}

#[tokio::test]
async fn test_no_sandbox_list_files_native() {
    let dir = tempfile::tempdir().unwrap();
    let nested = dir.path().join("nested");
    std::fs::create_dir(&nested).unwrap();
    let first = dir.path().join("a.txt");
    let second = nested.join("b.txt");
    std::fs::write(&first, "a").unwrap();
    std::fs::write(&second, "b").unwrap();

    let sandbox = NoSandbox;
    let id = SandboxId {
        scope: SandboxScope::Session,
        key: "test-list".into(),
    };

    let files = sandbox
        .list_files(&id, &dir.path().display().to_string())
        .await
        .unwrap();
    assert_eq!(files.files, vec![
        first.display().to_string(),
        second.display().to_string(),
    ]);
    assert!(!files.truncated);
}

#[cfg(unix)]
#[tokio::test]
async fn test_no_sandbox_write_file_rejects_symlink_native() {
    let dir = tempfile::tempdir().unwrap();
    let real = dir.path().join("real.txt");
    let link = dir.path().join("link.txt");
    std::fs::write(&real, "original").unwrap();
    std::os::unix::fs::symlink(&real, &link).unwrap();

    let sandbox = NoSandbox;
    let id = SandboxId {
        scope: SandboxScope::Session,
        key: "test-symlink".into(),
    };

    let result = sandbox
        .write_file(&id, &link.display().to_string(), b"nope")
        .await
        .unwrap();
    let payload = result.expect("expected typed payload");
    assert_eq!(payload["kind"], "path_denied");
    assert_eq!(std::fs::read_to_string(&real).unwrap(), "original");
}

#[test]
fn test_docker_container_name() {
    let config = SandboxConfig {
        container_prefix: Some("my-prefix".into()),
        ..Default::default()
    };
    let docker = DockerSandbox::new(config);
    let id = SandboxId {
        scope: SandboxScope::Session,
        key: "abc123".into(),
    };
    assert_eq!(docker.container_name(&id), "my-prefix-abc123");
}

#[tokio::test]
async fn test_docker_startup_gate_serializes_same_container() {
    let docker = DockerSandbox::new(SandboxConfig::default());
    let first = docker.startup_gate_for("chelix-sandbox-session").await;
    let second = docker.startup_gate_for("chelix-sandbox-session").await;
    assert!(Arc::ptr_eq(&first, &second));

    let permit = first.acquire().await.unwrap();
    assert!(second.try_acquire().is_err());
    drop(permit);

    let _second_permit = second.try_acquire().unwrap();
}

#[tokio::test]
async fn test_docker_startup_gate_allows_different_containers() {
    let docker = DockerSandbox::new(SandboxConfig::default());
    let first = docker.startup_gate_for("chelix-sandbox-session-a").await;
    let second = docker.startup_gate_for("chelix-sandbox-session-b").await;
    assert!(!Arc::ptr_eq(&first, &second));

    let _first_permit = first.acquire().await.unwrap();
    let _second_permit = second.try_acquire().unwrap();
}

#[test]
fn test_container_name_conflict_detection() {
    assert!(is_container_name_conflict(
        "docker: Error response from daemon: Conflict. The container name \
         \"/chelix-myagent-sandbox-cron-57120844\" is already in use by container \
         \"7587022e73ff\"."
    ));
    assert!(is_container_name_conflict(
        "Error: creating container storage: the name \"chelix-sandbox-main\" is already in use"
    ));
    assert!(!is_container_name_conflict(
        "Error response from daemon: pull access denied for image"
    ));
    assert!(!is_container_name_conflict(
        "Error: creating container storage: the namespace \"chelix-sandbox-main\" is already in use"
    ));
}

/// Helper: build a `SandboxRouter` with a deterministic backend so tests
/// don't depend on the host having Docker / Apple Container installed.
fn router_with_real_backend(config: SandboxConfig) -> SandboxRouter {
    let backend: Arc<dyn Sandbox> = Arc::new(TestSandbox::new("docker", None, None));
    SandboxRouter::with_backend(config, backend)
}

#[tokio::test]
async fn test_sandbox_router_default_all() {
    let config = SandboxConfig::default(); // mode = All
    let router = router_with_real_backend(config);
    assert!(router.is_sandboxed("main").await);
    assert!(router.is_sandboxed("session:abc").await);
}

#[tokio::test]
async fn test_sandbox_router_mode_off() {
    let config = SandboxConfig {
        mode: SandboxMode::Off,
        ..Default::default()
    };
    let router = router_with_real_backend(config);
    assert!(!router.is_sandboxed("main").await);
    assert!(!router.is_sandboxed("session:abc").await);
}

#[tokio::test]
async fn test_sandbox_router_mode_all() {
    let config = SandboxConfig {
        mode: SandboxMode::All,
        ..Default::default()
    };
    let router = router_with_real_backend(config);
    assert!(router.is_sandboxed("main").await);
    assert!(router.is_sandboxed("session:abc").await);
}

#[tokio::test]
async fn test_sandbox_router_mode_non_main() {
    let config = SandboxConfig {
        mode: SandboxMode::NonMain,
        ..Default::default()
    };
    let router = router_with_real_backend(config);
    assert!(!router.is_sandboxed("main").await);
    assert!(router.is_sandboxed("session:abc").await);
}

#[tokio::test]
async fn test_sandbox_router_override() {
    let config = SandboxConfig {
        mode: SandboxMode::Off,
        ..Default::default()
    };
    let router = router_with_real_backend(config);
    assert!(!router.is_sandboxed("session:abc").await);

    router.set_override("session:abc", true).await;
    assert!(router.is_sandboxed("session:abc").await);

    router.set_override("session:abc", false).await;
    assert!(!router.is_sandboxed("session:abc").await);

    router.remove_override("session:abc").await;
    assert!(!router.is_sandboxed("session:abc").await);
}

#[tokio::test]
async fn test_sandbox_router_override_overrides_mode() {
    let config = SandboxConfig {
        mode: SandboxMode::All,
        ..Default::default()
    };
    let router = router_with_real_backend(config);
    assert!(router.is_sandboxed("main").await);

    // Override to disable sandbox for main
    router.set_override("main", false).await;
    assert!(!router.is_sandboxed("main").await);
}

#[tokio::test]
async fn test_sandbox_router_explicit_override_overrides_agent_override() {
    let config = SandboxConfig {
        mode: SandboxMode::NonMain,
        ..Default::default()
    };
    let router = router_with_real_backend(config);
    assert!(router.is_sandboxed("cron:test").await);

    router.set_override("cron:test", false).await;
    router.set_agent_override("cron:test", true).await;
    assert!(!router.is_sandboxed("cron:test").await);

    router.remove_agent_override("cron:test").await;
    assert!(!router.is_sandboxed("cron:test").await);
}

#[tokio::test]
async fn test_sandbox_router_agent_override_falls_back_to_mode() {
    let config = SandboxConfig {
        mode: SandboxMode::NonMain,
        ..Default::default()
    };
    let router = router_with_real_backend(config);

    router.set_agent_override("cron:test", false).await;
    assert!(!router.is_sandboxed("cron:test").await);

    router.remove_agent_override("cron:test").await;
    assert!(router.is_sandboxed("cron:test").await);
}

#[tokio::test]
async fn test_sandbox_router_agent_override_beats_global_off() {
    let config = SandboxConfig {
        mode: SandboxMode::Off,
        ..Default::default()
    };
    let router = router_with_real_backend(config);
    assert!(!router.is_sandboxed("main").await);

    router.set_agent_override("main", true).await;
    assert!(router.is_sandboxed("main").await);

    router.remove_agent_override("main").await;
    assert!(!router.is_sandboxed("main").await);
}

#[tokio::test]
async fn test_sandbox_router_no_runtime_returns_false() {
    let backend: Arc<dyn Sandbox> = Arc::new(NoSandbox);
    let config = SandboxConfig {
        mode: SandboxMode::All,
        ..Default::default()
    };
    let router = SandboxRouter::with_backend(config, backend);

    // Even with mode=All, no runtime means not sandboxed
    assert!(!router.is_sandboxed("main").await);
    assert!(!router.is_sandboxed("session:abc").await);

    // Overrides are also ignored when there's no runtime
    router.set_override("main", true).await;
    assert!(!router.is_sandboxed("main").await);
}

#[test]
fn test_backend_name_docker() {
    let sandbox = DockerSandbox::new(SandboxConfig::default());
    assert_eq!(sandbox.backend_name(), "docker");
}

#[test]
fn test_backend_name_podman() {
    let sandbox = DockerSandbox::podman(SandboxConfig::default());
    assert_eq!(sandbox.backend_name(), "podman");
}

#[test]
fn test_backend_name_none() {
    let sandbox = NoSandbox;
    assert_eq!(sandbox.backend_name(), "none");
}

#[test]
fn test_sandbox_router_backend_name() {
    // With "auto", the backend depends on what's available on the host.
    let config = SandboxConfig::default();
    let router = SandboxRouter::new(config);
    let name = router.backend_name();
    assert!(
        name == "docker"
            || name == "podman"
            || name == "apple-container"
            || name == "restricted-host",
        "unexpected backend: {name}"
    );
}

#[test]
fn test_sandbox_router_explicit_docker_backend() {
    let config = SandboxConfig {
        backend: "docker".into(),
        ..Default::default()
    };
    let router = SandboxRouter::new(config);
    assert_eq!(router.backend_name(), "docker");
}

#[test]
fn test_sandbox_router_config_accessor() {
    let config = SandboxConfig {
        mode: SandboxMode::NonMain,
        scope: SandboxScope::Agent,
        image: Some("alpine:latest".into()),
        ..Default::default()
    };
    let router = SandboxRouter::new(config);
    assert_eq!(*router.mode(), SandboxMode::NonMain);
    assert_eq!(router.config().scope, SandboxScope::Agent);
    assert_eq!(router.config().image.as_deref(), Some("alpine:latest"));
}

#[test]
fn test_sandbox_router_sandbox_id_for() {
    let config = SandboxConfig {
        scope: SandboxScope::Session,
        ..Default::default()
    };
    let router = SandboxRouter::new(config);
    let id = router.sandbox_id_for("session:abc");
    assert_eq!(id.key, "session-abc");
    // Plain alphanumeric keys pass through unchanged.
    let id2 = router.sandbox_id_for("main");
    assert_eq!(id2.key, "main");
}

#[tokio::test]
async fn test_resolve_image_default() {
    let config = SandboxConfig::default();
    let router = SandboxRouter::new(config);
    let img = router.resolve_image("main", None).await;
    assert_eq!(img, DEFAULT_SANDBOX_IMAGE);
}

#[tokio::test]
async fn test_resolve_image_skill_override() {
    let config = SandboxConfig::default();
    let router = SandboxRouter::new(config);
    let img = router
        .resolve_image("main", Some("chelix-cache/my-skill:abc123"))
        .await;
    assert_eq!(img, "chelix-cache/my-skill:abc123");
}

#[tokio::test]
async fn test_resolve_image_session_override() {
    let config = SandboxConfig::default();
    let router = SandboxRouter::new(config);
    router
        .set_image_override("sess1", "custom:latest".into())
        .await;
    let img = router.resolve_image("sess1", None).await;
    assert_eq!(img, "custom:latest");
}

#[tokio::test]
async fn test_resolve_image_skill_beats_session() {
    let config = SandboxConfig::default();
    let router = SandboxRouter::new(config);
    router
        .set_image_override("sess1", "custom:latest".into())
        .await;
    let img = router
        .resolve_image("sess1", Some("chelix-cache/skill:hash"))
        .await;
    assert_eq!(img, "chelix-cache/skill:hash");
}

#[tokio::test]
async fn test_resolve_image_config_override() {
    let config = SandboxConfig {
        image: Some("my-org/image:v1".into()),
        ..Default::default()
    };
    let router = SandboxRouter::new(config);
    let img = router.resolve_image("main", None).await;
    assert_eq!(img, "my-org/image:v1");
}

#[tokio::test]
async fn test_remove_image_override() {
    let config = SandboxConfig::default();
    let router = SandboxRouter::new(config);
    router
        .set_image_override("sess1", "custom:latest".into())
        .await;
    router.remove_image_override("sess1").await;
    let img = router.resolve_image("sess1", None).await;
    assert_eq!(img, DEFAULT_SANDBOX_IMAGE);
}

#[test]
fn test_tools_service_inspect_template_matches_backend_schema() {
    assert_eq!(
        tools_service_inspect_template(BackendKind::Docker),
        "{{range .NetworkSettings.Networks}}{{println .IPAddress}}{{end}}"
    );
    assert_eq!(
        tools_service_inspect_template(BackendKind::Podman),
        "{{println .NetworkSettings.IPAddress}}"
    );
}

#[test]
fn test_tools_service_endpoint_candidates_include_host_and_container_transports() {
    let candidates =
        tools_service_endpoint_candidates("127.0.0.1:32769\n", "10.222.1.11\n", "test-token");

    assert_eq!(
        candidates
            .iter()
            .map(|endpoint| endpoint.base_url.as_str())
            .collect::<Vec<_>>(),
        vec!["http://127.0.0.1:32769", "http://10.222.1.11:43271"]
    );
    assert!(
        candidates
            .iter()
            .all(|endpoint| endpoint.token == "test-token")
    );
}

#[test]
fn test_parse_container_addresses_ignores_empty_invalid_and_unspecified_values() {
    assert_eq!(
        parse_container_addresses("\nnot-an-address\n0.0.0.0\n::\n172.20.0.4\n"),
        vec!["172.20.0.4".parse::<std::net::IpAddr>().unwrap()]
    );
}

#[tokio::test]
async fn test_select_reachable_tools_service_endpoint_skips_unreachable_candidate() {
    let mut server = mockito::Server::new_async().await;
    let health = server
        .mock("GET", TOOLS_SERVICE_HEALTH_PATH)
        .match_header("authorization", "Bearer test-token")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(format!(
            "{{\"protocolVersion\":{TOOLS_SERVICE_PROTOCOL_VERSION}}}"
        ))
        .create_async()
        .await;
    let unreachable = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let unreachable_address = unreachable.local_addr().unwrap();
    drop(unreachable);
    let candidates = vec![
        ToolsServiceEndpoint {
            base_url: format!("http://{unreachable_address}"),
            token: "test-token".into(),
        },
        ToolsServiceEndpoint {
            base_url: server.url(),
            token: "test-token".into(),
        },
    ];
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(1))
        .build()
        .unwrap();

    let selected = select_reachable_tools_service_endpoint(&client, &candidates)
        .await
        .unwrap();

    assert_eq!(selected.base_url, server.url());
    health.assert_async().await;
}

#[cfg(unix)]
#[tokio::test]
async fn test_discover_tools_service_endpoint_runs_oci_discovery_once() {
    use {
        std::os::unix::fs::PermissionsExt,
        tokio::io::{AsyncReadExt, AsyncWriteExt},
    };

    let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
        .await
        .unwrap();
    let port = listener.local_addr().unwrap().port();
    let health_requests = Arc::new(AtomicUsize::new(0));
    let health_requests_task = Arc::clone(&health_requests);
    let server = tokio::spawn(async move {
        loop {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = [0_u8; 2048];
            let _ = socket.read(&mut request).await.unwrap();
            let attempt = health_requests_task.fetch_add(1, Ordering::SeqCst) + 1;
            let (status, body) = if attempt < 3 {
                ("503 Service Unavailable", "{\"error\":\"starting\"}")
            } else {
                ("200 OK", "{\"protocolVersion\":1}")
            };
            let response = format!(
                "HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.len()
            );
            socket.write_all(response.as_bytes()).await.unwrap();
            if attempt >= 3 {
                break;
            }
        }
    });

    let directory = tempfile::tempdir().unwrap();
    let cli = directory.path().join("container-cli");
    let calls = directory.path().join("container-cli.calls");
    std::fs::write(
        &cli,
        format!(
            "#!/bin/sh\nprintf '%s\\n' \"$1\" >> \"${{0}}.calls\"\ncase \"$1\" in\n  port) printf '127.0.0.1:{port}\\n' ;;\n  inspect) printf '\\n' ;;\n  *) exit 64 ;;\nesac\n"
        ),
    )
    .unwrap();
    std::fs::set_permissions(&cli, std::fs::Permissions::from_mode(0o755)).unwrap();
    let cli: &'static str = Box::leak(cli.to_string_lossy().into_owned().into_boxed_str());
    let sandbox = DockerSandbox::with_cli(SandboxConfig::default(), cli);

    let endpoint = sandbox
        .discover_tools_service_endpoint("sandbox-name", "test-token".into())
        .await
        .unwrap();

    assert_eq!(endpoint.base_url, format!("http://127.0.0.1:{port}"));
    assert_eq!(health_requests.load(Ordering::SeqCst), 3);
    assert_eq!(std::fs::read_to_string(calls).unwrap(), "port\ninspect\n");
    server.await.unwrap();
}

#[cfg(unix)]
async fn spawn_single_health_server() -> (u16, tokio::task::JoinHandle<()>) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
        .await
        .unwrap();
    let port = listener.local_addr().unwrap().port();
    let task = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut request = [0_u8; 2048];
        let _ = socket.read(&mut request).await.unwrap();
        let body = format!("{{\"protocolVersion\":{TOOLS_SERVICE_PROTOCOL_VERSION}}}");
        let response = format!(
            "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
            body.len()
        );
        socket.write_all(response.as_bytes()).await.unwrap();
    });
    (port, task)
}

#[cfg(unix)]
#[tokio::test]
async fn test_ensure_ready_recreates_stopped_container_with_fresh_endpoint() {
    use std::os::unix::fs::PermissionsExt;

    let (first_port, first_health) = spawn_single_health_server().await;
    let (second_port, second_health) = spawn_single_health_server().await;
    let directory = tempfile::tempdir().unwrap();
    let cli = directory.path().join("container-cli");
    let state = directory.path().join("container-cli.state");
    let generation = directory.path().join("container-cli.generation");
    let removals = directory.path().join("container-cli.removals");
    std::fs::write(&state, "missing\n").unwrap();
    std::fs::write(&generation, "0\n").unwrap();
    std::fs::write(&removals, "0\n").unwrap();
    std::fs::write(
        &cli,
        format!(
            "#!/bin/sh\nstate_file=\"${{0}}.state\"\ngeneration_file=\"${{0}}.generation\"\nremovals_file=\"${{0}}.removals\"\ncase \"$1\" in\n  image) exit 0 ;;\n  inspect)\n    if [ \"$3\" = '{{{{.State.Running}}}}' ]; then\n      if [ \"$(cat \"$state_file\")\" = running ]; then printf 'true\\n'; else printf 'false\\n'; fi\n    else\n      printf '\\n'\n    fi\n    ;;\n  run)\n    if [ \"$(cat \"$state_file\")\" != missing ]; then\n      printf 'Error response from daemon: the container name is already in use\\n' >&2\n      exit 125\n    fi\n    next=$(( $(cat \"$generation_file\") + 1 ))\n    printf '%s\\n' \"$next\" > \"$generation_file\"\n    printf 'running\\n' > \"$state_file\"\n    ;;\n  port)\n    if [ \"$(cat \"$generation_file\")\" = 1 ]; then printf '127.0.0.1:{first_port}\\n'; else printf '127.0.0.1:{second_port}\\n'; fi\n    ;;\n  rm)\n    next=$(( $(cat \"$removals_file\") + 1 ))\n    printf '%s\\n' \"$next\" > \"$removals_file\"\n    printf 'missing\\n' > \"$state_file\"\n    ;;\n  *) exit 64 ;;\nesac\n"
        ),
    )
    .unwrap();
    std::fs::set_permissions(&cli, std::fs::Permissions::from_mode(0o755)).unwrap();
    let cli: &'static str = Box::leak(cli.to_string_lossy().into_owned().into_boxed_str());
    let sandbox = DockerSandbox::with_cli(
        SandboxConfig {
            home_persistence: HomePersistence::Off,
            host_data_dir: Some(directory.path().join("data")),
            image: Some("test-image:latest".into()),
            ..Default::default()
        },
        cli,
    );
    let id = SandboxId {
        scope: SandboxScope::Session,
        key: "oom-recovery".into(),
    };

    sandbox.ensure_ready(&id, None).await.unwrap();
    let first_endpoint = sandbox.tools_service_endpoint(&id).await.unwrap();
    std::fs::write(&state, "stopped\n").unwrap();

    sandbox.ensure_ready(&id, None).await.unwrap();
    let second_endpoint = sandbox.tools_service_endpoint(&id).await.unwrap();

    assert_eq!(
        first_endpoint.base_url,
        format!("http://127.0.0.1:{first_port}")
    );
    assert_eq!(
        second_endpoint.base_url,
        format!("http://127.0.0.1:{second_port}")
    );
    assert_ne!(first_endpoint.token, second_endpoint.token);
    assert_eq!(std::fs::read_to_string(generation).unwrap(), "2\n");
    assert_eq!(std::fs::read_to_string(removals).unwrap(), "1\n");
    first_health.await.unwrap();
    second_health.await.unwrap();
    sandbox.cleanup(&id).await.unwrap();
}

#[cfg(unix)]
#[tokio::test]
async fn test_force_remove_container_uses_rm_force_arguments() {
    use std::os::unix::fs::PermissionsExt;

    let directory = tempfile::tempdir().unwrap();
    let cli = directory.path().join("container-cli");
    let arguments = directory.path().join("container-cli.args");
    std::fs::write(&cli, "#!/bin/sh\nprintf '%s\\n' \"$@\" > \"${0}.args\"\n").unwrap();
    std::fs::set_permissions(&cli, std::fs::Permissions::from_mode(0o755)).unwrap();

    force_remove_container(cli.to_str().unwrap(), "sandbox-name")
        .await
        .unwrap();

    assert_eq!(
        std::fs::read_to_string(arguments).unwrap(),
        "rm\n-f\nsandbox-name\n"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn test_force_remove_container_reports_nonzero_status() {
    use std::os::unix::fs::PermissionsExt;

    let directory = tempfile::tempdir().unwrap();
    let cli = directory.path().join("container-cli");
    std::fs::write(&cli, "#!/bin/sh\nprintf 'cleanup denied\\n' >&2\nexit 23\n").unwrap();
    std::fs::set_permissions(&cli, std::fs::Permissions::from_mode(0o755)).unwrap();

    let error = force_remove_container(cli.to_str().unwrap(), "sandbox-name")
        .await
        .unwrap_err();

    assert!(error.to_string().contains("cleanup denied"));
    assert!(error.to_string().contains("sandbox-name"));
}

#[test]
fn test_docker_image_tag_deterministic() {
    let packages = vec!["curl".into(), "git".into(), "wget".into()];
    let tag1 = test_sandbox_image_tag("chelix-main-sandbox", "ubuntu:26.04", &packages);
    let tag2 = test_sandbox_image_tag("chelix-main-sandbox", "ubuntu:26.04", &packages);
    assert_eq!(tag1, tag2);
    assert!(tag1.starts_with("chelix-main-sandbox:"));
}

#[test]
fn test_docker_image_tag_order_independent() {
    let p1 = vec!["curl".into(), "git".into()];
    let p2 = vec!["git".into(), "curl".into()];
    assert_eq!(
        test_sandbox_image_tag("chelix-main-sandbox", "ubuntu:26.04", &p1),
        test_sandbox_image_tag("chelix-main-sandbox", "ubuntu:26.04", &p2),
    );
}

#[test]
fn test_docker_image_tag_normalizes_whitespace_and_duplicates() {
    let p1 = vec!["curl".into(), "git".into(), "curl".into()];
    let p2 = vec![" git ".into(), "curl".into()];
    assert_eq!(
        test_sandbox_image_tag("chelix-main-sandbox", "ubuntu:26.04", &p1),
        test_sandbox_image_tag("chelix-main-sandbox", "ubuntu:26.04", &p2),
    );
}

#[test]
fn test_sandbox_image_dockerfile_creates_home_in_install_layer() {
    let dockerfile = sandbox_image_dockerfile("ubuntu:26.04", &["curl".into()]);
    assert!(dockerfile.contains(
        "RUN apt-get update -qq && apt-get install -y -qq curl ripgrep && mkdir -p /home/sandbox && sed -i 's#^\\(root:[^:]*:[^:]*:[^:]*:[^:]*:\\)[^:]*:#\\1/home/sandbox:#' /etc/passwd"
    ));
    assert!(!dockerfile.contains("RUN mkdir -p /home/sandbox\n"));
}

#[test]
fn test_sandbox_image_dockerfile_installs_gogcli() {
    let dockerfile = sandbox_image_dockerfile("ubuntu:26.04", &["curl".into()]);
    assert!(dockerfile.contains(&format!("go install {GOGCLI_MODULE_PATH}@{GOGCLI_VERSION}")));
    assert!(dockerfile.contains("ln -sf /usr/local/bin/gog /usr/local/bin/gogcli"));
}

#[test]
fn test_sandbox_image_dockerfile_respects_go_tool_installs() {
    let dockerfile = sandbox_image_dockerfile("ubuntu:26.04", &["curl".into()]);
    for (module, version, _bin) in GO_TOOL_INSTALLS {
        assert!(
            dockerfile.contains(&format!("go install {module}@{version}")),
            "Dockerfile should install {module}"
        );
    }
}

#[test]
fn test_sandbox_image_dockerfile_adds_nodesource_for_nodejs() {
    let dockerfile = sandbox_image_dockerfile("ubuntu:26.04", &["curl".into(), "nodejs".into()]);
    assert!(dockerfile.contains("nodesource.gpg"));
    assert!(dockerfile.contains("node_22.x"));
    // Bootstraps curl+gnupg before using them
    assert!(dockerfile.contains("apt-get install -y -qq curl gnupg"));
    // nodejs should remain in the main apt-get install line
    assert!(dockerfile.contains("nodejs"));
    // npm is superseded by NodeSource nodejs and should be filtered out
    let dockerfile_with_npm = sandbox_image_dockerfile("ubuntu:26.04", &[
        "curl".into(),
        "nodejs".into(),
        "npm".into(),
    ]);
    assert!(!dockerfile_with_npm.contains(" npm "));
}

#[test]
fn test_sandbox_image_dockerfile_no_nodesource_without_nodejs() {
    let dockerfile = sandbox_image_dockerfile("ubuntu:26.04", &["curl".into(), "git".into()]);
    assert!(!dockerfile.contains("nodesource"));
}

#[test]
fn test_sandbox_image_dockerfile_npm_without_nodejs_kept() {
    // npm without nodejs is a valid config — should not be filtered
    let dockerfile = sandbox_image_dockerfile("ubuntu:26.04", &["npm".into(), "curl".into()]);
    assert!(dockerfile.contains("npm"));
    assert!(!dockerfile.contains("nodesource"));
}

#[test]
fn test_sandbox_image_dockerfile_adds_gh_repo() {
    let dockerfile = sandbox_image_dockerfile("ubuntu:26.04", &["curl".into(), "gh".into()]);
    assert!(dockerfile.contains("githubcli-archive-keyring.gpg"));
    assert!(dockerfile.contains("cli.github.com/packages"));
    assert!(dockerfile.contains("apt-get install -y -qq gh"));
    // gh should NOT appear in the main apt-get install line
    let main_install_line = dockerfile
        .lines()
        .find(|l| l.contains("apt-get install -y -qq") && !l.contains("githubcli"))
        .unwrap();
    assert!(!main_install_line.contains(" gh "));
}

#[test]
fn test_sandbox_image_dockerfile_no_gh_repo_without_gh() {
    let dockerfile = sandbox_image_dockerfile("ubuntu:26.04", &["curl".into(), "git".into()]);
    assert!(!dockerfile.contains("githubcli"));
}

#[test]
fn test_docker_image_tag_changes_with_base() {
    let packages = vec!["curl".into()];
    let t1 = test_sandbox_image_tag("chelix-main-sandbox", "ubuntu:26.04", &packages);
    let t2 = test_sandbox_image_tag("chelix-main-sandbox", "ubuntu:24.04", &packages);
    assert_ne!(t1, t2);
}

#[test]
fn test_docker_image_tag_changes_with_packages() {
    let p1 = vec!["curl".into()];
    let p2 = vec!["curl".into(), "git".into()];
    let t1 = test_sandbox_image_tag("chelix-main-sandbox", "ubuntu:26.04", &p1);
    let t2 = test_sandbox_image_tag("chelix-main-sandbox", "ubuntu:26.04", &p2);
    assert_ne!(t1, t2);
}

#[test]
fn test_docker_image_tag_changes_with_tools_service_bytes() {
    let packages = vec!["curl".into()];
    let first = sandbox_image_tag(
        "chelix-main-sandbox",
        "ubuntu:26.04",
        &packages,
        b"first-tools-service",
    );
    let second = sandbox_image_tag(
        "chelix-main-sandbox",
        "ubuntu:26.04",
        &packages,
        b"second-tools-service",
    );
    assert_ne!(first, second);
}

#[tokio::test]
async fn test_no_sandbox_build_image_is_noop() {
    let sandbox = NoSandbox;
    let result = sandbox
        .build_image("ubuntu:26.04", &["curl".into()])
        .await
        .unwrap();
    assert!(result.is_none());
}

#[tokio::test]
async fn test_sandbox_router_events() {
    let config = SandboxConfig::default();
    let router = SandboxRouter::new(config);
    let mut rx = router.subscribe_events();

    router.emit_event(SandboxEvent::Provisioning {
        container: "test".into(),
        packages: vec!["curl".into()],
    });

    let event = rx.try_recv().unwrap();
    match event {
        SandboxEvent::Provisioning {
            container,
            packages,
        } => {
            assert_eq!(container, "test");
            assert_eq!(packages, vec!["curl".to_string()]);
        },
        _ => panic!("unexpected event variant"),
    }

    assert!(router.mark_preparing_once("main").await);
    assert!(!router.mark_preparing_once("main").await);
    router.clear_prepared_session("main").await;
    assert!(router.mark_preparing_once("main").await);
}

#[tokio::test]
async fn test_sandbox_router_global_image_override() {
    let config = SandboxConfig::default();
    let router = SandboxRouter::new(config);

    // Default
    let img = router.default_image().await;
    assert_eq!(img, DEFAULT_SANDBOX_IMAGE);

    // Set global override
    router
        .set_global_image(Some("chelix-sandbox:abc123".into()))
        .await;
    let img = router.default_image().await;
    assert_eq!(img, "chelix-sandbox:abc123");

    // Global override flows through resolve_image
    let img = router.resolve_image("main", None).await;
    assert_eq!(img, "chelix-sandbox:abc123");

    // Session override still wins
    router.set_image_override("main", "custom:v1".into()).await;
    let img = router.resolve_image("main", None).await;
    assert_eq!(img, "custom:v1");

    // Clear and revert
    router.set_global_image(None).await;
    router.remove_image_override("main").await;
    let img = router.default_image().await;
    assert_eq!(img, DEFAULT_SANDBOX_IMAGE);
}

#[tokio::test]
async fn test_sandbox_router_backend_image_override_is_scoped() {
    let config = SandboxConfig {
        backend: "docker".into(),
        ..Default::default()
    };
    let mut router = SandboxRouter::new(config);
    router.register_backend(Arc::new(RestrictedHostSandbox::new(
        SandboxConfig::default(),
    )));

    router.set_global_image(Some("global:built".into())).await;
    router
        .set_backend_image("docker", "docker:built".into())
        .await
        .unwrap();
    router
        .set_backend_image("restricted-host", "restricted:built".into())
        .await
        .unwrap();

    assert_eq!(
        router
            .resolve_image_for_backend("session:abc", None, "docker")
            .await,
        "docker:built"
    );
    assert_eq!(
        router
            .resolve_image_for_backend("session:abc", None, "restricted-host")
            .await,
        "restricted:built"
    );

    router
        .set_image_override("session:abc", "session:built".into())
        .await;
    assert_eq!(
        router
            .resolve_image_for_backend("session:abc", None, "restricted-host")
            .await,
        "session:built"
    );
    assert_eq!(
        router
            .resolve_image_for_backend("session:abc", Some("skill:built"), "docker")
            .await,
        "skill:built"
    );
}

// ── Sandbox escape regression tests (issue #923) ───────────────────────────

#[test]
fn test_no_sandbox_does_not_provide_fs_isolation() {
    let sandbox = NoSandbox;
    assert!(!sandbox.provides_fs_isolation());
}

#[test]
fn test_docker_sandbox_provides_fs_isolation() {
    let sandbox = DockerSandbox::new(SandboxConfig::default());
    assert!(sandbox.provides_fs_isolation());
}

#[test]
fn test_podman_sandbox_provides_fs_isolation() {
    let sandbox = DockerSandbox::podman(SandboxConfig::default());
    assert!(sandbox.provides_fs_isolation());
}

#[tokio::test]
async fn test_failover_sandbox_reports_active_backend_name() {
    // Primary: a "docker" backend that always fails.
    let primary = Arc::new(TestSandbox::new(
        "docker",
        Some("cannot connect to the docker daemon"),
        None,
    ));
    // Fallback: restricted-host.
    let fallback: Arc<dyn Sandbox> = Arc::new(RestrictedHostSandbox::new(SandboxConfig::default()));

    let failover = FailoverSandbox::new(primary, fallback);

    // Before failover: reports primary name.
    assert_eq!(failover.backend_name(), "docker");
    assert!(failover.provides_fs_isolation());

    // Trigger failover via ensure_ready.
    let id = SandboxId {
        scope: SandboxScope::Session,
        key: "test-failover".into(),
    };
    failover.ensure_ready(&id, None).await.unwrap();

    // After failover: reports fallback name and isolation level.
    assert_eq!(failover.backend_name(), "restricted-host");
    assert!(
        !failover.provides_fs_isolation(),
        "after failing over to restricted-host, FS isolation must be false"
    );
}

#[tokio::test]
async fn test_failover_sandbox_to_restricted_host_does_not_claim_fs_isolation() {
    // Simulate macOS failover: apple-container → restricted-host
    let primary = Arc::new(TestSandbox::new(
        "apple-container",
        Some("XPC connection error"),
        None,
    ));
    let fallback: Arc<dyn Sandbox> = Arc::new(RestrictedHostSandbox::new(SandboxConfig::default()));
    let failover = FailoverSandbox::new(primary, fallback);

    let id = SandboxId {
        scope: SandboxScope::Session,
        key: "test-apple-failover".into(),
    };

    // Trigger failover.
    failover.ensure_ready(&id, None).await.unwrap();

    // The critical assertion: code must NOT see "apple-container" anymore.
    assert_ne!(
        failover.backend_name(),
        "apple-container",
        "backend_name must not mask failover to restricted-host"
    );
    assert!(!failover.provides_fs_isolation());
}

#[tokio::test]
async fn test_failover_sandbox_read_file_enforces_path_allowlist() {
    // After failover to RestrictedHostSandbox, file operations must go through
    // the fallback's read_file (which checks the path allowlist), not through
    // the default trait impl that calls self.run_command() and bypasses the check.
    let primary = Arc::new(TestSandbox::new(
        "docker",
        Some("cannot connect to the docker daemon"),
        None,
    ));
    let fallback: Arc<dyn Sandbox> = Arc::new(RestrictedHostSandbox::new(SandboxConfig::default()));
    let failover = FailoverSandbox::new(primary, fallback);

    let id = SandboxId {
        scope: SandboxScope::Session,
        key: "test-failover-read".into(),
    };

    // Trigger failover.
    failover.ensure_ready(&id, None).await.unwrap();
    assert_eq!(failover.backend_name(), "restricted-host");

    // read_file on a blocked path must be rejected by the allowlist.
    let result = failover.read_file(&id, "/etc/passwd", 4096).await;
    assert!(
        result.is_err(),
        "FailoverSandbox.read_file must enforce path allowlist after failover to restricted-host"
    );
}

#[tokio::test]
async fn test_failover_sandbox_write_file_enforces_path_allowlist() {
    let primary = Arc::new(TestSandbox::new(
        "docker",
        Some("cannot connect to the docker daemon"),
        None,
    ));
    let fallback: Arc<dyn Sandbox> = Arc::new(RestrictedHostSandbox::new(SandboxConfig::default()));
    let failover = FailoverSandbox::new(primary, fallback);

    let id = SandboxId {
        scope: SandboxScope::Session,
        key: "test-failover-write".into(),
    };

    failover.ensure_ready(&id, None).await.unwrap();

    let result = failover.write_file(&id, "/var/log/evil.txt", b"nope").await;
    assert!(
        result.is_err(),
        "FailoverSandbox.write_file must enforce path allowlist after failover"
    );
}

#[tokio::test]
async fn test_failover_sandbox_list_files_enforces_path_allowlist() {
    let primary = Arc::new(TestSandbox::new(
        "docker",
        Some("cannot connect to the docker daemon"),
        None,
    ));
    let fallback: Arc<dyn Sandbox> = Arc::new(RestrictedHostSandbox::new(SandboxConfig::default()));
    let failover = FailoverSandbox::new(primary, fallback);

    let id = SandboxId {
        scope: SandboxScope::Session,
        key: "test-failover-list".into(),
    };

    failover.ensure_ready(&id, None).await.unwrap();

    let result = failover.list_files(&id, "/etc").await;
    assert!(
        result.is_err(),
        "FailoverSandbox.list_files must enforce path allowlist after failover"
    );
}

/// E2E regression test for #796: Podman+BuildKit may leave images in
/// BuildKit's cache instead of the Podman store.  Gated behind
/// `CHELIX_SANDBOX_RUNTIME_E2E=1` and requires Podman to be installed.
#[tokio::test]
async fn test_podman_build_image_exists_in_store() {
    let enabled = env::var("CHELIX_SANDBOX_RUNTIME_E2E")
        .map(|v| matches!(v.trim().to_ascii_lowercase().as_str(), "1" | "true" | "yes"))
        .unwrap_or(false);
    if !enabled || !is_cli_available("podman") {
        eprintln!(
            "skipping test_podman_build_image_exists_in_store (set CHELIX_SANDBOX_RUNTIME_E2E=1 and install podman)"
        );
        return;
    }

    let sandbox = DockerSandbox::podman(SandboxConfig::default());
    let packages = vec!["curl".into()];
    let tag = test_sandbox_image_tag(sandbox.image_repo(), "ubuntu:26.04", &packages);

    // Remove any pre-existing image so we exercise the full build path.
    let _ = tokio::process::Command::new("podman")
        .args(["rmi", "-f", &tag])
        .output()
        .await;

    let result = sandbox
        .build_image("ubuntu:26.04", &packages)
        .await
        .expect("build_image should succeed");
    let result = result.expect("build_image should return Some for non-empty packages");
    assert_eq!(result.tag, tag);

    // The critical assertion: the image must be in the Podman store.
    assert!(
        sandbox_image_exists("podman", &tag).await,
        "image {tag} must exist in podman store after build_image"
    );

    // Cleanup.
    let _ = tokio::process::Command::new("podman")
        .args(["rmi", "-f", &tag])
        .output()
        .await;
}

// ── Multi-backend router tests ──────────────────────────────────────

#[test]
fn test_router_available_backends_contains_default() {
    let config = SandboxConfig {
        backend: "docker".into(),
        ..Default::default()
    };
    let router = SandboxRouter::new(config);
    let backends = router.available_backends();
    assert!(
        backends.contains(&"docker"),
        "default backend must be listed"
    );
}

#[test]
fn test_router_register_backend_adds_to_available() {
    let config = SandboxConfig {
        backend: "docker".into(),
        ..Default::default()
    };
    let mut router = SandboxRouter::new(config);
    assert!(!router.available_backends().contains(&"restricted-host"));

    router.register_backend(Arc::new(RestrictedHostSandbox::new(
        SandboxConfig::default(),
    )));
    let backends = router.available_backends();
    assert!(backends.contains(&"docker"));
    assert!(backends.contains(&"restricted-host"));
}

#[tokio::test]
async fn test_resolve_backend_returns_default_without_override() {
    let config = SandboxConfig {
        backend: "docker".into(),
        ..Default::default()
    };
    let router = SandboxRouter::new(config);
    let backend = router.resolve_backend("session:abc").await;
    assert_eq!(backend.backend_name(), "docker");
}

#[tokio::test]
async fn test_resolve_backend_returns_overridden_backend() {
    let config = SandboxConfig {
        backend: "docker".into(),
        ..Default::default()
    };
    let mut router = SandboxRouter::new(config);
    router.register_backend(Arc::new(RestrictedHostSandbox::new(
        SandboxConfig::default(),
    )));

    router
        .set_backend_override("session:abc", "restricted-host")
        .await
        .unwrap();

    let backend = router.resolve_backend("session:abc").await;
    assert_eq!(backend.backend_name(), "restricted-host");

    // Other sessions still get the default.
    let default_backend = router.resolve_backend("session:other").await;
    assert_eq!(default_backend.backend_name(), "docker");
}

#[tokio::test]
async fn test_set_backend_override_clears_runtime_state() {
    let config = SandboxConfig {
        backend: "docker".into(),
        ..Default::default()
    };
    let mut router = SandboxRouter::new(config);
    router.register_backend(Arc::new(RestrictedHostSandbox::new(
        SandboxConfig::default(),
    )));

    assert!(router.mark_preparing_once("session:abc").await);
    router.mark_synced("session:abc").await;
    assert!(!router.mark_preparing_once("session:abc").await);
    assert!(router.is_synced("session:abc").await);

    router
        .set_backend_override("session:abc", "restricted-host")
        .await
        .unwrap();

    assert!(router.mark_preparing_once("session:abc").await);
    assert!(!router.is_synced("session:abc").await);
}

#[tokio::test]
async fn test_set_backend_override_rejects_unknown_backend() {
    let config = SandboxConfig {
        backend: "docker".into(),
        ..Default::default()
    };
    let router = SandboxRouter::new(config);
    let result = router
        .set_backend_override("session:abc", "nonexistent")
        .await;
    assert!(result.is_err());
}

#[tokio::test]
async fn test_remove_backend_override_reverts_to_default() {
    let config = SandboxConfig {
        backend: "docker".into(),
        ..Default::default()
    };
    let mut router = SandboxRouter::new(config);
    router.register_backend(Arc::new(RestrictedHostSandbox::new(
        SandboxConfig::default(),
    )));

    router
        .set_backend_override("session:abc", "restricted-host")
        .await
        .unwrap();
    assert_eq!(
        router.resolve_backend("session:abc").await.backend_name(),
        "restricted-host"
    );

    router.remove_backend_override("session:abc").await;
    assert_eq!(
        router.resolve_backend("session:abc").await.backend_name(),
        "docker"
    );
}

#[tokio::test]
async fn test_cleanup_session_clears_backend_override() {
    let config = SandboxConfig {
        backend: "docker".into(),
        ..Default::default()
    };
    let mut router = SandboxRouter::new(config);
    router.register_backend(Arc::new(RestrictedHostSandbox::new(
        SandboxConfig::default(),
    )));

    router
        .set_backend_override("session:abc", "restricted-host")
        .await
        .unwrap();

    // cleanup_session should clear the backend override (along with other overrides).
    // Note: this will call cleanup on docker (the resolved backend at call time),
    // which is a no-op for containers that don't exist — that's fine for testing.
    let _ = router.cleanup_session("session:abc").await;

    // After cleanup, should revert to default.
    assert_eq!(
        router.resolve_backend("session:abc").await.backend_name(),
        "docker"
    );
}
