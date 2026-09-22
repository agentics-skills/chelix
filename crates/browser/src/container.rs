//! Container management for sandboxed browser instances.
//!
//! Docker and Podman are selected from `[sandbox].backend`. Apple Container is
//! used only when that backend is explicitly configured.

use std::{
    fmt::Display,
    path::{Path, PathBuf},
    process::Command,
};

use {
    crate::error::Error,
    tracing::{debug, info, warn},
};

type Result<T> = std::result::Result<T, Error>;

trait ContextExt<T> {
    fn with_context<C, F>(self, f: F) -> Result<T>
    where
        C: Into<String>,
        F: FnOnce() -> C;
}

impl<T, E> ContextExt<T> for std::result::Result<T, E>
where
    E: Display,
{
    fn with_context<C, F>(self, f: F) -> Result<T>
    where
        C: Into<String>,
        F: FnOnce() -> C,
    {
        let context = f().into();
        self.map_err(|source| Error::LaunchFailed(format!("{context}: {source}")))
    }
}

impl<T> ContextExt<T> for Option<T> {
    fn with_context<C, F>(self, f: F) -> Result<T>
    where
        C: Into<String>,
        F: FnOnce() -> C,
    {
        self.ok_or_else(|| Error::LaunchFailed(f().into()))
    }
}

fn browser_container_name_prefix(container_prefix: &str) -> String {
    format!("{container_prefix}-")
}

fn new_browser_container_name(container_prefix: &str) -> String {
    format!(
        "{}{}",
        browser_container_name_prefix(container_prefix),
        uuid::Uuid::new_v4().as_simple()
    )
}

fn configured_host_data_dir(host_data_dir: Option<&Path>) -> Option<PathBuf> {
    let path = host_data_dir.filter(|path| !path.as_os_str().is_empty())?;
    if path.is_absolute() {
        return Some(path.to_path_buf());
    }
    warn!(
        path = %path.display(),
        "sandbox.host_data_dir is a relative path; it must be an absolute host-visible path, ignoring and falling back to auto-detection"
    );
    None
}

fn host_visible_data_dir_with_references(
    cli: &str,
    configured_data_dir: Option<&Path>,
    guest_data_dir: &Path,
    references: &[String],
) -> PathBuf {
    if let Some(configured) = configured_host_data_dir(configured_data_dir) {
        return configured;
    }
    chelix_config::container_mounts::detect_host_data_dir_with_references(
        cli,
        guest_data_dir,
        references,
    )
    .unwrap_or_else(|| guest_data_dir.to_path_buf())
}

fn host_visible_path_with_references(
    cli: &str,
    configured_data_dir: Option<&Path>,
    path: &Path,
    references: &[String],
) -> PathBuf {
    let guest_data_dir = chelix_config::data_dir();
    let Ok(relative_path) = path.strip_prefix(&guest_data_dir) else {
        return path.to_path_buf();
    };
    let host_data_dir = host_visible_data_dir_with_references(
        cli,
        configured_data_dir,
        &guest_data_dir,
        references,
    );
    if relative_path.as_os_str().is_empty() {
        host_data_dir
    } else {
        host_data_dir.join(relative_path)
    }
}

fn host_visible_path(cli: &str, configured_data_dir: Option<&Path>, path: &Path) -> PathBuf {
    host_visible_path_with_references(
        cli,
        configured_data_dir,
        path,
        &chelix_config::container_mounts::current_container_references(),
    )
}

fn profile_mount_dir_for_backend(
    backend: ContainerBackend,
    profile_dir: &Path,
    host_data_dir: Option<&Path>,
) -> PathBuf {
    match backend {
        ContainerBackend::Docker | ContainerBackend::Podman => {
            host_visible_path(backend.cli(), host_data_dir, profile_dir)
        },
        #[cfg(target_os = "macos")]
        ContainerBackend::AppleContainer => profile_dir.to_path_buf(),
    }
}

fn profile_precreate_dir<'a>(
    profile_dir: Option<&'a Path>,
    profile_mount_dir: Option<&Path>,
) -> Option<&'a Path> {
    let guest_dir = profile_dir?;
    let mount_dir = profile_mount_dir?;
    (guest_dir != mount_dir).then_some(guest_dir)
}

fn browser_profile_permission_hint(
    logs: Option<&str>,
    profile_mount_dir: Option<&Path>,
    host_data_dir: Option<&Path>,
) -> Option<String> {
    if configured_host_data_dir(host_data_dir).is_some() {
        return None;
    }
    let logs = logs?;
    if !logs.contains(CONTAINER_PROFILE_PATH)
        || !logs.contains("SingletonLock")
        || !logs.contains("Permission denied")
    {
        return None;
    }
    let mount_dir = profile_mount_dir?;
    Some(format!(
        "Chrome could not write its browser profile at {CONTAINER_PROFILE_PATH}; Chelix mounted `{}` from the host. When Chelix runs inside Docker with the Docker socket mounted, add `host_data_dir = \"/absolute/host/path/to/chelix-data\"` under `[sandbox]` in chelix.toml to the host-visible path backing Chelix data, then restart Chelix",
        mount_dir.display()
    ))
}

