#![allow(clippy::unwrap_used, clippy::expect_used)]
use {
    super::*,
    crate::sandbox::{
        paths::{MountAccess, resolve_sandbox_mount_path, resolved_sandbox_mount_plan},
        types::tail_lines,
    },
};

#[test]
fn test_sandbox_mode_display() {
    assert_eq!(SandboxMode::Off.to_string(), "off");
    assert_eq!(SandboxMode::NonMain.to_string(), "non-main");
    assert_eq!(SandboxMode::All.to_string(), "all");
}

#[test]
fn test_sandbox_scope_display() {
    assert_eq!(SandboxScope::Session.to_string(), "session");
    assert_eq!(SandboxScope::Agent.to_string(), "agent");
    assert_eq!(SandboxScope::Shared.to_string(), "shared");
}

#[test]
fn test_docker_hardening_args_prebuilt() {
    let args = DockerSandbox::hardening_args(true, BackendKind::Docker, WorkspaceSysmount::Ro);
    assert!(args.contains(&"--cap-drop".to_string()));
    assert!(args.contains(&"ALL".to_string()));
    assert!(args.contains(&"--security-opt".to_string()));
    assert!(args.contains(&"no-new-privileges".to_string()));
    assert!(args.contains(&"--read-only".to_string()));
    // Verify tmpfs mounts are present
    assert!(args.contains(&"/tmp:rw,nosuid,size=256m".to_string()));
    assert!(args.contains(&"/run:rw,nosuid,size=64m".to_string()));
    // Host metadata isolation — assert flag-value adjacency for --hostname
    let hostname_pos = args
        .iter()
        .position(|a| a == "--hostname")
        .expect("--hostname flag missing");
    assert_eq!(
        args[hostname_pos + 1],
        "sandbox",
        "--hostname value should be 'sandbox'"
    );
    // Sysfs masks are present (actual set depends on host — macOS includes
    // all because /sys doesn't exist; Linux includes only existing paths).
    // On macOS CI all four are present.
    #[cfg(not(target_os = "linux"))]
    {
        assert!(args.contains(&"/sys/firmware:ro,nosuid".to_string()));
        assert!(args.contains(&"/sys/class/dmi:ro,nosuid".to_string()));
        assert!(args.contains(&"/sys/devices/virtual/dmi:ro,nosuid".to_string()));
        assert!(args.contains(&"/sys/class/block:ro,nosuid".to_string()));
    }
}

#[test]
fn test_docker_hardening_args_not_prebuilt() {
    let args = DockerSandbox::hardening_args(false, BackendKind::Docker, WorkspaceSysmount::Ro);
    assert!(args.contains(&"--cap-drop".to_string()));
    assert!(args.contains(&"ALL".to_string()));
    assert!(args.contains(&"--security-opt".to_string()));
    assert!(args.contains(&"no-new-privileges".to_string()));
    // --read-only must NOT be present for non-prebuilt (needs apt-get)
    assert!(!args.contains(&"--read-only".to_string()));
    // tmpfs mounts still present
    assert!(args.contains(&"/tmp:rw,nosuid,size=256m".to_string()));
    // Host metadata isolation still present — hostname
    let hostname_pos = args
        .iter()
        .position(|a| a == "--hostname")
        .expect("--hostname flag missing");
    assert_eq!(
        args[hostname_pos + 1],
        "sandbox",
        "--hostname value should be 'sandbox'"
    );
    #[cfg(not(target_os = "linux"))]
    {
        assert!(args.contains(&"/sys/firmware:ro,nosuid".to_string()));
        assert!(args.contains(&"/sys/class/dmi:ro,nosuid".to_string()));
        assert!(args.contains(&"/sys/devices/virtual/dmi:ro,nosuid".to_string()));
        assert!(args.contains(&"/sys/class/block:ro,nosuid".to_string()));
    }
}

