//! Path resolution, mount detection, and home persistence directories.

use std::{
    collections::HashMap,
    path::{Path as FsPath, PathBuf},
    sync::{Mutex, OnceLock},
};

use tracing::warn;

use {
    super::{
        containers::{is_cli_available, is_docker_daemon_available, should_use_docker_backend},
        types::{
            HomePersistence, SANDBOX_HOME_DIR, SandboxBackend, SandboxConfig, SandboxId,
            sanitize_path_component,
        },
    },
    crate::error::Result,
    chelix_config::container_mounts::{SandboxMount, sandbox_mount_plan},
};

pub(crate) static HOST_DATA_DIR_CACHE: OnceLock<Mutex<HashMap<String, PathBuf>>> = OnceLock::new();

pub(crate) fn configured_host_data_dir(config: &SandboxConfig) -> Option<PathBuf> {
    let guest_data_dir = chelix_config::data_dir();
    let path = config
        .host_data_dir
        .as_ref()
        .filter(|path| !path.as_os_str().is_empty())?;
    if path.is_absolute() {
        return Some(path.clone());
    }
    Some(guest_data_dir.join(path))
}

pub(crate) fn host_data_dir_cache() -> &'static Mutex<HashMap<String, PathBuf>> {
    HOST_DATA_DIR_CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

pub(crate) fn detect_host_data_dir(cli: &str, guest_data_dir: &FsPath) -> Option<PathBuf> {
    let cache_key = format!("{cli}:{}", guest_data_dir.display());
    {
        let guard = host_data_dir_cache()
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if let Some(cached) = guard.get(&cache_key) {
            return Some(cached.clone());
        }
    }

    let detected = chelix_config::container_mounts::detect_host_data_dir_with_references(
        cli,
        guest_data_dir,
        &chelix_config::container_mounts::current_container_references(),
    );

    if let Some(path) = detected.clone() {
        let mut guard = host_data_dir_cache()
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        guard.insert(cache_key, path);
    }
    detected
}

pub(crate) fn detected_container_cli(config: &SandboxConfig) -> Option<&'static str> {
    match config.backend {
        SandboxBackend::Docker => Some("docker"),
        SandboxBackend::Podman => Some("podman"),
        SandboxBackend::Auto => {
            if is_cli_available("podman") {
                Some("podman")
            } else if should_use_docker_backend(
                is_cli_available("docker"),
                is_docker_daemon_available(),
            ) || is_cli_available("docker")
            {
                Some("docker")
            } else {
                None
            }
        },
        SandboxBackend::AppleContainer => None,
    }
}

pub(crate) fn host_visible_data_dir(config: &SandboxConfig, cli: Option<&str>) -> PathBuf {
    let guest_data_dir = chelix_config::data_dir();
    if cli.is_none() {
        return guest_data_dir;
    }
    if let Some(configured) = configured_host_data_dir(config) {
        return configured;
    }
    if let Some(cli) = cli
        && let Some(detected) = detect_host_data_dir(cli, &guest_data_dir)
    {
        return detected;
    }
    guest_data_dir
}

pub(crate) fn host_visible_path(
    config: &SandboxConfig,
    cli: Option<&str>,
    path: &FsPath,
) -> PathBuf {
    let guest_data_dir = chelix_config::data_dir();
    let Ok(relative_path) = path.strip_prefix(&guest_data_dir) else {
        return path.to_path_buf();
    };
    let host_data_dir = host_visible_data_dir(config, cli);
    if relative_path.as_os_str().is_empty() {
        host_data_dir
    } else {
        host_data_dir.join(relative_path)
    }
}

/// Effective host path used when shared home persistence is enabled.
pub fn shared_home_dir_path(config: &SandboxConfig) -> PathBuf {
    let cli = detected_container_cli(config);
    let mut mount_config = mount_plan_config(config, cli.is_some());
    mount_config.home_persistence = chelix_config::schema::HomePersistenceConfig::Shared;
    sandbox_mount_plan(&mount_config)
        .into_iter()
        .find(|mount| mount.guest == FsPath::new(SANDBOX_HOME_DIR))
        .map(|mount| host_visible_mount_path(config, cli, &mount))
        .unwrap_or_else(|| host_visible_data_dir(config, cli))
}

pub(crate) fn sandbox_home_persistence_host_dir(
    config: &SandboxConfig,
    cli: Option<&str>,
    id: &SandboxId,
) -> Option<PathBuf> {
    resolved_sandbox_mount_plan(config, cli, id)
        .ok()?
        .into_iter()
        .find(|mount| mount.guest == FsPath::new(SANDBOX_HOME_DIR))
        .map(|mount| mount.host)
}

fn mount_plan_config(
    config: &SandboxConfig,
    include_host_data_dir: bool,
) -> chelix_config::schema::SandboxConfig {
    chelix_config::schema::SandboxConfig {
        host_data_dir: if include_host_data_dir {
            config
                .host_data_dir
                .as_ref()
                .map(|path| path.display().to_string())
        } else {
            None
        },
        home_persistence: match config.home_persistence {
            HomePersistence::Off => chelix_config::schema::HomePersistenceConfig::Off,
            HomePersistence::Session => chelix_config::schema::HomePersistenceConfig::Session,
            HomePersistence::Shared => chelix_config::schema::HomePersistenceConfig::Shared,
        },
        shared_home_dir: config
            .shared_home_dir
            .as_ref()
            .map(|path| path.display().to_string()),
        mounts: config.mounts.clone(),
        ..Default::default()
    }
}

fn host_visible_mount_path(
    config: &SandboxConfig,
    cli: Option<&str>,
    mount: &SandboxMount,
) -> PathBuf {
    let guest_data_dir = chelix_config::data_dir();
    if mount.guest == guest_data_dir {
        return host_visible_data_dir(config, cli);
    }

    if let Some(configured_data_dir) = configured_host_data_dir(config)
        && mount.host.starts_with(configured_data_dir)
    {
        return mount.host.clone();
    }

    host_visible_path(config, cli, &mount.host)
}

/// Resolve the declarative config plan into host-visible, session-specific mounts.
pub(crate) fn resolved_sandbox_mount_plan(
    config: &SandboxConfig,
    cli: Option<&str>,
    id: &SandboxId,
) -> Result<Vec<SandboxMount>> {
    let session_home = config.home_persistence == HomePersistence::Session;
    let mut mounts = sandbox_mount_plan(&mount_plan_config(config, cli.is_some()));

    for mount in &mut mounts {
        if session_home && mount.guest == FsPath::new(SANDBOX_HOME_DIR) {
            mount.host.push(sanitize_path_component(&id.key));
        }
        let guest_visible_path = mount.host.clone();
        mount.host = host_visible_mount_path(config, cli, mount);
        if mount.guest == FsPath::new(SANDBOX_HOME_DIR)
            && let Err(error) = std::fs::create_dir_all(&mount.host)
        {
            if guest_visible_path == mount.host {
                return Err(error.into());
            }
            warn!(
                path = %mount.host.display(),
                %error,
                "could not pre-create translated sandbox persistence path; runtime may create it"
            );
        }
    }

    Ok(mounts)
}