fn launch_error_with_hint(error: Error, hint: String) -> Error {
    match error {
        Error::LaunchFailed(message) => Error::LaunchFailed(format!("{message}; {hint}")),
        other => Error::LaunchFailed(format!("{other}; {hint}")),
    }
}

fn ensure_profile_dir(dir: &Path) {
    match std::fs::create_dir_all(dir) {
        Ok(()) => set_container_dir_permissions(dir),
        Err(error) => warn!(
            path = %dir.display(),
            %error,
            "could not pre-create browser profile path; runtime may create it"
        ),
    }
}

#[cfg(unix)]
fn set_container_dir_permissions(dir: &Path) {
    use std::os::unix::fs::PermissionsExt;
    if let Err(error) = std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o777)) {
        warn!(
            path = %dir.display(),
            %error,
            "failed to set browser profile directory permissions"
        );
    }
}

#[cfg(not(unix))]
fn set_container_dir_permissions(_dir: &Path) {}

/// Container backend type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContainerBackend {
    Docker,
    Podman,
    #[cfg(target_os = "macos")]
    AppleContainer,
}

impl ContainerBackend {
    /// Get the CLI command name for this backend.
    fn cli(&self) -> &'static str {
        match self {
            Self::Docker => "docker",
            Self::Podman => "podman",
            #[cfg(target_os = "macos")]
            Self::AppleContainer => "container",
        }
    }

    /// Check if this backend is available.
    fn is_available(&self) -> bool {
        is_cli_available(self.cli())
    }
}

fn stop_container_by_id(backend: ContainerBackend, container_id: &str) {
    let cli = backend.cli();
    let result = Command::new(cli).args(["stop", container_id]).output();

    match result {
        Ok(output) if output.status.success() => {
            debug!(container_id, backend = cli, "browser container stopped");
        },
        Ok(output) => {
            let stderr = String::from_utf8_lossy(&output.stderr);
            warn!(
                container_id,
                backend = cli,
                error = %stderr.trim(),
                "failed to stop browser container"
            );
        },
        Err(e) => {
            warn!(
                container_id,
                backend = cli,
                error = %e,
                "failed to run {} stop",
                cli
            );
        },
    }

    // Containers are started without --rm so that logs and status remain
    // available for diagnostics after a crash.  Explicitly remove the
    // container after stopping it.
    let mut rm_args = vec!["rm".to_string()];
    if matches!(backend, ContainerBackend::Docker | ContainerBackend::Podman) {
        rm_args.push("-fv".to_string());
    }
    rm_args.push(container_id.to_string());
    match Command::new(cli).args(&rm_args).output() {
        Ok(output) if output.status.success() => {
            debug!(container_id, backend = cli, "browser container removed");
        },
        Ok(output) => {
            let stderr = String::from_utf8_lossy(&output.stderr);
            warn!(
                container_id,
                backend = cli,
                error = %stderr.trim(),
                "failed to remove browser container"
            );
        },
        Err(e) => {
            warn!(
                container_id,
                backend = cli,
                error = %e,
                "failed to run {} rm",
                cli
            );
        },
    }
}

/// A running browser container instance.
pub struct BrowserContainer {
    /// Container ID or name.
    container_id: String,
    /// Host port mapped to the container's CDP port.
    host_port: u16,
    /// Hostname or IP used to connect to the container.
    host: String,
    /// The image used.
    #[allow(dead_code)]
    image: String,
    /// The container backend being used.
    backend: ContainerBackend,
}

impl BrowserContainer {
    /// Start a new browser container with the configured sandbox backend.
    ///
    /// When `profile_dir` is `Some`, the host directory is mounted into the
    /// container so that browser profile data persists across sessions.
    pub fn start(
        image: &str,
        container_prefix: &str,
        viewport_width: u32,
        viewport_height: u32,
        low_memory_threshold_mb: u64,
        session_timeout_ms: u64,
        profile_dir: Option<&Path>,
        host_data_dir: Option<&Path>,
        network: &str,
        backend: chelix_config::schema::SandboxBackend,
    ) -> Result<Self> {
        let backend = resolve_container_backend(backend)?;
        Self::start_with_backend(
            backend,
            image,
            container_prefix,
            viewport_width,
            viewport_height,
            low_memory_threshold_mb,
            session_timeout_ms,
            profile_dir,
            host_data_dir,
            network,
        )
    }