#[test]
fn test_docker_hardening_args_podman() {
    let args = DockerSandbox::hardening_args(true, BackendKind::Podman, WorkspaceSysmount::Ro);
    // Core hardening flags must still be present
    assert!(args.contains(&"--cap-drop".to_string()));
    assert!(args.contains(&"ALL".to_string()));
    assert!(args.contains(&"--security-opt".to_string()));
    assert!(args.contains(&"no-new-privileges".to_string()));
    assert!(args.contains(&"--read-only".to_string()));
    assert!(args.contains(&"/tmp:rw,nosuid,size=256m".to_string()));
    assert!(args.contains(&"/run:rw,nosuid,size=64m".to_string()));
    let hostname_pos = args
        .iter()
        .position(|a| a == "--hostname")
        .expect("--hostname flag missing");
    assert_eq!(
        args[hostname_pos + 1],
        "sandbox",
        "--hostname value should be 'sandbox'"
    );
    // Sysfs tmpfs overlays must NOT be present — Podman's tmpcopyup breaks
    // these under --cap-drop ALL.
    assert!(!args.contains(&"/sys/firmware:ro,nosuid".to_string()));
    assert!(!args.contains(&"/sys/class/dmi:ro,nosuid".to_string()));
    assert!(!args.contains(&"/sys/devices/virtual/dmi:ro,nosuid".to_string()));
    assert!(!args.contains(&"/sys/class/block:ro,nosuid".to_string()));
}

#[test]
fn test_docker_hardening_args_prebuilt_rw_sysmount_skips_read_only() {
    let args = DockerSandbox::hardening_args(true, BackendKind::Docker, WorkspaceSysmount::Rw);
    assert!(!args.contains(&"--read-only".to_string()));
    assert!(!args.contains(&"--cap-drop".to_string()));
    assert!(!args.contains(&"ALL".to_string()));
    assert!(!args.contains(&"--security-opt".to_string()));
    assert!(!args.contains(&"no-new-privileges".to_string()));
}

#[test]
fn test_sysfs_paths_to_mask_no_sysfs_root_returns_all() {
    // When /sys doesn't exist (macOS), all paths should be returned because
    // Docker Desktop runs in a Linux VM with full sysfs.
    let paths = sysfs_paths_to_mask_from("/nonexistent/sysfs/root");
    assert_eq!(paths, vec![
        "/sys/firmware",
        "/sys/class/dmi",
        "/sys/devices/virtual/dmi",
        "/sys/class/block",
    ]);
}

#[test]
fn test_sysfs_paths_to_mask_filters_missing_paths() {
    // Simulate a Linux host where the sysfs root exists but specific
    // subtrees are missing (e.g. ARM without DMI, or WSL2).
    let dir = tempfile::tempdir().unwrap();
    let sysfs_root = dir.path().join("sys");
    // Create only /sys/firmware and /sys/class/block, skip DMI paths
    // (simulates Raspberry Pi / ARM which lacks DMI).
    std::fs::create_dir_all(sysfs_root.join("firmware")).unwrap();
    std::fs::create_dir_all(sysfs_root.join("class/block")).unwrap();

    let paths = sysfs_paths_to_mask_from(sysfs_root.to_str().unwrap());
    // Only the two paths that exist under the tempdir sysfs root are returned.
    assert_eq!(paths, vec!["/sys/firmware", "/sys/class/block"]);
}

#[test]
fn test_sysfs_mask_paths_constant_contains_expected_entries() {
    // Guard against accidentally removing paths from the constant.
    assert!(SYSFS_MASK_PATHS.contains(&"/sys/firmware"));
    assert!(SYSFS_MASK_PATHS.contains(&"/sys/class/dmi"));
    assert!(SYSFS_MASK_PATHS.contains(&"/sys/devices/virtual/dmi"));
    assert!(SYSFS_MASK_PATHS.contains(&"/sys/class/block"));
    assert_eq!(SYSFS_MASK_PATHS.len(), 4);
}

#[test]
fn test_workspace_sysmount_display() {
    assert_eq!(WorkspaceSysmount::Ro.to_string(), "ro");
    assert_eq!(WorkspaceSysmount::Rw.to_string(), "rw");
}

