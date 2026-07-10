//! Core types, enums, traits, and constants for the sandbox subsystem.

use std::path::PathBuf;

use {
    async_trait::async_trait,
    serde::{Deserialize, Serialize},
};

use crate::{
    command::{CommandOptions, CommandOutput},
    error::Result,
    sandbox::file_system::{
        SandboxGrepOptions, SandboxListFilesResult, SandboxReadResult, command_grep,
        command_list_files, command_read_file, command_write_file,
    },
    wasm_limits::WasmToolLimits,
};

pub(crate) fn truncate_output_for_display(output: &mut String, max_output_bytes: usize) {
    if output.len() <= max_output_bytes {
        return;
    }
    output.truncate(output.floor_char_boundary(max_output_bytes));
    output.push_str("\n... [output truncated]");
}

/// Return the last `n` lines of `text`, or the full text if it has fewer lines.
pub(crate) fn tail_lines(text: &str, n: usize) -> String {
    let lines: Vec<&str> = text.lines().collect();
    if lines.len() <= n {
        return text.to_string();
    }
    format!(
        "... [{} lines truncated]\n{}",
        lines.len() - n,
        lines[lines.len() - n..].join("\n")
    )
}

/// Default container image used when none is configured.
pub const DEFAULT_SANDBOX_IMAGE: &str = "ubuntu:26.04";

/// Sandbox mode controlling when sandboxing is applied.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
#[derive(Default)]
pub enum SandboxMode {
    Off,
    NonMain,
    #[default]
    All,
}

impl std::fmt::Display for SandboxMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Off => f.write_str("off"),
            Self::NonMain => f.write_str("non-main"),
            Self::All => f.write_str("all"),
        }
    }
}

/// Known sandbox backend identifiers.
///
/// Used in the API/gon layer for type-safe backend references. The config
/// schema uses a plain `String` for flexibility (TOML compatibility), but
/// this enum ensures wire-format consistency.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SandboxBackendId {
    Docker,
    Podman,
    AppleContainer,
    Cgroup,
    RestrictedHost,
    Wasm,
    None,
}

impl SandboxBackendId {
    /// Parse from backend_name() output.
    pub fn from_name(name: &str) -> Self {
        match name {
            "docker" => Self::Docker,
            "podman" => Self::Podman,
            "apple-container" => Self::AppleContainer,
            "cgroup" => Self::Cgroup,
            "restricted-host" => Self::RestrictedHost,
            "wasm" => Self::Wasm,
            _ => Self::None,
        }
    }
}

/// Scope determines container lifecycle boundaries.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
#[derive(Default)]
pub enum SandboxScope {
    #[default]
    Session,
    Agent,
    Shared,
}

impl std::fmt::Display for SandboxScope {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Session => f.write_str("session"),
            Self::Agent => f.write_str("agent"),
            Self::Shared => f.write_str("shared"),
        }
    }
}

/// Root filesystem and privilege-hardening mode for sandbox containers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
#[derive(Default)]
pub enum WorkspaceSysmount {
    #[default]
    Ro,
    Rw,
}

impl std::fmt::Display for WorkspaceSysmount {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Ro => f.write_str("ro"),
            Self::Rw => f.write_str("rw"),
        }
    }
}

/// Persistence mode for `/home/sandbox` in container backends.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum HomePersistence {
    Off,
    Session,
    #[default]
    Shared,
}

impl std::fmt::Display for HomePersistence {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Off => f.write_str("off"),
            Self::Session => f.write_str("session"),
            Self::Shared => f.write_str("shared"),
        }
    }
}

impl From<&chelix_config::schema::HomePersistenceConfig> for HomePersistence {
    fn from(value: &chelix_config::schema::HomePersistenceConfig) -> Self {
        match value {
            chelix_config::schema::HomePersistenceConfig::Off => Self::Off,
            chelix_config::schema::HomePersistenceConfig::Session => Self::Session,
            chelix_config::schema::HomePersistenceConfig::Shared => Self::Shared,
        }
    }
}