    /// Start a new browser container with a specific backend.
    pub fn start_with_backend(
        backend: ContainerBackend,
        image: &str,
        container_prefix: &str,
        viewport_width: u32,
        viewport_height: u32,
        low_memory_threshold_mb: u64,
        session_timeout_ms: u64,
        profile_dir: Option<&Path>,
        host_data_dir: Option<&Path>,
        network: &str,
    ) -> Result<Self> {
        use std::time::Instant;

        if !backend.is_available() {
            return Err(Error::LaunchFailed(format!(
                "{} is not available. Please install it to use sandboxed browser.",
                backend.cli()
            )));
        }

        info!(
            image,
            backend = backend.cli(),
            network,
            "starting browser container"
        );

        let t0 = Instant::now();
        let profile_mount_dir =
            profile_dir.map(|dir| profile_mount_dir_for_backend(backend, dir, host_data_dir));

        if let Some(guest_dir) = profile_precreate_dir(profile_dir, profile_mount_dir.as_deref()) {
            ensure_profile_dir(guest_dir);
        }

        let (container_id, endpoint) = match backend {
            ContainerBackend::Docker | ContainerBackend::Podman => {
                let container_id = start_oci_container(
                    backend,
                    image,
                    container_prefix,
                    network,
                    viewport_width,
                    viewport_height,
                    low_memory_threshold_mb,
                    session_timeout_ms,
                    profile_mount_dir.as_deref(),
                )?;
                finish_browser_start(
                    backend,
                    container_id,
                    profile_mount_dir.as_deref(),
                    host_data_dir,
                    t0,
                )?
            },
            #[cfg(target_os = "macos")]
            ContainerBackend::AppleContainer => {
                let host_port = find_available_port()?;
                let container_id = start_apple_container(
                    image,
                    container_prefix,
                    host_port,
                    viewport_width,
                    viewport_height,
                    low_memory_threshold_mb,
                    session_timeout_ms,
                    profile_mount_dir.as_deref(),
                )?;
                let candidates = vec![BrowserEndpoint {
                    host: "127.0.0.1".to_string(),
                    port: host_port,
                }];
                finish_browser_start_with_candidates(
                    backend,
                    container_id,
                    candidates,
                    profile_mount_dir.as_deref(),
                    host_data_dir,
                    t0,
                )?
            },
        };

        Ok(Self {
            container_id,
            host_port: endpoint.port,
            host: endpoint.host,
            image: image.to_string(),
            backend,
        })
    }

    /// Get the WebSocket URL for CDP connection.
    #[must_use]
    pub fn websocket_url(&self) -> String {
        // browserless/chrome provides a direct WebSocket endpoint
        format!("ws://{}:{}", self.host, self.host_port)
    }

    /// Get the HTTP URL for health checks.
    #[must_use]
    pub fn http_url(&self) -> String {
        format!("http://{}:{}", self.host, self.host_port)
    }

    /// Stop and remove the container.
    pub fn stop(&self) {
        info!(
            container_id = %self.container_id,
            backend = self.backend.cli(),
            "stopping browser container"
        );
        stop_container_by_id(self.backend, &self.container_id);
    }

    /// Get the container ID.
    #[must_use]
    pub fn id(&self) -> &str {
        &self.container_id
    }

    /// Get the backend being used.
    #[must_use]
    pub fn backend(&self) -> ContainerBackend {
        self.backend
    }
}