#[test]
fn test_home_persistence_display() {
    assert_eq!(HomePersistence::Off.to_string(), "off");
    assert_eq!(HomePersistence::Session.to_string(), "session");
    assert_eq!(HomePersistence::Shared.to_string(), "shared");
}

#[test]
fn test_mount_path_resolution_prefers_longest_prefix_and_enforces_ro() {
    let broad = tempfile::tempdir().unwrap();
    let nested = tempfile::tempdir().unwrap();
    std::fs::write(nested.path().join("note.txt"), "nested").unwrap();
    let mounts = vec![
        chelix_config::container_mounts::SandboxMount {
            host: broad.path().to_path_buf(),
            guest: "/mnt".into(),
            mode: chelix_config::container_mounts::MountMode::Rw,
        },
        chelix_config::container_mounts::SandboxMount {
            host: nested.path().to_path_buf(),
            guest: "/mnt/nested".into(),
            mode: chelix_config::container_mounts::MountMode::Ro,
        },
    ];

    assert_eq!(
        resolve_sandbox_mount_path(
            &mounts,
            std::path::Path::new("/mnt/nested/note.txt"),
            MountAccess::Read,
        ),
        Some(nested.path().join("note.txt"))
    );
    assert!(
        resolve_sandbox_mount_path(
            &mounts,
            std::path::Path::new("/mnt/nested/new.txt"),
            MountAccess::Write,
        )
        .is_none(),
        "a nested read-only mount must not fall back to the broader rw mount"
    );
}

#[test]
fn test_mount_path_resolution_uses_later_mount_for_equal_guest_depth() {
    let first = tempfile::tempdir().unwrap();
    let second = tempfile::tempdir().unwrap();
    std::fs::write(first.path().join("note.txt"), "first").unwrap();
    std::fs::write(second.path().join("note.txt"), "second").unwrap();
    let mounts = vec![
        chelix_config::container_mounts::SandboxMount {
            host: first.path().to_path_buf(),
            guest: "/mnt/shared".into(),
            mode: chelix_config::container_mounts::MountMode::Rw,
        },
        chelix_config::container_mounts::SandboxMount {
            host: second.path().to_path_buf(),
            guest: "/mnt/shared".into(),
            mode: chelix_config::container_mounts::MountMode::Ro,
        },
    ];

    assert_eq!(
        resolve_sandbox_mount_path(
            &mounts,
            std::path::Path::new("/mnt/shared/note.txt"),
            MountAccess::Read,
        ),
        Some(second.path().join("note.txt"))
    );
    assert!(
        resolve_sandbox_mount_path(
            &mounts,
            std::path::Path::new("/mnt/shared/new.txt"),
            MountAccess::Write,
        )
        .is_none(),
        "the later read-only mount must supersede the earlier rw mount"
    );
}

#[test]
fn test_mount_path_resolution_rejects_parent_escape() {
    let host = tempfile::tempdir().unwrap();
    let mounts = vec![chelix_config::container_mounts::SandboxMount {
        host: host.path().to_path_buf(),
        guest: "/mnt".into(),
        mode: chelix_config::container_mounts::MountMode::Rw,
    }];

    assert!(
        resolve_sandbox_mount_path(
            &mounts,
            std::path::Path::new("/mnt/../../etc/passwd"),
            MountAccess::Read,
        )
        .is_none()
    );
    assert!(
        resolve_sandbox_mount_path(
            &mounts,
            std::path::Path::new("/mnt/../outside"),
            MountAccess::Write,
        )
        .is_none()
    );
}

#[test]
fn test_resource_limits_default() {
    let limits = ResourceLimits::default();
    assert!(limits.memory_limit.is_none());
    assert!(limits.cpu_quota.is_none());
    assert!(limits.pids_max.is_none());
}

