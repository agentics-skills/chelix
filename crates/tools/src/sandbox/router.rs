//! Sandbox orchestration: backend selection and routing.

use std::{collections::HashSet, sync::Arc};

use {
    tokio::sync::RwLock,
    tracing::{debug, info},
};

#[cfg(target_os = "macos")]
use super::apple::{AppleContainerSandbox, ensure_apple_container_service};
use {
    super::{
        containers::{is_cli_available, is_docker_daemon_available, should_use_docker_backend},
        docker::{DockerSandbox, NoSandbox},
        env::ExecEnv,
        owner::SandboxOwnerResolver,
        types::{
            Sandbox, SandboxBackend, SandboxBackendId, SandboxConfig, SandboxId, SandboxMode,
            SharedSandboxImage, ToolsServiceInstance, shared_sandbox_image,
        },
    },
    crate::error::{Error, Result},
};

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
    /// Single effective image shared by every sandbox session.
    effective_image: SharedSandboxImage,
    /// Event channel for sandbox lifecycle events (prepare/provision/build feedback).
    event_tx: tokio::sync::broadcast::Sender<SandboxEvent>,
    owner_resolver: Option<Arc<dyn SandboxOwnerResolver>>,
    /// Owner keys that have already completed sandbox initialization.
    /// Used to avoid repeating first-run preparation banners on every command.
    prepared_sessions: RwLock<HashSet<String>>,
}

impl SandboxRouter {
    pub fn new(
        config: SandboxConfig,
        owner_resolver: Option<Arc<dyn SandboxOwnerResolver>>,
    ) -> Result<Self> {
        Self::require_owner_resolver(&config, owner_resolver.as_ref())?;
        let effective_image = shared_sandbox_image(&config);
        let backend =
            create_sandbox_with_global_image(config.clone(), Arc::clone(&effective_image))?;
        let (event_tx, _) = tokio::sync::broadcast::channel(32);
        Ok(Self {
            config,
            backend,
            effective_image,
            event_tx,
            owner_resolver,
            prepared_sessions: RwLock::new(HashSet::new()),
        })
    }

    /// Create the canonical router for explicit global host execution.
    #[must_use]
    pub fn disabled() -> Self {
        let config = SandboxConfig {
            mode: SandboxMode::Off,
            ..SandboxConfig::default()
        };
        let effective_image = shared_sandbox_image(&config);
        let (event_tx, _) = tokio::sync::broadcast::channel(32);
        Self {
            config,
            backend: Arc::new(NoSandbox),
            effective_image,
            event_tx,
            owner_resolver: None,
            prepared_sessions: RwLock::new(HashSet::new()),
        }
    }

    /// Create a router with a custom sandbox backend (useful for testing).
    pub fn with_backend(
        config: SandboxConfig,
        backend: Arc<dyn Sandbox>,
        owner_resolver: Option<Arc<dyn SandboxOwnerResolver>>,
    ) -> Result<Self> {
        Self::require_owner_resolver(&config, owner_resolver.as_ref())?;
        let effective_image = shared_sandbox_image(&config);
        let (event_tx, _) = tokio::sync::broadcast::channel(32);
        Ok(Self {
            config,
            backend,
            effective_image,
            event_tx,
            owner_resolver,
            prepared_sessions: RwLock::new(HashSet::new()),
        })
    }

    fn require_owner_resolver(
        config: &SandboxConfig,
        owner_resolver: Option<&Arc<dyn SandboxOwnerResolver>>,
    ) -> Result<()> {
        if config.mode == SandboxMode::On && owner_resolver.is_none() {
            return Err(Error::message(
                "sandbox mode is On, but no sandbox owner resolver is configured",
            ));
        }
        Ok(())
    }

    async fn resolve_owner_key(&self, session_key: &str) -> Result<String> {
        if !self.enabled() {
            return Ok(session_key.to_string());
        }
        let resolver = self.owner_resolver.as_ref().ok_or_else(|| {
            Error::message("sandbox mode is On, but no sandbox owner resolver is configured")
        })?;
        resolver.resolve_owner_key(session_key).await
    }

    /// Subscribe to sandbox lifecycle events.
    pub fn subscribe_events(&self) -> tokio::sync::broadcast::Receiver<SandboxEvent> {
        self.event_tx.subscribe()
    }

    /// Emit a sandbox event. Silently drops if no subscribers.
    pub fn emit_event(&self, event: SandboxEvent) {
        let _ = self.event_tx.send(event);
    }

    /// Mark an owner as preparing for sandbox first-run work.
    /// Returns `true` only the first time for an owner key.
    pub async fn mark_preparing_once(&self, owner_key: &str) -> bool {
        self.prepared_sessions
            .write()
            .await
            .insert(owner_key.to_string())
    }

    /// Clear preparation marker for an owner (used on cleanup or prepare failure).
    pub async fn clear_prepared_session(&self, owner_key: &str) {
        self.prepared_sessions.write().await.remove(owner_key);
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

    /// Derive a SandboxId from an already resolved sandbox owner key.
    pub fn sandbox_id_for(&self, owner_key: &str) -> SandboxId {
        let sanitized: String = owner_key
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

    /// Clean up sandbox resources only when the requested session owns them.
    pub async fn cleanup_session(&self, session_key: &str) -> Result<()> {
        let owner_key = self.resolve_owner_key(session_key).await?;
        if session_key != owner_key {
            debug!(
                session_key,
                owner_key, "sandbox cleanup skipped for non-owner session"
            );
            return Ok(());
        }
        self.cleanup_owner_sandbox(&owner_key).await
    }

    /// Clean up sandbox resources for a session that owns them.
    pub async fn cleanup_owner_sandbox(&self, owner_key: &str) -> Result<()> {
        let id = self.sandbox_id_for(owner_key);
        let backend = Arc::clone(&self.backend);

        backend.cleanup(&id).await?;
        self.clear_prepared_session(owner_key).await;
        Ok(())
    }

    /// Prepare the sandbox for command execution.
    pub async fn prepare_command_session(
        &self,
        session_key: &str,
    ) -> Result<(Arc<dyn Sandbox>, SandboxId)> {
        let owner_key = self.resolve_owner_key(session_key).await?;
        let id = self.sandbox_id_for(&owner_key);
        let backend = Arc::clone(&self.backend);
        let image = self.default_image().await;

        info!(
            session = session_key,
            owner = owner_key,
            sandbox_id = %id,
            backend = %backend.backend_id(),
            image,
            "sandbox ensure_ready"
        );
        let announce_prepare = self.mark_preparing_once(&owner_key).await;
        if announce_prepare {
            self.emit_event(SandboxEvent::Preparing {
                session_key: owner_key.clone(),
                backend: backend.backend_id(),
                image: image.clone(),
            });
        }

        if let Err(error) = backend.ensure_ready(&id).await {
            if announce_prepare {
                self.clear_prepared_session(&owner_key).await;
                self.emit_event(SandboxEvent::PrepareFailed {
                    session_key: owner_key.clone(),
                    backend: backend.backend_id(),
                    image: image.clone(),
                    error: error.to_string(),
                });
            }
            return Err(error);
        }

        if announce_prepare {
            self.emit_event(SandboxEvent::Prepared {
                session_key: owner_key,
                backend: backend.backend_id(),
                image: image.clone(),
            });
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

    /// Return only tools-service instances already registered by the active backend.
    pub async fn tools_service_instances(&self) -> Result<Vec<ToolsServiceInstance>> {
        self.backend.tools_service_instances().await
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