impl Drop for BrowserContainer {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Path inside the container where the browser profile is mounted.
const CONTAINER_PROFILE_PATH: &str = "/data/browser-profile";

/// Build the `DEFAULT_LAUNCH_ARGS` env-var value for containerised Chrome.
///
/// Always includes `--window-size`; appends low-memory flags when the host
/// system RAM is below the given threshold. Adds `--user-data-dir` when a
/// container-side profile path is provided.
fn build_container_launch_args(
    viewport_width: u32,
    viewport_height: u32,
    low_memory_threshold_mb: u64,
    container_profile_dir: Option<&str>,
    backend: ContainerBackend,
) -> String {
    use crate::pool::low_memory_chrome_args;

    let mut args = vec![format!("--window-size={viewport_width},{viewport_height}")];

    if let Some(profile_dir) = container_profile_dir {
        args.push(format!("--user-data-dir={profile_dir}"));
    }

    // Apple Container VMs may not provide /dev/shm reliably; tell Chrome to
    // write shared-memory segments to /tmp instead.
    #[cfg(target_os = "macos")]
    if backend == ContainerBackend::AppleContainer {
        args.push("--disable-dev-shm-usage".to_string());
    }
    #[cfg(not(target_os = "macos"))]
    let _ = backend;

    if low_memory_threshold_mb > 0 {
        let mut sys = sysinfo::System::new();
        sys.refresh_memory();
        let total_mb = sys.total_memory() / (1024 * 1024);
        for flag in low_memory_chrome_args(total_mb, low_memory_threshold_mb) {
            args.push((*flag).to_string());
        }
    }

    let joined = args
        .iter()
        .map(|a| format!("\"{a}\""))
        .collect::<Vec<_>>()
        .join(",");
    format!("DEFAULT_LAUNCH_ARGS=[{joined}]")
}

/// Compute the browserless container `TIMEOUT` (in ms) from pool lifecycle settings.
///
/// The result is `max(idle_timeout_secs, max_instance_lifetime_secs)` converted
/// to milliseconds, then floored against `navigation_timeout_ms` so that a single
/// long navigation cannot exceed the container's own timeout. The final value is
/// capped at `max_instance_lifetime_secs * 1000` to prevent disagree­ment with the
/// Chelix-side hard TTL when `navigation_timeout_ms` is very large.
pub(crate) fn browserless_session_timeout_ms(
    idle_timeout_secs: u64,
    navigation_timeout_ms: u64,
    max_instance_lifetime_secs: u64,
) -> u64 {
    let ceiling_ms = max_instance_lifetime_secs.saturating_mul(1000);
    idle_timeout_secs
        .max(max_instance_lifetime_secs)
        .saturating_mul(1000)
        .max(navigation_timeout_ms)
        .min(ceiling_ms)
}

fn browserless_container_env(session_timeout_ms: u64) -> Vec<String> {
    vec![
        format!("TIMEOUT={session_timeout_ms}"),
        "MAX_CONCURRENT_SESSIONS=1".to_string(),
        "PREBOOT_CHROME=true".to_string(),
    ]
}

const BROWSER_CONTAINER_PORT: u16 = 3000;

#[derive(Clone)]
struct BrowserEndpoint {
    host: String,
    port: u16,
}

fn finish_browser_start(
    backend: ContainerBackend,
    container_id: String,
    profile_mount_dir: Option<&Path>,
    host_data_dir: Option<&Path>,
    started: std::time::Instant,
) -> Result<(String, BrowserEndpoint)> {
    let candidates = match browser_endpoint_candidates(backend, &container_id) {
        Ok(candidates) => candidates,
        Err(error) => {
            return Err(fail_browser_container(
                backend,
                &container_id,
                profile_mount_dir,
                host_data_dir,
                error,
            ));
        },
    };
    finish_browser_start_with_candidates(
        backend,
        container_id,
        candidates,
        profile_mount_dir,
        host_data_dir,
        started,
    )
}

fn finish_browser_start_with_candidates(
    backend: ContainerBackend,
    container_id: String,
    candidates: Vec<BrowserEndpoint>,
    profile_mount_dir: Option<&Path>,
    host_data_dir: Option<&Path>,
    started: std::time::Instant,
) -> Result<(String, BrowserEndpoint)> {
    info!(
        container_id,
        candidates = candidates.len(),
        backend = backend.cli(),
        elapsed_ms = started.elapsed().as_millis() as u64,
        "browser container process started, waiting for Chrome readiness"
    );
    match wait_for_ready(&candidates) {
        Ok(endpoint) => {
            info!(
                container_id,
                host = %endpoint.host,
                host_port = endpoint.port,
                backend = backend.cli(),
                total_startup_ms = started.elapsed().as_millis() as u64,
                "browser container ready"
            );
            Ok((container_id, endpoint))
        },
        Err(error) => Err(fail_browser_container(
            backend,
            &container_id,
            profile_mount_dir,
            host_data_dir,
            error,
        )),
    }
}

fn fail_browser_container(
    backend: ContainerBackend,
    container_id: &str,
    profile_mount_dir: Option<&Path>,
    host_data_dir: Option<&Path>,
    error: Error,
) -> Error {
    let container_logs = fetch_container_logs(backend, container_id);
    let container_status = inspect_container_status(backend, container_id);
    warn!(
        container_id,
        backend = backend.cli(),
        error = %error,
        "browser container failed readiness check, cleaning up"
    );
    if let Some(ref status) = container_status {
        warn!(
            container_id,
            container_status = status,
            "browser container status at time of failure"
        );
    }
    if let Some(ref logs) = container_logs {
        warn!(container_id, logs = %logs, "browser container logs");
    } else {
        warn!(container_id, "no container logs available");
    }
    let permission_hint = browser_profile_permission_hint(
        container_logs.as_deref(),
        profile_mount_dir,
        host_data_dir,
    );
    if let Some(ref hint) = permission_hint {
        warn!(
            container_id,
            hint, "browser profile mount permission failure detected"
        );
    }
    stop_container_by_id(backend, container_id);
    if let Some(hint) = permission_hint {
        launch_error_with_hint(error, hint)
    } else {
        error
    }
}

fn browser_endpoint_candidates(
    backend: ContainerBackend,
    name: &str,
) -> Result<Vec<BrowserEndpoint>> {
    let cli = backend.cli();
    let published_output = Command::new(cli)
        .args(["port", name, &format!("{BROWSER_CONTAINER_PORT}/tcp")])
        .output()
        .with_context(|| format!("failed to run {cli} port"))?;
    let inspect_output = Command::new(cli)
        .args([
            "inspect",
            "--format",
            "{{range .NetworkSettings.Networks}}{{println .IPAddress}}{{end}}",
            name,
        ])
        .output()
        .with_context(|| format!("failed to run {cli} inspect"))?;
    let published = if published_output.status.success() {
        String::from_utf8_lossy(&published_output.stdout).into_owned()
    } else {
        String::new()
    };
    let addresses = if inspect_output.status.success() {
        String::from_utf8_lossy(&inspect_output.stdout).into_owned()
    } else {
        String::new()
    };
    let candidates = endpoints_from_port_and_inspect(&published, &addresses);
    if candidates.is_empty() {
        let published_error = String::from_utf8_lossy(&published_output.stderr);
        let inspect_error = String::from_utf8_lossy(&inspect_output.stderr);
        return Err(Error::LaunchFailed(format!(
            "{cli} returned no browser endpoint candidates for container {name}; port error: {}; inspect error: {}",
            published_error.trim(),
            inspect_error.trim()
        )));
    }
    Ok(candidates)
}

fn endpoints_from_port_and_inspect(published: &str, inspect_output: &str) -> Vec<BrowserEndpoint> {
    let mut endpoints = Vec::new();
    if let Some(port) = parse_published_port(published) {
        endpoints.push(BrowserEndpoint {
            host: "127.0.0.1".to_string(),
            port,
        });
    }
    for address in parse_container_addresses(inspect_output) {
        let host = match address {
            std::net::IpAddr::V4(address) => address.to_string(),
            std::net::IpAddr::V6(address) => format!("[{address}]"),
        };
        endpoints.push(BrowserEndpoint {
            host,
            port: BROWSER_CONTAINER_PORT,
        });
    }
    endpoints.dedup_by(|left, right| left.host == right.host && left.port == right.port);
    endpoints
}

fn parse_published_port(output: &str) -> Option<u16> {
    output.lines().find_map(|line| {
        line.trim()
            .rsplit_once(':')
            .and_then(|(_, port)| port.parse().ok())
    })
}

fn parse_container_addresses(output: &str) -> Vec<std::net::IpAddr> {
    output
        .lines()
        .filter_map(|line| line.trim().parse().ok())
        .filter(|address: &std::net::IpAddr| !address.is_unspecified())
        .collect()
}

/// Start a Docker or Podman container for the browser.
fn start_oci_container(
    backend: ContainerBackend,
    image: &str,
    container_prefix: &str,
    network: &str,
    viewport_width: u32,
    viewport_height: u32,
    low_memory_threshold_mb: u64,
    session_timeout_ms: u64,
    profile_dir: Option<&Path>,
) -> Result<String> {
    let cli = backend.cli();
    let container_name = new_browser_container_name(container_prefix);

    let container_profile_dir = profile_dir.map(|_| CONTAINER_PROFILE_PATH);
    let launch_args = build_container_launch_args(
        viewport_width,
        viewport_height,
        low_memory_threshold_mb,
        container_profile_dir,
        backend,
    );
    let browserless_env = browserless_container_env(session_timeout_ms);

    let mut run_args = vec![
        "run".to_string(),
        "-d".to_string(),
        "--name".to_string(),
        container_name.clone(),
        format!("--network={network}"),
        "-p".to_string(),
        format!("127.0.0.1::{BROWSER_CONTAINER_PORT}"),
        "-e".to_string(),
        launch_args,
        "--shm-size=2gb".to_string(),
    ];

    for env in browserless_env {
        run_args.push("-e".to_string());
        run_args.push(env);
    }

    // Mount the profile directory if persistence is enabled
    if let Some(host_path) = profile_dir {
        run_args.push("-v".to_string());
        run_args.push(format!(
            "{}:{}:rw",
            host_path.display(),
            CONTAINER_PROFILE_PATH
        ));
    }

    run_args.push(image.to_string());

    info!(
        backend = cli,
        args = %run_args.join(" "),
        "browser container run command"
    );

    let output = Command::new(cli)
        .args(&run_args)
        .output()
        .with_context(|| format!("failed to run {cli} command"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let launch_error = stderr.trim().to_string();
        match Command::new(cli)
            .args(["rm", "-fv", &container_name])
            .output()
        {
            Ok(cleanup) if cleanup.status.success() => {},
            Ok(cleanup) => {
                let cleanup_error = String::from_utf8_lossy(&cleanup.stderr);
                warn!(
                    container_name,
                    cli,
                    error = %cleanup_error.trim(),
                    "failed to remove browser container after run failure"
                );
            },
            Err(error) => {
                warn!(
                    container_name,
                    cli,
                    %error,
                    "failed to remove browser container after run failure"
                );
            },
        }
        return Err(Error::LaunchFailed(format!(
            "failed to start {cli} container: {launch_error}"
        )));
    }

    if container_name.is_empty() {
        return Err(Error::LaunchFailed(format!(
            "{cli} container name is empty"
        )));
    }

    Ok(container_name)
}

/// Start an Apple Container for the browser.
#[cfg(target_os = "macos")]
fn start_apple_container(
    image: &str,
    container_prefix: &str,
    host_port: u16,
    viewport_width: u32,
    viewport_height: u32,
    low_memory_threshold_mb: u64,
    session_timeout_ms: u64,
    profile_dir: Option<&Path>,
) -> Result<String> {
    let container_name = new_browser_container_name(container_prefix);

    let container_profile_dir = profile_dir.map(|_| CONTAINER_PROFILE_PATH);
    let launch_args = build_container_launch_args(
        viewport_width,
        viewport_height,
        low_memory_threshold_mb,
        container_profile_dir,
        ContainerBackend::AppleContainer,
    );
    let browserless_env = browserless_container_env(session_timeout_ms);

    let mut container_args = vec![
        "run".to_string(),
        "-d".to_string(),
        "--name".to_string(),
        container_name.clone(),
        "-p".to_string(),
        format!("{}:3000", host_port),
        "-e".to_string(),
        launch_args,
        // Chrome requires shared memory for rendering; Docker uses --shm-size=2gb,
        // Apple Container doesn't support --shm-size so mount tmpfs at /dev/shm.
        "--tmpfs".to_string(),
        "/dev/shm".to_string(),
    ];

    for env in browserless_env {
        container_args.push("-e".to_string());
        container_args.push(env);
    }

    // Mount the profile directory if persistence is enabled
    if let Some(host_path) = profile_dir {
        container_args.push("-v".to_string());
        container_args.push(format!(
            "{}:{}",
            host_path.display(),
            CONTAINER_PROFILE_PATH
        ));
    }

    container_args.push(image.to_string());

    let output = Command::new("container")
        .args(&container_args)
        .output()
        .with_context(|| "failed to run container command")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(Error::LaunchFailed(format!(
            "failed to start apple container: {}",
            stderr.trim()
        )));
    }

    Ok(container_name)
}

/// Check if a CLI tool is available.
fn is_cli_available(name: &str) -> bool {
    Command::new(name)
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

/// Find an available TCP port.
#[cfg(any(target_os = "macos", test))]
fn find_available_port() -> Result<u16> {
    let listener = std::net::TcpListener::bind("127.0.0.1:0")
        .with_context(|| "failed to bind to ephemeral port")?;

    let port = listener
        .local_addr()
        .with_context(|| "failed to get local address")?
        .port();

    drop(listener);
    Ok(port)
}

/// Fetch the last logs from a container for diagnostic purposes.
fn fetch_container_logs(backend: ContainerBackend, container_id: &str) -> Option<String> {
    // Apple Container CLI may not support `logs --tail`
    #[cfg(target_os = "macos")]
    if backend == ContainerBackend::AppleContainer {
        return None;
    }

    let cli = backend.cli();
    let output = Command::new(cli)
        .args(["logs", "--tail", "50", container_id])
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    // Browserless/Chrome may log to either stdout or stderr
    let combined = format!("{stdout}{stderr}");
    let trimmed = combined.trim().to_string();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed)
    }
}

