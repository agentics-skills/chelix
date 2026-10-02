//! Sandbox initialization helpers: router construction, deterministic image
//! preparation, and archived session sandbox retention.

use std::sync::Arc;

use {
    async_trait::async_trait,
    chelix_sessions::metadata::SqliteSessionMetadata,
    chelix_tools::sandbox::{SandboxConfig, SandboxMode, SandboxOwnerResolver, SandboxRouter},
    tracing::{debug, error, info},
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
    terminal_size: chelix_config::schema::TerminalSizeConfig,
    container_prefix: &str,
    timezone: Option<&str>,
    session_metadata: Arc<SqliteSessionMetadata>,
) -> anyhow::Result<SandboxRouter> {
    let mut config = sandbox_config.clone();
    config.container_prefix = Some(container_prefix.to_string());
    config.timezone = timezone.map(ToOwned::to_owned);
    config.terminal_size = Some(terminal_size);

    let owner_resolver: Arc<dyn SandboxOwnerResolver> =
        Arc::new(SessionSandboxOwnerResolver::new(session_metadata));
    SandboxRouter::new(config, Some(owner_resolver))
        .map_err(|error| anyhow::anyhow!("failed to initialize sandbox: {error}"))
}

/// Build and register the one deterministic global sandbox image before startup continues.
pub(super) async fn prepare_sandbox_images(
    sandbox_router: &Arc<SandboxRouter>,
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

/// Check archived owner sandbox retention at startup and every day.
pub(super) fn spawn_sandbox_background_tasks(
    sandbox_router: &Arc<SandboxRouter>,
    session_metadata: &Arc<SqliteSessionMetadata>,
) -> anyhow::Result<()> {
    if !sandbox_router.enabled() {
        return Ok(());
    }
    let retention_days = sandbox_router
        .config()
        .archived_session_retention_days
        .ok_or_else(|| {
            anyhow::anyhow!(
                "sandbox.archived_session_retention_days is required when sandbox.mode is On"
            )
        })?;
    let sandbox_router = Arc::clone(sandbox_router);
    let session_metadata = Arc::clone(session_metadata);
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(time::Duration::days(1).unsigned_abs());
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            interval.tick().await;
            if let Err(error) =
                prune_archived_sandboxes(&sandbox_router, &session_metadata, retention_days).await
            {
                error!(%error, "archived session sandbox retention failed");
            }
        }
    });
    Ok(())
}

async fn prune_archived_sandboxes(
    sandbox_router: &SandboxRouter,
    session_metadata: &SqliteSessionMetadata,
    retention_days: u32,
) -> anyhow::Result<()> {
    let container_ids = sandbox_router.backend().existing_container_ids().await?;
    let sessions = session_metadata.list().await?;
    let cutoff_ms = (time::OffsetDateTime::now_utc()
        - time::OffsetDateTime::UNIX_EPOCH
        - time::Duration::days(i64::from(retention_days)))
    .whole_milliseconds();
    for session in sessions.iter().filter(|session| {
        session.archived
            && i128::from(session.updated_at) <= cutoff_ms
            && session
                .sandbox_owner_key
                .as_deref()
                .is_none_or(|owner| owner == session.key)
    }) {
        let id = sandbox_router.sandbox_id_for(&session.key);
        if !container_ids
            .iter()
            .any(|container_id| container_id == &id.key)
        {
            continue;
        }
        match sandbox_router.cleanup_owner_sandbox(&session.key).await {
            Ok(()) => info!(session = %session.key, "removed expired archived session sandbox"),
            Err(error) => {
                error!(session = %session.key, %error, "archived session sandbox cleanup failed")
            },
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use {
        async_trait::async_trait,
        chelix_tools::{
            command::{CommandOptions, CommandOutput},
            sandbox::{Sandbox, SandboxBackendId, SandboxId, SandboxRouter},
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
        cleanup_calls: AtomicUsize,
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

        async fn cleanup(&self, id: &SandboxId) -> chelix_tools::error::Result<()> {
            assert_eq!(id.key, "session-expired");
            self.cleanup_calls.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }

        async fn existing_container_ids(&self) -> chelix_tools::error::Result<Vec<String>> {
            Ok([
                "session-expired",
                "session-active",
                "session-recent",
                "session-child",
            ]
            .into_iter()
            .map(str::to_string)
            .collect())
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
    async fn archived_sandbox_retention_removes_only_expired_matching_owners() -> anyhow::Result<()>
    {
        let metadata = sqlite_metadata().await;
        let model_reasoning = chelix_common::ResolvedModelReasoning::try_new(
            "test::model".to_string(),
            chelix_common::ReasoningEffort::from("off"),
        )?;
        for (key, archived, owner, expired) in [
            ("session:expired", true, None, true),
            ("session:active", false, None, true),
            ("session:recent", true, None, false),
            ("session:child", true, Some("session:expired"), true),
            ("session:no-container", true, None, true),
        ] {
            metadata
                .create_llm_session(key, None, &model_reasoning, Some("main"))
                .await?;
            metadata.set_archived(key, archived).await?;
            if let Some(owner) = owner {
                metadata.set_sandbox_owner_key(key, Some(owner)).await?;
            }
            if expired {
                metadata.set_timestamps_and_counts(key, 0, 0, 0, 0).await?;
            }
        }
        let backend = Arc::new(RecordingBuildSandbox {
            build_calls: AtomicUsize::new(0),
            cleanup_calls: AtomicUsize::new(0),
        });
        let sandbox_backend: Arc<dyn Sandbox> = backend.clone();
        let resolver: Arc<dyn SandboxOwnerResolver> =
            Arc::new(SessionSandboxOwnerResolver::new(Arc::clone(&metadata)));
        let router = SandboxRouter::with_backend(
            SandboxConfig {
                mode: SandboxMode::On,
                archived_session_retention_days: Some(1),
                ..Default::default()
            },
            sandbox_backend,
            Some(resolver),
        )?;

        prune_archived_sandboxes(&router, &metadata, 1).await?;
        assert_eq!(backend.cleanup_calls.load(Ordering::SeqCst), 1);
        Ok(())
    }

    #[tokio::test]
    async fn sandbox_mode_off_skips_backend_image_build() {
        let backend = Arc::new(RecordingBuildSandbox {
            build_calls: AtomicUsize::new(0),
            cleanup_calls: AtomicUsize::new(0),
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