/// Resource limits for sandboxed execution.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ResourceLimits {
    /// Memory limit (e.g. "512M", "1G").
    pub memory_limit: Option<String>,
    /// CPU quota as a fraction (e.g. 0.5 = half a core, 2.0 = two cores).
    pub cpu_quota: Option<f64>,
    /// Maximum number of PIDs.
    pub pids_max: Option<u32>,
}

/// Configuration for sandbox behavior.
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct SandboxConfig {
    pub mode: SandboxMode,
    pub scope: SandboxScope,
    /// Root filesystem and privilege-hardening mode for sandbox containers.
    pub workspace_sysmount: WorkspaceSysmount,
    /// Host-visible path for Chelix `data_dir()` when running container-backed
    /// sandboxes from inside another container.
    pub host_data_dir: Option<PathBuf>,
    /// Persistence strategy for `/home/sandbox`.
    pub home_persistence: HomePersistence,
    /// Host directory used for shared `/home/sandbox` persistence.
    /// Relative paths are resolved against `data_dir()`.
    pub shared_home_dir: Option<PathBuf>,
    /// Additional declarative bind mounts copied from `[[sandbox.mounts]]`.
    pub mounts: Vec<chelix_config::container_mounts::SandboxMount>,
    pub image: Option<String>,
    pub container_prefix: Option<String>,
    /// Docker/Podman network name passed to `--network=<name>`.
    pub network: String,
    /// Backend: `"auto"` (default), `"docker"`, `"podman"`, `"apple-container"`,
    /// `"restricted-host"`, or `"wasm"`.
    /// `"auto"` prefers Apple Container on macOS, then Podman, then Docker, then restricted-host.
    pub backend: String,
    pub resource_limits: ResourceLimits,
    /// GPU device passthrough for Docker/Podman backends (e.g. "all", "device=0").
    pub gpus: Option<String>,
    /// Packages to install via `apt-get` after container creation.
    /// Set to an empty list to skip provisioning.
    pub packages: Vec<String>,
    /// IANA timezone (e.g. "Europe/Paris") injected as `TZ` env var into containers.
    pub timezone: Option<String>,
    /// Fuel limit for WASM sandbox execution (default: 1 billion instructions).
    pub wasm_fuel_limit: Option<u64>,
    /// Epoch interruption interval in milliseconds for WASM sandbox (default: 100ms).
    pub wasm_epoch_interval_ms: Option<u64>,
    /// Per-tool WASM limits (fuel/memory). Falls back to built-in defaults when absent.
    pub wasm_tool_limits: Option<WasmToolLimits>,
}

impl Default for SandboxConfig {
    fn default() -> Self {
        Self {
            mode: SandboxMode::default(),
            scope: SandboxScope::default(),
            workspace_sysmount: WorkspaceSysmount::default(),
            host_data_dir: None,
            home_persistence: HomePersistence::default(),
            shared_home_dir: None,
            mounts: Vec::new(),
            image: None,
            container_prefix: None,
            network: "bridge".into(),
            backend: "auto".into(),
            resource_limits: ResourceLimits::default(),
            gpus: None,
            packages: Vec::new(),
            timezone: None,
            wasm_fuel_limit: None,
            wasm_epoch_interval_ms: None,
            wasm_tool_limits: None,
        }
    }
}

