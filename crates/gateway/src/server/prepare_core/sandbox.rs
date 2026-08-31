//! Sandbox initialization helpers: router construction, deterministic image
//! preparation, and startup container garbage collection.

use std::sync::Arc;

use {
    async_trait::async_trait,
    chelix_sessions::metadata::SqliteSessionMetadata,
    chelix_tools::sandbox::{SandboxBackendId, SandboxConfig, SandboxMode, SandboxOwnerResolver},
    tracing::{debug, info},
};

pub(super) struct SessionSandboxOwnerResolver {
    session_metadata: Arc<SqliteSessionMetadata>,
}

impl SessionSandboxOwnerResolver {
    pub(super) fn new(session_metadata: Arc<SqliteSessionMetadata>) -> Self {
        Self { session_metadata }
    }
}

#[async_trait]
impl SandboxOwnerResolver for SessionSandboxOwnerResolver {
    async fn resolve_owner_key(&self, session_key: &str) -> chelix_tools::error::Result<String> {
        let session = self
            .session_metadata
            .try_get(session_key)
            .await
            .map_err(|error| chelix_tools::error::Error::message(error.to_string()))?
            .ok_or_else(|| {
                chelix_tools::error::Error::message(format!(
                    "sandbox session {session_key:?} does not exist"
                ))
            })?;
        let owner_key = session
            .sandbox_owner_key
            .unwrap_or_else(|| session.key.clone());
        if owner_key != session.key {
            let owner_exists = self
                .session_metadata
                .try_get(&owner_key)
                .await
                .map_err(|error| chelix_tools::error::Error::message(error.to_string()))?
                .is_some();
            if !owner_exists {
                return Err(chelix_tools::error::Error::message(format!(
                    "sandbox owner session {owner_key:?} referenced by {session_key:?} does not exist"
                )));
            }
        }
        Ok(owner_key)
    }
}

/// Build the sandbox router with the selected global backend.
pub(super) fn build_sandbox_router(
    sandbox_config: &SandboxConfig,
    container_prefix: &str,
    timezone: Option<&str>,
    session_metadata: Arc<SqliteSessionMetadata>,
) -> anyhow::Result<chelix_tools::sandbox::SandboxRouter> {
    let mut config = sandbox_config.clone();
    config.container_prefix = Some(container_prefix.to_string());
    config.timezone = timezone.map(ToOwned::to_owned);

    let owner_resolver: Arc<dyn SandboxOwnerResolver> =
        Arc::new(SessionSandboxOwnerResolver::new(session_metadata));
    chelix_tools::sandbox::SandboxRouter::new(config, Some(owner_resolver))
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

    async fn sqlite_metadata() -> Arc<SqliteSessionMetadata> {
        let pool = sqlx::SqlitePool::connect("sqlite::memory:")
            .await
            .unwrap_or_else(|error| panic!("test SQLite connection failed: {error}"));
        chelix_projects::run_migrations(&pool)
            .await
            .unwrap_or_else(|error| panic!("test project migrations failed: {error}"));
        chelix_sessions::run_migrations(&pool)
            .await
            .unwrap_or_else(|error| panic!("test session migrations failed: {error}"));
        Arc::new(SqliteSessionMetadata::new(pool))
    }

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
    async fn session_owner_resolver_rejects_missing_referenced_owner() {
        let metadata = sqlite_metadata().await;
        let model_reasoning = chelix_common::ResolvedModelReasoning::try_new(
            "test::model".to_string(),
            chelix_common::ReasoningEffort::from("off"),
        )
        .unwrap_or_else(|error| panic!("valid test pair: {error}"));
        metadata
            .create_llm_session("session:child", None, &model_reasoning, Some("main"))
            .await
            .unwrap_or_else(|error| panic!("child session setup failed: {error}"));
        metadata
            .set_sandbox_owner_key("session:child", Some("session:missing"))
            .await
            .unwrap_or_else(|error| panic!("child owner setup failed: {error}"));
        let resolver = SessionSandboxOwnerResolver::new(metadata);

        let error = resolver
            .resolve_owner_key("session:child")
            .await
            .err()
            .unwrap_or_else(|| panic!("missing owner must fail"));
        assert!(
            error
                .to_string()
                .contains("referenced by \"session:child\" does not exist")
        );
    }

    #[tokio::test]
    async fn sandbox_mode_off_skips_backend_image_build() {
        let backend = Arc::new(RecordingBuildSandbox {
            build_calls: AtomicUsize::new(0),
        });
        let sandbox_backend: Arc<dyn Sandbox> = backend.clone();
        let router = Arc::new(
            SandboxRouter::with_backend(
                SandboxConfig {
                    mode: SandboxMode::Off,
                    ..Default::default()
                },
                sandbox_backend,
                None,
            )
            .unwrap_or_else(|error| panic!("test sandbox router failed: {error}")),
        );

        let result = prepare_sandbox_images(&router).await;

        assert!(result.is_ok(), "mode=off must skip image preparation");
        assert_eq!(backend.build_calls.load(Ordering::SeqCst), 0);
    }
}
