//! Sandbox initialization helpers: router construction, deterministic image
//! preparation, host provisioning, and startup container garbage collection.

use std::sync::Arc;

use {
    chelix_tools::sandbox::{SandboxConfig, SandboxMode},
    tracing::{debug, info, warn},
};

use crate::{
    broadcast::{BroadcastOpts, broadcast},
    state::GatewayState,
};

/// Type alias for the deferred state used in prepare_core.
type DeferredState = tokio::sync::OnceCell<Arc<GatewayState>>;

/// Build the sandbox router with all configured backends registered.
pub(super) fn build_sandbox_router(
    sandbox_config: &SandboxConfig,
    container_prefix: &str,
    timezone: Option<&str>,
) -> chelix_tools::sandbox::SandboxRouter {
    let mut config = sandbox_config.clone();
    config.container_prefix = Some(container_prefix.to_string());
    config.timezone = timezone.map(ToOwned::to_owned);

    chelix_tools::sandbox::SandboxRouter::new(config)
}

/// Build and register the current deterministic sandbox image before startup continues.
pub(super) async fn prepare_sandbox_images(
    sandbox_router: &Arc<chelix_tools::sandbox::SandboxRouter>,
) -> anyhow::Result<()> {
    if !should_prepare_sandbox_images(sandbox_router.mode()) {
        debug!("sandbox image preparation skipped because sandbox mode is off");
        return Ok(());
    }

    let backends = sandbox_router.available_backend_instances();
    let default_backend_name = sandbox_router.backend_name().to_string();
    let packages = sandbox_router.config().packages.clone();
    let base_image = sandbox_router
        .config()
        .image
        .clone()
        .unwrap_or_else(|| chelix_tools::sandbox::DEFAULT_SANDBOX_IMAGE.to_string());
    let mut default_tag = None;

    for backend in backends {
        let backend_name = backend.backend_name();
        let result = backend
            .build_image(&base_image, &packages)
            .await
            .map_err(|error| {
                anyhow::anyhow!(
                    "failed to prepare current sandbox image for backend {backend_name}: {error}"
                )
            })?;
        let Some(result) = result else {
            debug!(
                backend = backend_name,
                "sandbox backend does not build OCI images"
            );
            continue;
        };

        if result.built {
            info!(
                backend = backend_name,
                tag = %result.tag,
                "current sandbox image build complete"
            );
        } else {
            debug!(
                backend = backend_name,
                tag = %result.tag,
                "current sandbox image already exists"
            );
        }

        sandbox_router
            .set_backend_image(backend_name, result.tag.clone())
            .await
            .map_err(|error| {
                anyhow::anyhow!(
                    "failed to register current sandbox image for backend {backend_name}: {error}"
                )
            })?;
        if backend_name == default_backend_name {
            default_tag = Some(result.tag);
        }
    }

    if let Some(tag) = default_tag {
        sandbox_router.set_global_image(Some(tag)).await;
    }

    Ok(())
}

fn should_prepare_sandbox_images(mode: &SandboxMode) -> bool {
    !matches!(mode, SandboxMode::Off)
}