impl From<&chelix_config::schema::SandboxConfig> for SandboxConfig {
    fn from(cfg: &chelix_config::schema::SandboxConfig) -> Self {
        Self {
            mode: match cfg.mode.as_str() {
                "all" => SandboxMode::All,
                "non-main" | "nonmain" => SandboxMode::NonMain,
                _ => SandboxMode::Off,
            },
            scope: match cfg.scope.as_str() {
                "agent" => SandboxScope::Agent,
                "shared" => SandboxScope::Shared,
                _ => SandboxScope::Session,
            },
            workspace_sysmount: match cfg.workspace_sysmount.as_str() {
                "rw" => WorkspaceSysmount::Rw,
                _ => WorkspaceSysmount::Ro,
            },
            host_data_dir: cfg
                .host_data_dir
                .as_deref()
                .map(str::trim)
                .filter(|p| !p.is_empty())
                .map(PathBuf::from),
            home_persistence: HomePersistence::from(&cfg.home_persistence),
            shared_home_dir: cfg
                .shared_home_dir
                .as_deref()
                .map(str::trim)
                .filter(|p| !p.is_empty())
                .map(PathBuf::from),
            mounts: cfg.mounts.clone(),
            image: cfg.image.clone(),
            container_prefix: cfg.container_prefix.clone(),
            network: normalize_container_network(&cfg.network),
            backend: cfg.backend.clone(),
            resource_limits: ResourceLimits {
                memory_limit: cfg.resource_limits.memory_limit.clone(),
                cpu_quota: cfg.resource_limits.cpu_quota,
                pids_max: cfg.resource_limits.pids_max,
            },
            gpus: cfg.gpus.clone(),
            packages: cfg.packages.clone(),
            timezone: None, // Set by gateway from user profile
            wasm_fuel_limit: cfg.wasm_fuel_limit,
            wasm_epoch_interval_ms: cfg.wasm_epoch_interval_ms,
            wasm_tool_limits: cfg.wasm_tool_limits.as_ref().map(WasmToolLimits::from),
        }
    }
}

fn normalize_container_network(network: &str) -> String {
    let trimmed = network.trim();
    if trimmed.is_empty() {
        "bridge".to_string()
    } else {
        trimmed.to_string()
    }
}

/// Sandbox identifier — session or agent scoped.
#[derive(Debug, Clone)]
pub struct SandboxId {
    pub scope: SandboxScope,
    pub key: String,
}

impl std::fmt::Display for SandboxId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?}/{}", self.scope, self.key)
    }
}

/// Result of a `build_image` call.
#[derive(Debug, Clone)]
pub struct BuildImageResult {
    /// The full image tag (e.g. `chelix-sandbox:abc123`).
    pub tag: String,
    /// Whether the build was actually performed (false = image already existed).
    pub built: bool,
}

/// Trait for sandbox implementations (Docker, cgroups, Apple Container, etc.).
#[async_trait]
pub trait Sandbox: Send + Sync {
    /// Human-readable backend name (e.g. "docker", "podman", "apple-container", "cgroup", "none").
    fn backend_name(&self) -> &'static str;

    /// Ensure the sandbox environment is ready (e.g., container started).
    /// If `image_override` is provided, use that image instead of the configured default.
    async fn ensure_ready(&self, id: &SandboxId, image_override: Option<&str>) -> Result<()>;

    /// Run a command inside the sandbox.
    async fn run_command(
        &self,
        id: &SandboxId,
        command: &str,
        opts: &CommandOptions,
    ) -> Result<CommandOutput>;

    /// Read a file inside the sandbox.
    async fn read_file(
        &self,
        id: &SandboxId,
        file_path: &str,
        max_bytes: u64,
    ) -> Result<SandboxReadResult> {
        command_read_file(self, id, file_path, max_bytes).await
    }

    /// Write a file inside the sandbox.
    async fn write_file(
        &self,
        id: &SandboxId,
        file_path: &str,
        content: &[u8],
    ) -> Result<Option<serde_json::Value>> {
        command_write_file(self, id, file_path, content).await
    }

    /// List regular files inside the sandbox.
    async fn list_files(&self, id: &SandboxId, root: &str) -> Result<SandboxListFilesResult> {
        command_list_files(self, id, root).await
    }

    /// Run grep inside the sandbox.
    async fn grep(&self, id: &SandboxId, opts: SandboxGrepOptions) -> Result<serde_json::Value> {
        command_grep(self, id, opts).await
    }

    /// Clean up sandbox resources.
    async fn cleanup(&self, id: &SandboxId) -> Result<()>;

    /// Whether this backend provides actual isolation.
    /// Returns `false` for `NoSandbox` (pass-through to host).
    fn is_real(&self) -> bool {
        true
    }

