//! Session-end memory summary.
//!
//! Before a session is cleared (`sessions.reset`), runs an LLM-powered
//! silent turn to summarize what was accomplished and save it to memory.
//! Gated by `[memory] enable_session_summary = true` (default: true).

use std::sync::Arc;

use {
    anyhow::{Context, Result},
    tracing::{debug, info},
};

use crate::state::GatewayState;

/// Run the session-end memory summary if the configured summary gates allow it.
pub(crate) async fn run_session_summary_if_enabled(
    state: &Arc<GatewayState>,
    session_key: &str,
) -> Result<()> {
    let config = &state.config;
    if !config.memory.enable_session_summary {
        return Ok(());
    }

    let write_mode = config.memory.agent_write_mode;
    if !chelix_chat::memory_write_mode_allows_save(write_mode) {
        debug!("session summary: agent memory writes disabled, skipping");
        return Ok(());
    }

    let session_store = state
        .services
        .session_store
        .as_ref()
        .context("session summary requires a session store")?;
    let history = session_store
        .read(session_key)
        .await
        .with_context(|| format!("session summary: failed to read session '{session_key}'"))?;
    if history.len() < 4 {
        debug!("session summary: too few messages, skipping");
        return Ok(());
    }

    let Some(memory_manager) = state.memory_manager.as_ref() else {
        return Ok(());
    };

    let metadata = state
        .services
        .session_metadata
        .as_ref()
        .context("session summary requires session metadata")?;
    let session_entry = metadata
        .get(session_key)
        .await
        .with_context(|| format!("session summary: failed to load session '{session_key}'"))?
        .ok_or_else(|| anyhow::anyhow!("session '{session_key}' not found"))?;
    let model_reasoning = session_entry.model_reasoning().cloned().ok_or_else(|| {
        anyhow::anyhow!("session '{session_key}' does not have a model/reasoning pair")
    })?;
    let agent_id = session_entry
        .agent_id
        .ok_or_else(|| anyhow::anyhow!("session '{session_key}' does not have an agent ID"))?;

    let provider = {
        let registry = state
            .inner
            .read()
            .await
            .llm_providers
            .clone()
            .context("session summary requires a provider registry")?;
        let registry = registry.read().await;
        let resolved = registry.resolve_model_reasoning(
            Some(model_reasoning.model_id()),
            Some(model_reasoning.reasoning_effort()),
        )?;
        Arc::clone(resolved.provider())
    };

    let tools_config = chelix_config::ToolsConfigSource::Filesystem
        .load()
        .context("session summary: failed to reload tools config")?;
    let runtime_limits = config.agent_runtime_limits(&agent_id).with_context(|| {
        format!("session summary: failed to resolve runtime limits for agent '{agent_id}'")
    })?;
    let chat_messages = chelix_agents::model::values_to_chat_messages(&history)
        .context("session summary: failed to reconstruct session history")?;
    let writer: Arc<dyn chelix_agents::memory_writer::MemoryWriter> = Arc::new(
        chelix_chat::AgentScopedMemoryWriter::new(Arc::clone(memory_manager), agent_id, write_mode),
    );

    let paths = chelix_agents::silent_turn::run_silent_memory_turn_with_prompt(
        provider,
        &tools_config,
        runtime_limits.max_tools_threshold,
        &chat_messages,
        writer,
        chelix_agents::silent_turn::SilentTurnPrompt::SessionSummary,
    )
    .await?;
    if !paths.is_empty() {
        info!(
            files = paths.len(),
            session = %session_key,
            "session-end summary: wrote memory files"
        );
    }
    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use {
        super::*,
        crate::services::GatewayServices,
        chelix_common::{ReasoningEffort, ResolvedModelReasoning},
        chelix_providers::ProviderRegistry,
        chelix_sessions::{metadata::SqliteSessionMetadata, store::SessionStore},
        tokio::sync::RwLock,
    };

    #[tokio::test]
    async fn session_summary_returns_model_resolution_errors() {
        const SESSION_KEY: &str = "session:summary-test";

        let dir = tempfile::tempdir().unwrap();
        let session_store = Arc::new(SessionStore::new(dir.path().join("sessions")));
        let session_pool = sqlx::SqlitePool::connect("sqlite::memory:").await.unwrap();
        chelix_projects::run_migrations(&session_pool)
            .await
            .unwrap();
        SqliteSessionMetadata::init(&session_pool).await.unwrap();
        let session_metadata = Arc::new(SqliteSessionMetadata::new(session_pool));

        let memory_pool = sqlx::SqlitePool::connect(":memory:").await.unwrap();
        chelix_memory::schema::run_migrations(&memory_pool)
            .await
            .unwrap();
        let memory_manager: chelix_memory::runtime::DynMemoryRuntime =
            Arc::new(chelix_memory::manager::MemoryManager::keyword_only(
                chelix_memory::config::MemoryConfig {
                    db_path: ":memory:".to_string(),
                    data_dir: Some(dir.path().to_path_buf()),
                    ..Default::default()
                },
                Box::new(chelix_memory::store_sqlite::SqliteMemoryStore::new(
                    memory_pool,
                )),
            ));

        let services = GatewayServices::noop()
            .with_session_store(Arc::clone(&session_store))
            .with_session_metadata(Arc::clone(&session_metadata));
        let mut config = chelix_config::ChelixConfig::default();
        config.sandbox.mode = chelix_config::schema::SandboxMode::Off;
        config.agents.default = "main".to_string();
        config.agents.entries.insert(
            "main".to_string(),
            chelix_config::AgentConfig::new(
                "Main",
                "missing::summary-model",
                ReasoningEffort::from("low"),
            ),
        );
        let state = GatewayState::with_options(
            crate::auth::resolve_auth(None, None),
            services,
            config,
            Arc::new(chelix_tools::sandbox::SandboxRouter::disabled()),
            None,
            false,
            false,
            false,
            None,
            Some(memory_manager),
            Arc::new(chelix_code_index::CodeIndex::config_only(
                chelix_code_index::CodeIndexConfig::default(),
            )),
            18789,
            false,
            None,
            None,
            #[cfg(feature = "metrics")]
            None,
            #[cfg(feature = "metrics")]
            None,
            #[cfg(feature = "vault")]
            None,
        );
        state.inner.write().await.llm_providers =
            Some(Arc::new(RwLock::new(ProviderRegistry::empty())));

        let pair = ResolvedModelReasoning::try_new(
            "missing::summary-model".to_string(),
            ReasoningEffort::from("low"),
        )
        .unwrap();
        session_metadata
            .create_llm_session(SESSION_KEY, None, &pair, Some("main"))
            .await
            .unwrap();
        for message in [
            serde_json::json!({"role": "user", "content": "First question"}),
            serde_json::json!({"role": "assistant", "content": "First answer"}),
            serde_json::json!({"role": "user", "content": "Second question"}),
            serde_json::json!({"role": "assistant", "content": "Second answer"}),
        ] {
            session_store.append(SESSION_KEY, &message).await.unwrap();
        }

        let error = run_session_summary_if_enabled(&state, SESSION_KEY)
            .await
            .unwrap_err();

        assert_eq!(
            error.to_string(),
            "model `missing::summary-model` is not registered"
        );
    }
}