/// Spawn non-critical sandbox background tasks: host provisioning and startup GC.
pub(super) fn spawn_sandbox_background_tasks(
    sandbox_router: &Arc<chelix_tools::sandbox::SandboxRouter>,
    deferred_state: &Arc<DeferredState>,
) {
    // Host package provisioning when no container runtime is available.
    {
        let packages = sandbox_router.config().packages.clone();
        if sandbox_router.backend_name() == "none"
            && !packages.is_empty()
            && chelix_tools::sandbox::is_debian_host()
        {
            let deferred_for_host = Arc::clone(deferred_state);
            let pkg_count = packages.len();
            tokio::spawn(async move {
                if let Some(state) = deferred_for_host.get() {
                    broadcast(
                        state,
                        "sandbox.host.provision",
                        serde_json::json!({
                            "phase": "start",
                            "count": pkg_count,
                        }),
                        BroadcastOpts {
                            drop_if_slow: true,
                            ..Default::default()
                        },
                    )
                    .await;
                }

                match chelix_tools::sandbox::provision_host_packages(&packages).await {
                    Ok(Some(result)) => {
                        info!(
                            installed = result.installed.len(),
                            skipped = result.skipped.len(),
                            sudo = result.used_sudo,
                            "host package provisioning complete"
                        );
                        if let Some(state) = deferred_for_host.get() {
                            broadcast(
                                state,
                                "sandbox.host.provision",
                                serde_json::json!({
                                    "phase": "done",
                                    "installed": result.installed.len(),
                                    "skipped": result.skipped.len(),
                                }),
                                BroadcastOpts {
                                    drop_if_slow: true,
                                    ..Default::default()
                                },
                            )
                            .await;
                        }
                    },
                    Ok(None) => {
                        debug!("host package provisioning: no-op (not debian or empty packages)");
                    },
                    Err(e) => {
                        warn!("host package provisioning failed: {e}");
                        if let Some(state) = deferred_for_host.get() {
                            broadcast(
                                state,
                                "sandbox.host.provision",
                                serde_json::json!({
                                    "phase": "error",
                                    "error": e.to_string(),
                                }),
                                BroadcastOpts {
                                    drop_if_slow: true,
                                    ..Default::default()
                                },
                            )
                            .await;
                        }
                    },
                }
            });
        }
    }

    // Startup GC: remove orphaned session containers.
    if sandbox_router.backend_name() != "none" {
        let prefix = sandbox_router.config().container_prefix.clone();
        tokio::spawn(async move {
            if let Some(prefix) = prefix {
                match chelix_tools::sandbox::clean_all_containers(&prefix).await {
                    Ok(0) => {},
                    Ok(n) => info!(
                        removed = n,
                        "startup GC: cleaned orphaned session containers"
                    ),
                    Err(e) => debug!("startup GC: container cleanup skipped: {e}"),
                }
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use {
        async_trait::async_trait,
        chelix_tools::{
            command::{CommandOptions, CommandOutput},
            sandbox::{Sandbox, SandboxId, SandboxRouter},
        },
    };

    use super::*;

    struct RecordingBuildSandbox {
        build_calls: AtomicUsize,
    }

    #[async_trait]
    impl Sandbox for RecordingBuildSandbox {
        fn backend_name(&self) -> &'static str {
            "recording"
        }

        async fn ensure_ready(
            &self,
            _id: &SandboxId,
            _image_override: Option<&str>,
        ) -> chelix_tools::error::Result<()> {
            Ok(())
        }

        async fn run_command(
            &self,
            _id: &SandboxId,
            _command: &str,
            _opts: &CommandOptions,
        ) -> chelix_tools::error::Result<CommandOutput> {
            Err(chelix_tools::error::Error::message(
                "run_command is not used by this test",
            ))
        }

        async fn cleanup(&self, _id: &SandboxId) -> chelix_tools::error::Result<()> {
            Ok(())
        }

        async fn build_image(
            &self,
            _base: &str,
            _packages: &[String],
        ) -> chelix_tools::error::Result<Option<chelix_tools::sandbox::BuildImageResult>> {
            self.build_calls.fetch_add(1, Ordering::SeqCst);
            Ok(None)
        }
    }

    #[test]
    fn sandbox_image_preparation_follows_global_mode() {
        assert!(!should_prepare_sandbox_images(&SandboxMode::Off));
        assert!(should_prepare_sandbox_images(&SandboxMode::NonMain));
        assert!(should_prepare_sandbox_images(&SandboxMode::All));
    }

    #[tokio::test]
    async fn sandbox_mode_off_skips_backend_image_build() {
        let backend = Arc::new(RecordingBuildSandbox {
            build_calls: AtomicUsize::new(0),
        });
        let sandbox_backend: Arc<dyn Sandbox> = backend.clone();
        let router = Arc::new(SandboxRouter::with_backend(
            SandboxConfig {
                mode: SandboxMode::Off,
                ..Default::default()
            },
            sandbox_backend,
        ));

        let result = prepare_sandbox_images(&router).await;

        assert!(result.is_ok(), "mode=off must skip image preparation");
        assert_eq!(backend.build_calls.load(Ordering::SeqCst), 0);
    }
}
