//! Sandbox initialization helpers: router construction, deterministic image
//! preparation, and startup container garbage collection.

use std::sync::Arc;

use {
    chelix_tools::sandbox::{SandboxBackendId, SandboxConfig, SandboxMode},
    tracing::{debug, info},
};

/// Build the sandbox router with the selected global backend.
pub(super) fn build_sandbox_router(
    sandbox_config: &SandboxConfig,
    container_prefix: &str,
    timezone: Option<&str>,
) -> anyhow::Result<chelix_tools::sandbox::SandboxRouter> {
    let mut config = sandbox_config.clone();
    config.container_prefix = Some(container_prefix.to_string());
    config.timezone = timezone.map(ToOwned::to_owned);

    chelix_tools::sandbox::SandboxRouter::new(config)
        .map_err(|error| anyhow::anyhow!("failed to initialize sandbox: {error}"))
}

/// Build and register the one deterministic global sandbox image before startup continues.
pub(super) async fn prepare_sandbox_images(
    sandbox_router: &Arc<chelix_tools::sandbox::SandboxRouter>,
) -> anyhow::Result<()> {
    if !should_prepare_sandbox_images(sandbox_router.mode()) {
        debug!("sandbox image preparation skipped because sandbox mode is off");
        return Ok(());
    }

    let backend = sandbox_router.backend();
    let backend_id = backend.backend_id();
    let packages = sandbox_router.config().packages.clone();
    let base_image = sandbox_router
        .config()
        .image
        .clone()
        .unwrap_or_else(|| chelix_tools::sandbox::DEFAULT_SANDBOX_IMAGE.to_string());
    let result = backend
        .build_image(&base_image, &packages)
        .await
        .map_err(|error| {
            anyhow::anyhow!(
                "failed to prepare current sandbox image for backend {backend_id}: {error}"
            )
        })?;
    let Some(result) = result else {
        debug!(
            backend = %backend_id,
            "sandbox backend does not build OCI images"
        );
        return Ok(());
    };

    if result.built {
        info!(
            backend = %backend_id,
            tag = %result.tag,
            "current sandbox image build complete"
        );
    } else {
        debug!(
            backend = %backend_id,
            tag = %result.tag,
            "current sandbox image already exists"
        );
    }

    sandbox_router.set_prepared_image(result.tag).await;

    Ok(())
}

fn should_prepare_sandbox_images(mode: &SandboxMode) -> bool {
    matches!(mode, SandboxMode::On)
}

/// Spawn non-critical startup container garbage collection.
pub(super) fn spawn_sandbox_background_tasks(
    sandbox_router: &Arc<chelix_tools::sandbox::SandboxRouter>,
) {
    // Startup GC: remove orphaned session containers.
    if sandbox_router.backend_id() != SandboxBackendId::None {
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
        fn backend_id(&self) -> SandboxBackendId {
            SandboxBackendId::Docker
        }

        async fn ensure_ready(&self, _id: &SandboxId) -> chelix_tools::error::Result<()> {
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
        assert!(should_prepare_sandbox_images(&SandboxMode::On));
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
