//! Sandbox orchestration: backend selection, failover, routing.

use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
    time::Duration,
};

use {
    tokio::sync::RwLock,
    tracing::{info, warn},
};

#[cfg(any(target_os = "macos", test))]
use {async_trait::async_trait, tracing::debug};

#[cfg(target_os = "macos")]
use super::apple::{AppleContainerSandbox, ensure_apple_container_service};
#[cfg(feature = "wasm")]
use super::wasm::WasmSandbox;
use {
    super::{
        containers::{is_cli_available, is_docker_daemon_available, should_use_docker_backend},
        docker::{DockerSandbox, NoSandbox},
        env::ExecEnv,
        types::{
            DEFAULT_SANDBOX_IMAGE, Sandbox, SandboxBackend, SandboxBackendId, SandboxConfig,
            SandboxId, SandboxMode, SharedSandboxImage, shared_sandbox_image,
        },
    },
    crate::error::{Error, Result},
};

#[cfg(any(target_os = "macos", test))]
use {
    super::{
        containers::is_apple_container_corruption_error,
        file_system::{SandboxGrepOptions, SandboxListFilesResult, SandboxReadResult},
        types::{BuildImageResult, ToolsServiceEndpoint},
    },
    crate::command::{CommandOptions, CommandOutput},
};

/// Wrapper sandbox that can fail over from a primary backend to a fallback backend.
///
/// This is used on macOS to fail over from Apple Container to Docker when the
/// Apple runtime enters a corrupted state (stale metadata, missing config.json,
/// service errors, etc.).
#[cfg(any(target_os = "macos", test))]
pub(crate) struct FailoverSandbox {
    primary: Arc<dyn Sandbox>,
    fallback: Arc<dyn Sandbox>,
    primary_backend: SandboxBackendId,
    fallback_backend: SandboxBackendId,
    use_fallback: RwLock<bool>,
}

#[cfg(any(target_os = "macos", test))]
impl FailoverSandbox {
    pub(crate) fn new(primary: Arc<dyn Sandbox>, fallback: Arc<dyn Sandbox>) -> Result<Self> {
        if !primary.provides_fs_isolation() || !fallback.provides_fs_isolation() {
            return Err(Error::message(
                "sandbox failover requires filesystem-isolated primary and fallback backends",
            ));
        }
        let primary_backend = primary.backend_id();
        let fallback_backend = fallback.backend_id();
        Ok(Self {
            primary,
            fallback,
            primary_backend,
            fallback_backend,
            use_fallback: RwLock::new(false),
        })
    }

    async fn fallback_enabled(&self) -> bool {
        *self.use_fallback.read().await
    }

    async fn switch_to_fallback(&self, error: &Error) {
        let mut use_fallback = self.use_fallback.write().await;
        if !*use_fallback {
            warn!(
                primary = %self.primary_backend,
                fallback = %self.fallback_backend,
                %error,
                "sandbox primary backend failed, switching to fallback backend"
            );
            *use_fallback = true;
        }
    }

    fn should_failover(&self, error: &Error) -> bool {
        let message = format!("{error:#}");
        match self.primary_backend {
            SandboxBackendId::AppleContainer => is_apple_container_corruption_error(&message),
            SandboxBackendId::Docker => is_docker_failover_error(&message),
            SandboxBackendId::Podman => is_podman_failover_error(&message),
            SandboxBackendId::Wasm | SandboxBackendId::None => false,
        }
    }
}

#[cfg(any(target_os = "macos", test))]
#[async_trait]
impl Sandbox for FailoverSandbox {
    fn backend_id(&self) -> SandboxBackendId {
        // Report the active backend so callers know the true isolation level.
        // On lock contention (write lock held during failover switch),
        // conservatively assume fallback is active — the safer default.
        if self
            .use_fallback
            .try_read()
            .map(|guard| *guard)
            .unwrap_or(true)
        {
            self.fallback_backend
        } else {
            self.primary_backend
        }
    }