/// Inspect a container's status (running, exited, etc.) for diagnostics.
fn inspect_container_status(backend: ContainerBackend, container_id: &str) -> Option<String> {
    let cli = backend.cli();

    #[cfg(target_os = "macos")]
    if backend == ContainerBackend::AppleContainer {
        // Apple Container doesn't support `inspect --format`
        return None;
    }

    let output = Command::new(cli)
        .args([
            "inspect",
            "--format",
            "{{.State.Status}} (ExitCode={{.State.ExitCode}}, OOMKilled={{.State.OOMKilled}})",
            container_id,
        ])
        .output()
        .ok()?;

    if output.status.success() {
        let status = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if status.is_empty() {
            None
        } else {
            Some(status)
        }
    } else {
        None
    }
}

/// Wait for the container to be ready by probing the Chrome DevTools endpoint.
///
/// TCP connectivity alone isn't sufficient - Chrome inside the container may accept
/// connections before it's ready to handle WebSocket requests. We probe `/json/version`
/// which browserless exposes when Chrome is truly ready.
fn wait_for_ready(candidates: &[BrowserEndpoint]) -> Result<BrowserEndpoint> {
    use std::time::{Duration, Instant};

    let timeout = Duration::from_secs(60);
    let start = Instant::now();
    let mut attempts: u32 = 0;
    let mut last_error = String::new();

    info!(
        candidates = candidates.len(),
        timeout_secs = 60,
        "waiting for browser container Chrome readiness"
    );

    loop {
        let elapsed = start.elapsed();
        if elapsed > timeout {
            warn!(
                attempts,
                elapsed_ms = elapsed.as_millis() as u64,
                "browser container failed to become ready within {}s",
                timeout.as_secs()
            );
            return Err(Error::LaunchFailed(format!(
                "browser container failed to become ready within {}s ({} probe attempts): {last_error}",
                timeout.as_secs(),
                attempts
            )));
        }

        attempts += 1;
        let mut round_errors = Vec::new();
        for candidate in candidates {
            match probe_http_endpoint(&candidate.host, candidate.port) {
                Ok(true) => {
                    info!(
                        attempts,
                        host = %candidate.host,
                        port = candidate.port,
                        elapsed_ms = elapsed.as_millis() as u64,
                        "browser container Chrome endpoint is ready"
                    );
                    return Ok(candidate.clone());
                },
                Ok(false) => {
                    round_errors.push(format!("{}:{} not ready", candidate.host, candidate.port));
                },
                Err(error) => {
                    round_errors.push(format!("{}:{}: {error}", candidate.host, candidate.port));
                },
            }
        }
        last_error = round_errors.join("; ");
        if attempts.is_multiple_of(10) {
            info!(
                attempts,
                elapsed_ms = elapsed.as_millis() as u64,
                error = %last_error,
                "Chrome endpoint not ready yet, still probing"
            );
        } else {
            debug!(attempts, error = %last_error, "Chrome endpoint not ready yet, retrying");
        }

        std::thread::sleep(Duration::from_millis(500));
    }
}