#[test]
fn test_resource_limits_serde() {
    let json = r#"{"memory_limit":"512M","cpu_quota":1.5,"pids_max":100}"#;
    let limits: ResourceLimits = serde_json::from_str(json).unwrap();
    assert_eq!(limits.memory_limit.as_deref(), Some("512M"));
    assert_eq!(limits.cpu_quota, Some(1.5));
    assert_eq!(limits.pids_max, Some(100));
}

#[test]
fn test_sandbox_config_serde() {
    let json = r#"{
        "mode": "all",
        "scope": "session",
        "workspace_sysmount": "rw",
        "network": "custom-net",
        "resource_limits": {"memory_limit": "1G"}
    }"#;
    let config: SandboxConfig = serde_json::from_str(json).unwrap();
    assert_eq!(config.mode, SandboxMode::All);
    assert_eq!(config.workspace_sysmount, WorkspaceSysmount::Rw);
    assert_eq!(config.network, "custom-net");
    assert_eq!(config.resource_limits.memory_limit.as_deref(), Some("1G"));
}

#[test]
fn test_docker_resource_args() {
    let config = SandboxConfig {
        resource_limits: ResourceLimits {
            memory_limit: Some("256M".into()),
            cpu_quota: Some(0.5),
            pids_max: Some(50),
        },
        ..Default::default()
    };
    let docker = DockerSandbox::new(config);
    let args = docker.resource_args();
    assert_eq!(args, vec![
        "--memory",
        "256M",
        "--cpus",
        "0.5",
        "--pids-limit",
        "50"
    ]);
}

fn mount_volume_for_guest<'a>(args: &'a [String], guest: &str) -> Option<&'a str> {
    let marker = format!(":{guest}:");
    args.chunks_exact(2)
        .find_map(|pair| (pair[0] == "-v" && pair[1].contains(&marker)).then_some(pair[1].as_str()))
}

fn test_sandbox_id() -> SandboxId {
    SandboxId {
        scope: SandboxScope::Session,
        key: "sess-1".into(),
    }
}

#[test]
fn test_docker_mount_args_include_data_dir_rw() {
    let temp_dir = tempfile::tempdir().unwrap();
    let host_data_dir = temp_dir.path().join("chelix-data");
    let docker = DockerSandbox::new(SandboxConfig {
        host_data_dir: Some(host_data_dir.clone()),
        ..Default::default()
    });
    let args = docker.mount_args(&test_sandbox_id()).unwrap();
    let guest_data_dir = chelix_config::data_dir();
    let volume = mount_volume_for_guest(&args, &guest_data_dir.display().to_string()).unwrap();

    assert_eq!(
        volume,
        format!(
            "{}:{}:rw",
            host_data_dir.display(),
            guest_data_dir.display()
        )
    );
}

#[test]
fn test_data_mount_exposes_agent_state_paths_in_sandbox_namespace() {
    let temp_dir = tempfile::tempdir().unwrap();
    let host_data_dir = temp_dir.path().join("chelix-data");
    for relative in ["memory", "logs", "skills", "sessions", "checkpoints"] {
        std::fs::create_dir_all(host_data_dir.join(relative)).unwrap();
    }
    for relative in ["AGENTS.md", "SOUL.md", "IDENTITY.md"] {
        std::fs::write(host_data_dir.join(relative), relative).unwrap();
    }

    let config = SandboxConfig {
        host_data_dir: Some(host_data_dir.clone()),
        home_persistence: HomePersistence::Off,
        ..Default::default()
    };
    let mounts = resolved_sandbox_mount_plan(&config, Some("docker"), &test_sandbox_id()).unwrap();
    let data_dir = chelix_config::data_dir();

    for relative in [
        "memory",
        "logs",
        "skills",
        "sessions",
        "checkpoints",
        "AGENTS.md",
        "SOUL.md",
        "IDENTITY.md",
    ] {
        assert_eq!(
            resolve_sandbox_mount_path(&mounts, &data_dir.join(relative), MountAccess::Read,),
            Some(host_data_dir.join(relative)),
            "{relative} must resolve through the mandatory data_dir mount"
        );
    }
    assert_eq!(
        resolve_sandbox_mount_path(&mounts, &data_dir.join("memory/new.md"), MountAccess::Write,),
        Some(host_data_dir.join("memory/new.md")),
        "the mandatory data_dir mount must remain writable"
    );

    if let Some(config_dir) = chelix_config::config_dir() {
        for secret in ["credentials.json", "provider_keys.json"] {
            assert!(
                resolve_sandbox_mount_path(&mounts, &config_dir.join(secret), MountAccess::Read,)
                    .is_none(),
                "config_dir secret {secret} must not resolve through a sandbox mount"
            );
        }
    }
}