    fn provides_fs_isolation(&self) -> bool {
        // On lock contention, conservatively report the fallback's (weaker)
        // isolation level rather than the primary's.
        if self
            .use_fallback
            .try_read()
            .map(|guard| *guard)
            .unwrap_or(true)
        {
            self.fallback.provides_fs_isolation()
        } else {
            self.primary.provides_fs_isolation()
        }
    }

    fn workspace_dir(&self) -> &str {
        if self
            .use_fallback
            .try_read()
            .map(|guard| *guard)
            .unwrap_or(true)
        {
            self.fallback.workspace_dir()
        } else {
            self.primary.workspace_dir()
        }
    }

    async fn workspace_dir_for(&self, id: &SandboxId) -> String {
        if self.fallback_enabled().await {
            self.fallback.workspace_dir_for(id).await
        } else {
            self.primary.workspace_dir_for(id).await
        }
    }

    fn is_isolated(&self) -> bool {
        if self
            .use_fallback
            .try_read()
            .map(|guard| *guard)
            .unwrap_or(true)
        {
            self.fallback.is_isolated()
        } else {
            self.primary.is_isolated()
        }
    }

    async fn ensure_ready(&self, id: &SandboxId) -> Result<()> {
        if self.fallback_enabled().await {
            return self.fallback.ensure_ready(id).await;
        }

        match self.primary.ensure_ready(id).await {
            Ok(()) => Ok(()),
            Err(primary_error) => {
                if !self.should_failover(&primary_error) {
                    return Err(primary_error);
                }

                self.switch_to_fallback(&primary_error).await;
                let primary_message = format!("{primary_error:#}");
                self.fallback
                    .ensure_ready(id)
                    .await
                    .map_err(|fallback_error| {
                        Error::message(format!(
                            "primary sandbox backend ({}) failed: {}; fallback backend ({}) also failed: {}",
                            self.primary_backend,
                            primary_message,
                            self.fallback_backend,
                            fallback_error
                        ))
                    })
            },
        }
    }

    async fn tools_service_endpoint(&self, id: &SandboxId) -> Result<ToolsServiceEndpoint> {
        if self.fallback_enabled().await {
            self.fallback.tools_service_endpoint(id).await
        } else {
            self.primary.tools_service_endpoint(id).await
        }
    }

    async fn run_command(
        &self,
        id: &SandboxId,
        command: &str,
        opts: &CommandOptions,
    ) -> Result<CommandOutput> {
        if self.fallback_enabled().await {
            return self.fallback.run_command(id, command, opts).await;
        }

        match self.primary.run_command(id, command, opts).await {
            Ok(result) => Ok(result),
            Err(primary_error) => {
                if !self.should_failover(&primary_error) {
                    return Err(primary_error);
                }

                self.switch_to_fallback(&primary_error).await;
                let primary_message = format!("{primary_error:#}");
                self.fallback
                    .ensure_ready(id)
                    .await
                    .map_err(|fallback_error| {
                        Error::message(format!(
                            "primary sandbox backend ({}) failed during command execution: {}; fallback backend ({}) failed to initialize: {}",
                            self.primary_backend,
                            primary_message,
                            self.fallback_backend,
                            fallback_error
                        ))
                    })?;
                self.fallback.run_command(id, command, opts).await
            },
        }
    }

    async fn cleanup(&self, id: &SandboxId) -> Result<()> {
        if self.fallback_enabled().await {
            let result = self.fallback.cleanup(id).await;
            if let Err(error) = self.primary.cleanup(id).await {
                debug!(
                    backend = %self.primary_backend,
                    %error,
                    "primary sandbox cleanup failed after failover"
                );
            }
            return result;
        }

        self.primary.cleanup(id).await
    }

    // Delegate file operations to the active backend's own implementations.