/// Probe the Chrome /json/version endpoint to check if it's ready.
fn probe_http_endpoint(host: &str, port: u16) -> Result<bool> {
    use std::{
        io::{BufRead, BufReader, Write},
        net::{TcpStream, ToSocketAddrs},
        time::Duration,
    };

    let addr = format!("{}:{}", host, port);
    let socket_addr = addr
        .to_socket_addrs()
        .map_err(|e| Error::LaunchFailed(format!("failed to resolve {addr}: {e}")))?
        .next()
        .ok_or_else(|| Error::LaunchFailed(format!("no addresses resolved for {addr}")))?;
    let mut stream = TcpStream::connect_timeout(&socket_addr, Duration::from_secs(2))?;
    stream.set_read_timeout(Some(Duration::from_secs(2)))?;
    stream.set_write_timeout(Some(Duration::from_secs(2)))?;

    // Send minimal HTTP request
    let request =
        format!("GET /json/version HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n\r\n");
    stream.write_all(request.as_bytes())?;

    // Read response status line
    let mut reader = BufReader::new(stream);
    let mut status_line = String::new();
    reader.read_line(&mut status_line)?;

    let ready = status_line.contains("200");
    debug!(
        port,
        status_line = status_line.trim(),
        ready,
        "probe response"
    );

    Ok(ready)
}