#[test]
fn test_docker_mount_args_include_custom_declarative_mount() {
    let temp_dir = tempfile::tempdir().unwrap();
    let custom_host = temp_dir.path().join("reference");
    let custom_guest = PathBuf::from("/opt/reference");
    let docker = DockerSandbox::new(SandboxConfig {
        host_data_dir: Some(temp_dir.path().join("chelix-data")),
        mounts: vec![chelix_config::container_mounts::SandboxMount {
            host: custom_host.clone(),
            guest: custom_guest.clone(),
            mode: chelix_config::container_mounts::MountMode::Ro,
        }],
        ..Default::default()
    });
    let args = docker.mount_args(&test_sandbox_id()).unwrap();
    let expected_volume = format!("{}:{}:ro", custom_host.display(), custom_guest.display());

    assert_eq!(
        mount_volume_for_guest(&args, &custom_guest.display().to_string()),
        Some(expected_volume.as_str())
    );
}

#[test]
fn test_docker_hardening_args_enable_init_reaper() {
    let args = DockerSandbox::hardening_args(true, BackendKind::Docker, WorkspaceSysmount::Ro);
    assert!(
        args.contains(&"--init".to_string()),
        "Docker sandboxes must run with an init process so orphaned children are reaped"
    );
}

#[test]
fn test_podman_hardening_args_do_not_require_host_init_binary() {
    let args = DockerSandbox::hardening_args(true, BackendKind::Podman, WorkspaceSysmount::Ro);
    assert!(!args.contains(&"--init".to_string()));
}

#[test]
fn test_docker_mount_args_omit_home_when_persistence_is_off() {
    let temp_dir = tempfile::tempdir().unwrap();
    let docker = DockerSandbox::new(SandboxConfig {
        home_persistence: HomePersistence::Off,
        host_data_dir: Some(temp_dir.path().join("chelix-data")),
        ..Default::default()
    });
    let args = docker.mount_args(&test_sandbox_id()).unwrap();

    assert!(mount_volume_for_guest(&args, "/home/sandbox").is_none());
}

#[test]
fn test_docker_mount_args_include_default_shared_home() {
    let temp_dir = tempfile::tempdir().unwrap();
    let host_data_dir = temp_dir.path().join("chelix-data");
    let docker = DockerSandbox::new(SandboxConfig {
        host_data_dir: Some(host_data_dir.clone()),
        ..Default::default()
    });
    let args = docker.mount_args(&test_sandbox_id()).unwrap();
    let expected_host_dir = host_data_dir.join("sandbox/home/shared");
    let expected_volume = format!("{}:/home/sandbox:rw", expected_host_dir.display());

    assert_eq!(
        mount_volume_for_guest(&args, "/home/sandbox"),
        Some(expected_volume.as_str())
    );
}

#[test]
fn test_docker_mount_args_support_custom_shared_home() {
    let temp_dir = tempfile::tempdir().unwrap();
    let shared_home = temp_dir.path().join("custom-shared");
    let docker = DockerSandbox::new(SandboxConfig {
        host_data_dir: Some(temp_dir.path().join("chelix-data")),
        shared_home_dir: Some(shared_home.clone()),
        ..Default::default()
    });
    let args = docker.mount_args(&test_sandbox_id()).unwrap();
    let expected_volume = format!("{}:/home/sandbox:rw", shared_home.display());

    assert_eq!(
        mount_volume_for_guest(&args, "/home/sandbox"),
        Some(expected_volume.as_str())
    );
}