    async fn read_file(
        &self,
        id: &SandboxId,
        file_path: &str,
        max_bytes: u64,
    ) -> Result<SandboxReadResult> {
        if self.fallback_enabled().await {
            return self.fallback.read_file(id, file_path, max_bytes).await;
        }
        self.primary.read_file(id, file_path, max_bytes).await
    }

    async fn write_file(
        &self,
        id: &SandboxId,
        file_path: &str,
        content: &[u8],
    ) -> Result<Option<serde_json::Value>> {
        if self.fallback_enabled().await {
            return self.fallback.write_file(id, file_path, content).await;
        }
        self.primary.write_file(id, file_path, content).await
    }

    async fn list_files(&self, id: &SandboxId, root: &str) -> Result<SandboxListFilesResult> {
        if self.fallback_enabled().await {
            return self.fallback.list_files(id, root).await;
        }
        self.primary.list_files(id, root).await
    }

    async fn grep(&self, id: &SandboxId, opts: SandboxGrepOptions) -> Result<serde_json::Value> {
        if self.fallback_enabled().await {
            return self.fallback.grep(id, opts).await;
        }
        self.primary.grep(id, opts).await
    }

    async fn build_image(
        &self,
        base: &str,
        packages: &[String],
    ) -> Result<Option<BuildImageResult>> {
        if self.fallback_enabled().await {
            return self.fallback.build_image(base, packages).await;
        }

        let primary_result = match self.primary.build_image(base, packages).await {
            Ok(result) => result,
            Err(primary_error) => {
                if !self.should_failover(&primary_error) {
                    return Err(primary_error);
                }

                self.switch_to_fallback(&primary_error).await;
                return self.fallback.build_image(base, packages).await;
            },
        };

        let fallback_result = self.fallback.build_image(base, packages).await?;
        match (primary_result, fallback_result) {
            (Some(mut primary), Some(fallback)) => {
                if primary.tag != fallback.tag {
                    return Err(Error::message(format!(
                        "sandbox failover backends produced different deterministic image tags: primary={} fallback={}",
                        primary.tag, fallback.tag
                    )));
                }
                primary.built |= fallback.built;
                Ok(Some(primary))
            },
            (Some(primary), None) => Ok(Some(primary)),
            (None, Some(fallback)) => Ok(Some(fallback)),
            (None, None) => Ok(None),
        }
    }
}

/// Create the appropriate sandbox backend based on config and platform.
pub fn create_sandbox(config: SandboxConfig) -> Result<Arc<dyn Sandbox>> {
    let effective_image = shared_sandbox_image(&config);
    create_sandbox_with_global_image(config, effective_image)
}

fn create_sandbox_with_global_image(
    config: SandboxConfig,
    effective_image: SharedSandboxImage,
) -> Result<Arc<dyn Sandbox>> {
    if config.mode == SandboxMode::Off {
        return Ok(Arc::new(NoSandbox));
    }

    select_backend_with_global_image(config, effective_image)
}

/// Select the sandbox backend based on config and platform availability.
///
/// When `backend` is `"auto"` (the default):
/// - On macOS, prefer Apple Container if the `container` CLI is installed
///   (each sandbox runs in a lightweight VM — stronger isolation than Docker).
/// - Prefer Podman (daemonless, rootless) over Docker when available.
/// - Fall back to Docker, then fail closed when no isolated runtime is available.
#[cfg(test)]
pub(crate) fn select_backend(config: SandboxConfig) -> Result<Arc<dyn Sandbox>> {
    let effective_image = shared_sandbox_image(&config);
    select_backend_with_global_image(config, effective_image)
}