/// Check if Docker is available.
#[must_use]
pub fn is_docker_available() -> bool {
    is_cli_available("docker")
}

/// Report whether the configured sandbox backend CLI is installed.
#[must_use]
pub fn configured_backend_available(backend: chelix_config::schema::SandboxBackend) -> bool {
    match backend {
        chelix_config::schema::SandboxBackend::Docker => is_cli_available("docker"),
        chelix_config::schema::SandboxBackend::Podman => is_cli_available("podman"),
        chelix_config::schema::SandboxBackend::AppleContainer => is_cli_available("container"),
    }
}

fn resolve_container_backend(
    backend: chelix_config::schema::SandboxBackend,
) -> Result<ContainerBackend> {
    if !configured_backend_available(backend) {
        return Err(Error::LaunchFailed(format!(
            "{} is not available. Please install it to use sandboxed browser.",
            backend.as_str()
        )));
    }
    match backend {
        chelix_config::schema::SandboxBackend::Docker => Ok(ContainerBackend::Docker),
        chelix_config::schema::SandboxBackend::Podman => Ok(ContainerBackend::Podman),
        chelix_config::schema::SandboxBackend::AppleContainer => {
            #[cfg(target_os = "macos")]
            {
                Ok(ContainerBackend::AppleContainer)
            }
            #[cfg(not(target_os = "macos"))]
            {
                Err(Error::LaunchFailed(
                    "Apple Container sandbox is only available on macOS".to_string(),
                ))
            }
        },
    }
}