#[test]
fn test_docker_mount_args_append_sanitized_session_to_home() {
    let temp_dir = tempfile::tempdir().unwrap();
    let host_data_dir = temp_dir.path().join("chelix-data");
    let docker = DockerSandbox::new(SandboxConfig {
        home_persistence: HomePersistence::Session,
        host_data_dir: Some(host_data_dir.clone()),
        ..Default::default()
    });
    let id = SandboxId {
        scope: SandboxScope::Session,
        key: "sess:/weird key".into(),
    };
    let args = docker.mount_args(&id).unwrap();
    let expected_host_dir = host_data_dir.join("sandbox/home/session/sess--weird-key");
    let expected_volume = format!("{}:/home/sandbox:rw", expected_host_dir.display());

    assert_eq!(
        mount_volume_for_guest(&args, "/home/sandbox"),
        Some(expected_volume.as_str())
    );
}

const MISSING_OCI_TEST_CLI: &str = "/chelix-test-missing-oci-cli";

#[tokio::test]
async fn test_docker_read_file_uses_container_even_for_mounted_workspace_path() {
    let temp_dir = tempfile::tempdir().unwrap();
    let host_data_dir = temp_dir.path().join("chelix-data");
    let host_file = host_data_dir.join("notes/todo.txt");
    std::fs::create_dir_all(host_file.parent().unwrap()).unwrap();
    std::fs::write(&host_file, "docker mounted read").unwrap();

    let docker = DockerSandbox::with_cli(
        SandboxConfig {
            host_data_dir: Some(host_data_dir),
            ..Default::default()
        },
        MISSING_OCI_TEST_CLI,
    );
    let id = SandboxId {
        scope: SandboxScope::Session,
        key: "test-docker-read".into(),
    };
    let guest_file = chelix_config::data_dir().join("notes/todo.txt");

    let result = docker
        .read_file(&id, &guest_file.display().to_string(), 1024)
        .await;
    assert!(result.is_err(), "container access must be attempted");
}

#[tokio::test]
async fn test_docker_write_file_uses_container_even_for_mounted_workspace_path() {
    let temp_dir = tempfile::tempdir().unwrap();
    let host_data_dir = temp_dir.path().join("chelix-data");
    let docker = DockerSandbox::with_cli(
        SandboxConfig {
            host_data_dir: Some(host_data_dir.clone()),
            ..Default::default()
        },
        MISSING_OCI_TEST_CLI,
    );
    let id = SandboxId {
        scope: SandboxScope::Session,
        key: "test-docker-write".into(),
    };
    let guest_file = chelix_config::data_dir().join("notes/todo.txt");
    std::fs::create_dir_all(host_data_dir.join("notes")).unwrap();

    let result = docker
        .write_file(
            &id,
            &guest_file.display().to_string(),
            b"docker mounted write",
        )
        .await;
    assert!(result.is_err(), "container access must be attempted");
    assert!(!host_data_dir.join("notes/todo.txt").exists());
}

#[tokio::test]
async fn test_docker_write_file_uses_container_even_for_mounted_home_path() {
    let temp_dir = tempfile::tempdir().unwrap();
    let host_data_dir = temp_dir.path().join("chelix-data");
    let docker = DockerSandbox::with_cli(
        SandboxConfig {
            home_persistence: HomePersistence::Session,
            host_data_dir: Some(host_data_dir.clone()),
            ..Default::default()
        },
        MISSING_OCI_TEST_CLI,
    );
    let id = SandboxId {
        scope: SandboxScope::Session,
        key: "test-docker-home-write".into(),
    };
    let host_home = host_data_dir.join("sandbox/home/session/test-docker-home-write");
    std::fs::create_dir_all(&host_home).unwrap();

    let result = docker
        .write_file(&id, "/home/sandbox/todo.txt", b"docker home write")
        .await;

    assert!(result.is_err(), "container access must be attempted");
    assert!(!host_home.join("todo.txt").exists());
}