fn select_backend_with_global_image(
    config: SandboxConfig,
    effective_image: SharedSandboxImage,
) -> Result<Arc<dyn Sandbox>> {
    match config.backend {
        SandboxBackend::Auto => auto_detect_backend_with_global_image(config, effective_image),
        SandboxBackend::Docker => {
            if !should_use_docker_backend(is_cli_available("docker"), is_docker_daemon_available())
            {
                return Err(Error::message(
                    "Docker sandbox requested but the Docker daemon is unavailable",
                ));
            }
            Ok(Arc::new(DockerSandbox::new_with_global_image(
                config,
                effective_image,
            )))
        },
        SandboxBackend::Podman => {
            if !is_cli_available("podman") {
                return Err(Error::message(
                    "Podman sandbox requested but the podman CLI is unavailable",
                ));
            }
            Ok(Arc::new(DockerSandbox::podman_with_global_image(
                config,
                effective_image,
            )))
        },
        SandboxBackend::AppleContainer => create_apple_backend(config, effective_image),
        SandboxBackend::Wasm => create_wasm_backend(config),
    }
}

#[cfg(target_os = "macos")]
fn create_apple_backend(
    config: SandboxConfig,
    effective_image: SharedSandboxImage,
) -> Result<Arc<dyn Sandbox>> {
    if !is_cli_available("container") || !ensure_apple_container_service() {
        return Err(Error::message(
            "Apple Container sandbox requested but the container runtime is unavailable",
        ));
    }
    Ok(Arc::new(AppleContainerSandbox::new_with_global_image(
        config,
        effective_image,
    )))
}

#[cfg(not(target_os = "macos"))]
fn create_apple_backend(
    _config: SandboxConfig,
    _effective_image: SharedSandboxImage,
) -> Result<Arc<dyn Sandbox>> {
    Err(Error::message(
        "Apple Container sandbox is only available on macOS",
    ))
}

/// Create a WASM sandbox backend and fail closed when unavailable.
fn create_wasm_backend(config: SandboxConfig) -> Result<Arc<dyn Sandbox>> {
    #[cfg(feature = "wasm")]
    {
        let sandbox = WasmSandbox::new(config).map_err(|error| {
            Error::message(format!("failed to initialize WASM sandbox: {error}"))
        })?;
        tracing::info!("sandbox backend: wasm (WASI-isolated execution)");
        Ok(Arc::new(sandbox))
    }
    #[cfg(not(feature = "wasm"))]
    {
        let _ = config;
        Err(Error::message(
            "WASM sandbox requested but the wasm feature is not compiled in",
        ))
    }
}

/// Wrap a primary sandbox backend with a failover chain.
///
/// Tries Podman, then Docker as isolated fallbacks, returning the primary
/// unwrapped if no fallback runtime is available.
#[cfg(target_os = "macos")]
fn maybe_wrap_with_failover(
    primary: Arc<dyn Sandbox>,
    config: &SandboxConfig,
    effective_image: SharedSandboxImage,
) -> Result<Arc<dyn Sandbox>> {
    let primary_backend = primary.backend_id();

    // Try Podman as fallback (skip if primary is already Podman).
    if primary_backend != SandboxBackendId::Podman && is_cli_available("podman") {
        tracing::info!(
            primary = %primary_backend,
            fallback = "podman",
            "sandbox backend failover enabled"
        );
        return Ok(Arc::new(FailoverSandbox::new(
            primary,
            Arc::new(DockerSandbox::podman_with_global_image(
                config.clone(),
                effective_image,
            )),
        )?));
    }

    // Try Docker as fallback (skip if primary is already Docker).
    if primary_backend != SandboxBackendId::Docker
        && should_use_docker_backend(is_cli_available("docker"), is_docker_daemon_available())
    {
        tracing::info!(
            primary = %primary_backend,
            fallback = "docker",
            "sandbox backend failover enabled"
        );
        return Ok(Arc::new(FailoverSandbox::new(
            primary,
            Arc::new(DockerSandbox::new_with_global_image(
                config.clone(),
                effective_image,
            )),
        )?));
    }

    Ok(primary)
}