fn parse_docker_container_names(output: &[u8], container_prefix: &str) -> Vec<String> {
    let name_prefix = browser_container_name_prefix(container_prefix);
    String::from_utf8_lossy(output)
        .lines()
        .map(str::trim)
        .filter(|name| name.starts_with(&name_prefix))
        .map(str::to_string)
        .collect()
}

#[cfg(target_os = "macos")]
#[derive(serde::Deserialize)]
struct AppleContainerListEntry {
    configuration: AppleContainerConfig,
}

#[cfg(target_os = "macos")]
#[derive(serde::Deserialize)]
struct AppleContainerConfig {
    id: String,
}

#[cfg(target_os = "macos")]
fn parse_apple_container_names(output: &[u8]) -> Result<Vec<String>> {
    let entries: Vec<AppleContainerListEntry> = serde_json::from_slice(output)
        .with_context(|| "failed to parse apple container list JSON")?;
    Ok(entries
        .into_iter()
        .map(|entry| entry.configuration.id)
        .collect())
}

#[cfg(target_os = "macos")]
fn parse_apple_container_names_for_prefix(
    output: &[u8],
    container_prefix: &str,
) -> Result<Vec<String>> {
    let name_prefix = browser_container_name_prefix(container_prefix);
    Ok(parse_apple_container_names(output)?
        .into_iter()
        .filter(|name| name.starts_with(&name_prefix))
        .collect())
}

fn cleanup_stale_oci_browser_containers(cli: &str, container_prefix: &str) -> Result<usize> {
    if !is_cli_available(cli) {
        return Ok(0);
    }

    let output = Command::new(cli)
        .args(["ps", "-a", "--format", "{{.Names}}"])
        .output()
        .with_context(|| format!("failed to list {cli} containers"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(Error::LaunchFailed(format!(
            "{cli} ps failed while cleaning stale browser containers: {}",
            stderr.trim()
        )));
    }

    let names = parse_docker_container_names(&output.stdout, container_prefix);
    let mut removed = 0usize;
    for name in names {
        let rm = Command::new(cli)
            .args(["rm", "-fv", &name])
            .output()
            .with_context(|| format!("failed to remove stale {cli} browser container {name}"))?;
        if rm.status.success() {
            removed += 1;
        } else {
            let stderr = String::from_utf8_lossy(&rm.stderr);
            warn!(
                container_name = %name,
                cli,
                error = %stderr.trim(),
                "failed to remove stale browser container"
            );
        }
    }

    Ok(removed)
}

#[cfg(target_os = "macos")]
fn cleanup_stale_apple_browser_containers(container_prefix: &str) -> Result<usize> {
    if !is_cli_available("container") {
        return Ok(0);
    }

    let output = Command::new("container")
        .args(["list", "--all", "--format", "json"])
        .output()
        .with_context(|| "failed to list apple containers")?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(Error::LaunchFailed(format!(
            "container list failed while cleaning stale browser containers: {}",
            stderr.trim()
        )));
    }

    let names = parse_apple_container_names_for_prefix(&output.stdout, container_prefix)?;
    let mut removed = 0usize;
    for name in names {
        let rm = Command::new("container")
            .args(["delete", "--force", &name])
            .output()
            .with_context(|| format!("failed to remove stale apple browser container {name}"))?;
        if rm.status.success() {
            removed += 1;
        } else {
            let stderr = String::from_utf8_lossy(&rm.stderr);
            warn!(
                container_name = %name,
                error = %stderr.trim(),
                "failed to remove stale apple browser container"
            );
        }
    }

    Ok(removed)
}

#[cfg(target_os = "macos")]
fn cleanup_stale_apple_browser_containers_for_current_platform(
    container_prefix: &str,
) -> Result<usize> {
    cleanup_stale_apple_browser_containers(container_prefix)
}

#[cfg(not(target_os = "macos"))]
fn cleanup_stale_apple_browser_containers_for_current_platform(
    _container_prefix: &str,
) -> Result<usize> {
    Ok(0)
}

/// Remove stale browser containers left behind by previous runs.
///
/// Browser containers are named with an instance-specific prefix so startup can
/// clean up orphaned instances before creating new ones.
pub fn cleanup_stale_browser_containers(
    container_prefix: &str,
    backend: chelix_config::schema::SandboxBackend,
) -> Result<usize> {
    let removed = match backend {
        chelix_config::schema::SandboxBackend::Docker => {
            cleanup_stale_oci_browser_containers("docker", container_prefix)?
        },
        chelix_config::schema::SandboxBackend::Podman => {
            cleanup_stale_oci_browser_containers("podman", container_prefix)?
        },
        chelix_config::schema::SandboxBackend::AppleContainer => {
            cleanup_stale_apple_browser_containers_for_current_platform(container_prefix)?
        },
    };
    Ok(removed)
}

#[allow(clippy::unwrap_used, clippy::expect_used)]
#[cfg(test)]
mod tests;