#[tokio::test]
async fn test_docker_list_files_uses_container_even_for_mounted_workspace_path() {
    let temp_dir = tempfile::tempdir().unwrap();
    let host_data_dir = temp_dir.path().join("chelix-data");
    let host_root = host_data_dir.join("notes");
    std::fs::create_dir_all(host_root.join("nested")).unwrap();
    std::fs::write(host_root.join("todo.txt"), "a").unwrap();
    std::fs::write(host_root.join("nested/done.txt"), "b").unwrap();

    let docker = DockerSandbox::with_cli(
        SandboxConfig {
            host_data_dir: Some(host_data_dir),
            ..Default::default()
        },
        MISSING_OCI_TEST_CLI,
    );
    let id = SandboxId {
        scope: SandboxScope::Session,
        key: "test-docker-list".into(),
    };
    let guest_root = chelix_config::data_dir().join("notes");

    let result = docker
        .list_files(&id, &guest_root.display().to_string())
        .await;
    assert!(result.is_err(), "container access must be attempted");
}

#[tokio::test]
async fn test_provisioning_guard_skips_second_call() {
    let docker = DockerSandbox::new(SandboxConfig::default());
    let name = "chelix-sandbox-test-guard";

    // First insertion succeeds.
    {
        let mut guard = docker.provisioned.lock().await;
        assert!(!guard.contains(name));
        guard.insert(name.to_string());
    }

    // Second check shows already provisioned.
    {
        let guard = docker.provisioned.lock().await;
        assert!(guard.contains(name));
    }
}

#[tokio::test]
async fn test_provisioning_guard_cleared_on_cleanup_entry() {
    let docker = DockerSandbox::new(SandboxConfig::default());
    let name = "chelix-sandbox-test-cleanup";

    // Mark as provisioned.
    docker.provisioned.lock().await.insert(name.to_string());
    assert!(docker.provisioned.lock().await.contains(name));

    // Simulate cleanup clearing the entry.
    docker.provisioned.lock().await.remove(name);
    assert!(!docker.provisioned.lock().await.contains(name));
}

#[tokio::test]
async fn test_provisioning_guard_independent_containers() {
    let docker = DockerSandbox::new(SandboxConfig::default());

    docker
        .provisioned
        .lock()
        .await
        .insert("container-a".to_string());

    let guard = docker.provisioned.lock().await;
    assert!(guard.contains("container-a"));
    assert!(!guard.contains("container-b"));
}

#[test]
fn test_podman_build_verifies_image_in_store() {
    // The Podman constructor must set `kind = BackendKind::Podman` so the
    // post-build verification branch in `build_image` activates.
    let sandbox = DockerSandbox::podman(SandboxConfig::default());
    assert_eq!(sandbox.kind, BackendKind::Podman);
    assert_eq!(sandbox.backend_name(), "podman");

    // Docker constructor must NOT be Podman.
    let docker = DockerSandbox::new(SandboxConfig::default());
    assert_eq!(docker.kind, BackendKind::Docker);
    assert_ne!(docker.kind, BackendKind::Podman);
}

#[test]
fn test_tail_lines_fewer_than_n() {
    let text = "line1\nline2\nline3";
    assert_eq!(tail_lines(text, 5), text);
}

#[test]
fn test_tail_lines_exact_n() {
    let text = "line1\nline2\nline3";
    assert_eq!(tail_lines(text, 3), text);
}

#[test]
fn test_tail_lines_more_than_n() {
    let text = "line1\nline2\nline3\nline4\nline5";
    let result = tail_lines(text, 2);
    assert!(result.starts_with("... [3 lines truncated]"));
    assert!(result.contains("line4\nline5"));
    assert!(!result.contains("line3"));
}

#[test]
fn test_tail_lines_empty() {
    assert_eq!(tail_lines("", 5), "");
}