/// Check whether an error message indicates a Docker daemon connectivity issue.
#[cfg(any(target_os = "macos", test))]
pub(crate) fn is_docker_failover_error(message: &str) -> bool {
    let lower = message.to_ascii_lowercase();
    lower.contains("cannot connect to the docker daemon")
        || lower.contains("is the docker daemon running")
        || lower.contains("error during connect")
        || lower.contains("connection refused")
}

/// Check whether an error message indicates a Podman runtime issue that warrants
/// failover. Podman is daemonless so most Docker-daemon errors don't apply, but
/// socket/service errors or missing runtimes do.
#[cfg(any(target_os = "macos", test))]
pub(crate) fn is_podman_failover_error(message: &str) -> bool {
    let lower = message.to_ascii_lowercase();
    lower.contains("cannot connect to podman")
        || lower.contains("no such file or directory") && lower.contains("podman")
        || lower.contains("connection refused")
        || lower.contains("runtime") && lower.contains("not found")
}

pub fn auto_detect_backend(config: SandboxConfig) -> Result<Arc<dyn Sandbox>> {
    let effective_image = shared_sandbox_image(&config);
    auto_detect_backend_with_global_image(config, effective_image)
}

fn auto_detect_backend_with_global_image(
    config: SandboxConfig,
    effective_image: SharedSandboxImage,
) -> Result<Arc<dyn Sandbox>> {
    #[cfg(target_os = "macos")]
    {
        if is_cli_available("container") {
            if ensure_apple_container_service() {
                tracing::info!("sandbox backend: apple-container (VM-isolated, preferred)");
                let apple_backend: Arc<dyn Sandbox> =
                    Arc::new(AppleContainerSandbox::new_with_global_image(
                        config.clone(),
                        Arc::clone(&effective_image),
                    ));
                return maybe_wrap_with_failover(apple_backend, &config, effective_image);
            }
            tracing::warn!(
                "apple container CLI found but service could not be started; \
                 falling back to podman/docker"
            );
        }
    }

    // Prefer Podman (daemonless, rootless by default) over Docker.
    if is_cli_available("podman") {
        tracing::info!("sandbox backend: podman (daemonless, preferred over docker)");
        return Ok(Arc::new(DockerSandbox::podman_with_global_image(
            config,
            effective_image,
        )));
    }

    if should_use_docker_backend(is_cli_available("docker"), is_docker_daemon_available()) {
        tracing::info!("sandbox backend: docker");
        return Ok(Arc::new(DockerSandbox::new_with_global_image(
            config,
            effective_image,
        )));
    }

    if is_cli_available("docker") {
        tracing::warn!("docker CLI detected but daemon is not accessible");
    }

    Err(Error::message(
        "sandbox mode is On, but no isolated runtime is available; install or start Apple Container, Podman, or Docker, or configure the WASM backend",
    ))
}

/// Events emitted by the sandbox subsystem for UI feedback.
#[derive(Debug, Clone)]
pub enum SandboxEvent {
    /// First-run container/image setup is about to begin for a session.
    Preparing {
        session_key: String,
        backend: SandboxBackendId,
        image: String,
    },
    /// First-run container/image setup completed for a session.
    Prepared {
        session_key: String,
        backend: SandboxBackendId,
        image: String,
    },
    /// First-run container/image setup failed for a session.
    PrepareFailed {
        session_key: String,
        backend: SandboxBackendId,
        image: String,
        error: String,
    },
    /// Package provisioning started (Apple Container per-container install).
    Provisioning {
        container: String,
        packages: Vec<String>,
    },
    /// Package provisioning finished.
    Provisioned { container: String },
    /// Package provisioning failed (non-fatal).
    ProvisionFailed { container: String, error: String },
}