    /// Whether this backend provides filesystem isolation from the host.
    ///
    /// Defaults to `false` (fail-safe): new backends must explicitly opt in
    /// by returning `true`.  Container-based backends (Docker, Podman, Apple
    /// Container, WASM) override this to `true`.  Backends that only provide
    /// resource limits (restricted-host, cgroup) or no isolation (none) keep
    /// the default.
    ///
    /// Used by command execution to enforce approval gating and file-path
    /// restrictions when true filesystem isolation is unavailable.
    fn provides_fs_isolation(&self) -> bool {
        false
    }

    /// The default workspace/home directory inside this backend.
    ///
    /// Used by workspace sync to determine where to extract files.
    /// Defaults to `/home/sandbox`. Backends with a different internal
    /// workspace layout override this.
    fn workspace_dir(&self) -> &str {
        SANDBOX_HOME_DIR
    }

    /// Workspace directory for a specific prepared session.
    ///
    /// Most backends use a fixed directory and can rely on the default.
    /// Backends whose API returns a per-session project directory override
    /// this so workspace sync uses the same path as command execution.
    async fn workspace_dir_for(&self, _id: &SandboxId) -> String {
        self.workspace_dir().to_string()
    }

    /// Whether this backend manages an isolated filesystem that requires
    /// workspace sync (copy-in on setup, patch extraction on cleanup).
    ///
    /// Defaults to `false`. Local bind-mount backends (Docker, Podman, Apple
    /// Container) mount the host workspace directly. Backends that maintain a
    /// separate workspace copy return `true` so the host workspace can be
    /// synchronized in and out.
    fn is_isolated(&self) -> bool {
        false
    }

    /// Install packages inside the sandbox.
    ///
    /// Default implementation uses `apt-get` (Ubuntu/Debian). Backends with
    /// a different package manager override this method.
    ///
    /// Called once per session after `ensure_ready()` for isolated backends
    /// that don't have packages pre-baked into the image.
    async fn provision_packages(&self, id: &SandboxId, packages: &[String]) -> Result<()> {
        if packages.is_empty() {
            return Ok(());
        }
        let pkg_list = packages.join(" ");
        let cmd = format!(
            "apt-get update -qq && apt-get install -y -qq --no-install-recommends {pkg_list}"
        );
        let opts = CommandOptions {
            timeout: std::time::Duration::from_secs(600),
            ..Default::default()
        };
        let result = self.run_command(id, &cmd, &opts).await?;
        if result.exit_code != 0 {
            tracing::warn!(
                %id,
                exit_code = result.exit_code,
                stderr = result.stderr.trim(),
                "package provisioning failed (non-fatal)"
            );
        }
        Ok(())
    }

    /// Pre-build a container image with packages baked in.
    /// Returns `None` for backends that don't support image building.
    async fn build_image(
        &self,
        _base: &str,
        _packages: &[String],
    ) -> Result<Option<BuildImageResult>> {
        Ok(None)
    }
}

pub(crate) fn canonical_sandbox_packages(packages: &[String]) -> Vec<String> {
    let mut canonical: Vec<String> = packages
        .iter()
        .map(|p| p.trim())
        .filter(|p| !p.is_empty())
        .map(ToOwned::to_owned)
        .collect();
    canonical.sort();
    canonical.dedup();
    canonical
}

pub(crate) const SANDBOX_HOME_DIR: &str = "/home/sandbox";
pub(crate) const GOGCLI_MODULE_PATH: &str = "github.com/steipete/gogcli/cmd/gog";
pub(crate) const GOGCLI_VERSION: &str = "latest";

/// Additional Go-based CLI tools installed via `go install` in the sandbox image.
/// Each entry is `(module_path, version, binary_name)`.
///
/// Only tools that work inside a Linux container belong here. macOS-only tools
/// (e.g. wacrawl) are host-only and install via their skill's `requires.install`.
pub(crate) const GO_TOOL_INSTALLS: &[(&str, &str, &str)] = &[];
#[cfg(any(target_os = "macos", test))]
pub(crate) const APPLE_CONTAINER_SAFE_WORKDIR: &str = "/tmp";

pub(crate) fn sanitize_path_component(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for ch in input.chars() {
        if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.') {
            out.push(ch);
        } else {
            out.push('-');
        }
    }
    if out.is_empty() {
        "default".to_string()
    } else {
        out
    }
}