/// Routes every session according to the single global `[sandbox]` policy.
pub struct SandboxRouter {
    config: SandboxConfig,
    backend: Arc<dyn Sandbox>,
    /// Single effective image shared by the router and every OCI failover backend.
    effective_image: SharedSandboxImage,
    /// Event channel for sandbox lifecycle events (prepare/provision/build feedback).
    event_tx: tokio::sync::broadcast::Sender<SandboxEvent>,
    /// Session keys that have already completed sandbox initialization.
    /// Used to avoid repeating first-run preparation banners on every command.
    prepared_sessions: RwLock<HashSet<String>>,
    /// Session keys where workspace sync-in has completed.
    /// Subsequent command calls wait until sync_in finishes before proceeding.
    synced_sessions: RwLock<HashSet<String>>,
    /// Per-session first-run failures that should unblock waiters without
    /// allowing them to run against an incomplete sandbox workspace.
    sync_failures: RwLock<HashMap<String, String>>,
}

impl SandboxRouter {
    pub fn new(config: SandboxConfig) -> Result<Self> {
        let effective_image = shared_sandbox_image(&config);
        let backend =
            create_sandbox_with_global_image(config.clone(), Arc::clone(&effective_image))?;
        let (event_tx, _) = tokio::sync::broadcast::channel(32);
        Ok(Self {
            config,
            backend,
            effective_image,
            event_tx,
            prepared_sessions: RwLock::new(HashSet::new()),
            synced_sessions: RwLock::new(HashSet::new()),
            sync_failures: RwLock::new(HashMap::new()),
        })
    }

    /// Create the canonical router for explicit global host execution.
    #[must_use]
    pub fn disabled() -> Self {
        Self::with_backend(
            SandboxConfig {
                mode: SandboxMode::Off,
                ..SandboxConfig::default()
            },
            Arc::new(NoSandbox),
        )
    }

    /// Create a router with a custom sandbox backend (useful for testing).
    pub fn with_backend(config: SandboxConfig, backend: Arc<dyn Sandbox>) -> Self {
        let effective_image = shared_sandbox_image(&config);
        let (event_tx, _) = tokio::sync::broadcast::channel(32);
        Self {
            config,
            backend,
            effective_image,
            event_tx,
            prepared_sessions: RwLock::new(HashSet::new()),
            synced_sessions: RwLock::new(HashSet::new()),
            sync_failures: RwLock::new(HashMap::new()),
        }
    }

    /// Subscribe to sandbox lifecycle events.
    pub fn subscribe_events(&self) -> tokio::sync::broadcast::Receiver<SandboxEvent> {
        self.event_tx.subscribe()
    }

    /// Emit a sandbox event. Silently drops if no subscribers.
    pub fn emit_event(&self, event: SandboxEvent) {
        let _ = self.event_tx.send(event);
    }

    /// Mark a session as preparing for sandbox first-run work.
    /// Returns `true` only the first time for a session key.
    pub async fn mark_preparing_once(&self, session_key: &str) -> bool {
        let inserted = self
            .prepared_sessions
            .write()
            .await
            .insert(session_key.to_string());
        if inserted {
            self.clear_synced_session(session_key).await;
        }
        inserted
    }

    /// Clear preparation marker for a session (used on cleanup or prepare failure).
    pub async fn clear_prepared_session(&self, session_key: &str) {
        self.prepared_sessions.write().await.remove(session_key);
    }

    /// Mark a session as having completed workspace sync-in.
    pub async fn mark_synced(&self, session_key: &str) {
        self.sync_failures.write().await.remove(session_key);
        self.synced_sessions
            .write()
            .await
            .insert(session_key.to_string());
    }

    /// Mark a session as unblocked after first-run preparation failed.
    pub async fn mark_sync_failed(&self, session_key: &str, error: String) {
        self.sync_failures
            .write()
            .await
            .insert(session_key.to_string(), error);
        self.synced_sessions
            .write()
            .await
            .insert(session_key.to_string());
    }

    /// Check whether workspace sync has completed for a session.
    pub async fn is_synced(&self, session_key: &str) -> bool {
        self.synced_sessions.read().await.contains(session_key)
    }

    /// Return the first-run preparation failure for a session, if any.
    pub async fn sync_failure(&self, session_key: &str) -> Option<String> {
        self.sync_failures.read().await.get(session_key).cloned()
    }

    /// Clear sync marker for a session (used on cleanup).
    pub async fn clear_synced_session(&self, session_key: &str) {
        self.synced_sessions.write().await.remove(session_key);
        self.sync_failures.write().await.remove(session_key);
    }

    /// Clear per-session lifecycle markers after its global-backend runtime is removed.
    pub async fn clear_runtime_state(&self, session_key: &str) {
        self.clear_prepared_session(session_key).await;
        self.clear_synced_session(session_key).await;
    }

    /// Return whether the global sandbox policy is enabled.
    pub fn enabled(&self) -> bool {
        self.config.mode == SandboxMode::On
    }

    /// Resolve and prepare the sole execution environment for a session.
    ///
    /// Host execution is returned only when sandboxing is globally disabled.
    /// Enabled sessions fail closed unless the
    /// selected backend provides filesystem isolation and prepares successfully.
    pub async fn resolve_env(&self, session_key: &str) -> Result<ExecEnv> {
        if !self.enabled() {
            return Ok(ExecEnv::Host);
        }

        Self::require_fs_isolation(session_key, &*self.backend)?;

        let (backend, id) = self.prepare_command_session(session_key).await?;

        // Preparation can switch a failover backend to a weaker implementation.
        Self::require_fs_isolation(session_key, &*backend)?;

        Ok(ExecEnv::Sandbox { backend, id })
    }

    fn require_fs_isolation(session_key: &str, backend: &dyn Sandbox) -> Result<()> {
        if backend.provides_fs_isolation() {
            return Ok(());
        }

        Err(Error::message(format!(
            "sandbox is enabled for session {session_key:?}, but backend {} does not provide filesystem isolation",
            backend.backend_id()
        )))
    }

    /// Derive a SandboxId for a given session key.
    /// The key is sanitized for use as a container name (only alphanumeric, dash, underscore, dot).
    pub fn sandbox_id_for(&self, session_key: &str) -> SandboxId {
        let sanitized: String = session_key
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.' {
                    c
                } else {
                    '-'
                }
            })
            .collect();
        SandboxId {
            scope: self.config.scope.clone(),
            key: sanitized,
        }
    }

    /// Clean up sandbox resources for a session.
    ///
    /// For isolated backends, syncs workspace changes back to the host
    /// before destroying the sandbox.
    pub async fn cleanup_session(&self, session_key: &str) -> Result<()> {
        let id = self.sandbox_id_for(session_key);
        let backend = Arc::clone(&self.backend);

        // Sync workspace changes back to host for isolated backends.
        if backend.is_isolated()
            && let Some(host_workspace) = super::sync::resolve_sync_workspace(&self.config, &id)
        {
            let sandbox_workspace = backend.workspace_dir_for(&id).await;
            if let Err(e) =
                super::sync::sync_out(&*backend, &id, &host_workspace, &sandbox_workspace).await
            {
                warn!(
                    session = session_key,
                    %id,
                    error = %e,
                    "workspace sync-out failed, changes in sandbox may be lost"
                );
            }
        }

        backend.cleanup(&id).await?;
        self.clear_prepared_session(session_key).await;
        self.clear_synced_session(session_key).await;
        Ok(())
    }

    /// Prepare the sandbox for command execution, including first-run workspace sync.
    pub async fn prepare_command_session(
        &self,
        session_key: &str,
    ) -> Result<(Arc<dyn Sandbox>, SandboxId)> {
        let id = self.sandbox_id_for(session_key);
        let backend = Arc::clone(&self.backend);
        let image = self.default_image().await;

        info!(
            session = session_key,
            sandbox_id = %id,
            backend = %backend.backend_id(),
            image,
            "sandbox ensure_ready"
        );
        let announce_prepare = self.mark_preparing_once(session_key).await;
        if announce_prepare {
            self.emit_event(SandboxEvent::Preparing {
                session_key: session_key.to_string(),
                backend: backend.backend_id(),
                image: image.clone(),
            });
        }

        if let Err(error) = backend.ensure_ready(&id).await {
            if announce_prepare {
                self.clear_prepared_session(session_key).await;
                if backend.is_isolated() {
                    self.mark_sync_failed(session_key, error.to_string()).await;
                }
                self.emit_event(SandboxEvent::PrepareFailed {
                    session_key: session_key.to_string(),
                    backend: backend.backend_id(),
                    image: image.clone(),
                    error: error.to_string(),
                });
            }
            return Err(error);
        }

        if announce_prepare {
            self.emit_event(SandboxEvent::Prepared {
                session_key: session_key.to_string(),
                backend: backend.backend_id(),
                image: image.clone(),
            });

            if backend.is_isolated() {
                let sync_ok = if let Some(host_workspace) =
                    super::sync::resolve_sync_workspace(&self.config, &id)
                {
                    let sandbox_workspace = backend.workspace_dir_for(&id).await;
                    match super::sync::sync_in(&*backend, &id, &host_workspace, &sandbox_workspace)
                        .await
                    {
                        Ok(()) => true,
                        Err(error) => {
                            let message = error.to_string();
                            warn!(
                                session = session_key,
                                sandbox_id = %id,
                                error = %message,
                                "workspace sync-in failed"
                            );
                            self.clear_prepared_session(session_key).await;
                            self.mark_sync_failed(session_key, message.clone()).await;
                            return Err(Error::message(format!(
                                "workspace sync-in failed: {message}"
                            )));
                        },
                    }
                } else {
                    true
                };

                if sync_ok {
                    let has_prebuilt = image != DEFAULT_SANDBOX_IMAGE && !image.is_empty();
                    let packages = &self.config.packages;
                    if !has_prebuilt
                        && !packages.is_empty()
                        && let Err(error) = backend.provision_packages(&id, packages).await
                    {
                        warn!(
                            session = session_key,
                            sandbox_id = %id,
                            error = %error,
                            "package provisioning failed (non-fatal)"
                        );
                    }
                }

                self.mark_synced(session_key).await;
            }
        } else if backend.is_isolated() && !self.is_synced(session_key).await {
            let deadline = tokio::time::Instant::now() + Duration::from_secs(120);
            while !self.is_synced(session_key).await {
                if tokio::time::Instant::now() >= deadline {
                    warn!(
                        session = session_key,
                        "timed out waiting for workspace sync-in"
                    );
                    break;
                }
                tokio::time::sleep(Duration::from_millis(200)).await;
            }
        }

        if let Some(error) = self.sync_failure(session_key).await {
            return Err(Error::message(format!(
                "sandbox preparation failed: {error}"
            )));
        }

        Ok((backend, id))
    }

    /// Access the global sandbox backend.
    pub fn backend(&self) -> &Arc<dyn Sandbox> {
        &self.backend
    }

    /// Access the global sandbox mode.
    pub fn mode(&self) -> &SandboxMode {
        &self.config.mode
    }

    /// Access the global sandbox config.
    pub fn config(&self) -> &SandboxConfig {
        &self.config
    }

    /// Effective global sandbox runtime identity.
    pub fn backend_id(&self) -> SandboxBackendId {
        self.backend.backend_id()
    }

    /// Store the deterministic image produced from the global config.
    pub async fn set_prepared_image(&self, image: String) {
        *self.effective_image.write().await = image;
    }

    /// Get the single effective image for every sandboxed session.
    pub async fn default_image(&self) -> String {
        self.effective_image.read().await.clone()
    }
}
