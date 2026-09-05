//! Session title auto-generation.
//!
//! Uses a lightweight LLM call to produce a short descriptive title from
//! the first few messages. Runs in the background after the first assistant
//! response so it never blocks the chat flow.

use std::sync::Arc;

use {
    anyhow::{Context, Result},
    tracing::{debug, info, warn},
};

use crate::{
    broadcast::{BroadcastOpts, broadcast},
    state::GatewayState,
};

/// Minimum number of messages before title generation fires (1 user + 1 assistant).
const MIN_MESSAGES_FOR_TITLE: usize = 2;
const MAIN_SESSION_KEY: &str = "main";

fn is_reserved_main_session(session_key: &str) -> bool {
    session_key == MAIN_SESSION_KEY
}

/// Generate and persist a session title if the session has no label yet.
///
/// Intended to be called from a background task after the first assistant
/// response. No-ops silently when:
/// - the session already has a label
/// - there are too few messages
pub(crate) async fn generate_title_if_needed(state: &Arc<GatewayState>, session_key: &str) {
    let Err(e) = try_generate_title_if_needed(state, session_key).await else {
        return;
    };
    warn!(error = %e, session = %session_key, "auto-title: generation failed");
}

async fn try_generate_title_if_needed(
    state: &Arc<GatewayState>,
    session_key: &str,
) -> Result<Option<String>> {
    let Some(session_metadata) = state.services.session_metadata.as_ref() else {
        return Ok(None);
    };
    let entry = match session_metadata
        .get(session_key)
        .await
        .context("failed to load session metadata for auto-title")?
    {
        Some(entry) => entry,
        None => return Ok(None),
    };

    // Skip if the session already has a user-set label.
    if entry.label.is_some() {
        debug!(session = %session_key, "auto-title: session already has label, skipping");
        return Ok(entry.label);
    }

    generate_title_for_session(state, session_key).await
}

/// Unconditionally generate and persist a title for the session.
///
/// Used by both the auto-trigger (via [`generate_title_if_needed`]) and the
/// manual `/title` command / RPC endpoint.
pub(crate) async fn generate_title_for_session(
    state: &Arc<GatewayState>,
    session_key: &str,
) -> Result<Option<String>> {
    if is_reserved_main_session(session_key) {
        debug!(session = %session_key, "auto-title: reserved main session, skipping");
        return Ok(None);
    }

    let Some(session_store) = state.services.session_store.as_ref() else {
        return Ok(None);
    };
    let Some(session_metadata) = state.services.session_metadata.as_ref() else {
        return Ok(None);
    };

    let history = match session_store.read(session_key).await {
        Ok(h) if h.len() >= MIN_MESSAGES_FOR_TITLE => h,
        Ok(_) => {
            debug!("auto-title: too few messages, skipping");
            return Ok(None);
        },
        Err(e) => {
            warn!(error = %e, "auto-title: failed to read session history");
            return Err(e).context("failed to read session history");
        },
    };

    let provider: Arc<dyn chelix_agents::model::LlmProvider> = {
        let inner = state.inner.read().await;
        let Some(ref registry) = inner.llm_providers else {
            anyhow::bail!("auto-title provider registry is unavailable");
        };
        let reg = registry.read().await;

        let title_config = state
            .config
            .auxiliary
            .title_generation
            .as_ref()
            .context("auto-title auxiliary.title_generation is not configured")?;
        let resolved = reg.resolve_model_reasoning(
            Some(&title_config.model),
            Some(&title_config.reasoning_effort),
        )?;
        Arc::clone(resolved.provider())
    };

    let chat_msgs = chelix_agents::model::values_to_chat_messages(&history)
        .context("failed to reconstruct session history for title generation")?;
    let title = chelix_agents::title::generate_title(provider, &chat_msgs).await?;
    // Persist the title as the session label and read back the
    // entry atomically so the broadcast version is consistent.
    let entry = session_metadata
        .update_label(session_key, Some(&title))
        .await
        .with_context(|| format!("failed to persist title for session {session_key}"))?;

    info!(session = %session_key, title = %title, "auto-title: set session title");

    broadcast(
        state,
        "session",
        serde_json::json!({
            "kind": "patched",
            "sessionKey": session_key,
            "version": entry.version,
            "label": title,
        }),
        BroadcastOpts::default(),
    )
    .await;

    Ok(Some(title))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use std::{
        pin::Pin,
        sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        },
    };

    use {
        async_trait::async_trait,
        chelix_agents::model::{ChatMessage, CompletionResponse, LlmProvider, StreamEvent, Usage},
        chelix_auth::{AuthMode, ResolvedAuth},
        chelix_common::{ConfigModelOverride, ModelMetadata, ModelModality, ReasoningEffort},
        chelix_providers::{ModelInfo, ProviderRegistry},
        chelix_sessions::{metadata::SqliteSessionMetadata, store::SessionStore},
        tokio::sync::RwLock,
        tokio_stream::Stream,
    };

    use {super::*, crate::services::GatewayServices};

    #[derive(Clone)]
    struct MockTitleProvider {
        result: Result<&'static str, &'static str>,
        expected_effort: ReasoningEffort,
        applied_effort: Option<ReasoningEffort>,
        calls: Arc<AtomicUsize>,
    }

    impl MockTitleProvider {
        fn new(result: Result<&'static str, &'static str>, effort: &str) -> Self {
            Self {
                result,
                expected_effort: ReasoningEffort::from(effort),
                applied_effort: None,
                calls: Arc::new(AtomicUsize::new(0)),
            }
        }
    }

    fn title_config(effort: &str) -> ConfigModelOverride {
        ConfigModelOverride {
            model: "mock::mock-title".to_string(),
            reasoning_effort: ReasoningEffort::from(effort),
        }
    }

    fn title_pair() -> chelix_common::ResolvedModelReasoning {
        chelix_common::ResolvedModelReasoning::try_new(
            "mock::mock-title".to_string(),
            ReasoningEffort::from("low"),
        )
        .unwrap()
    }

    #[async_trait]
    impl LlmProvider for MockTitleProvider {
        fn name(&self) -> &str {
            "mock"
        }

        fn id(&self) -> &str {
            "mock-title"
        }

        async fn complete(
            &self,
            _messages: &[ChatMessage],
            _tools: &[serde_json::Value],
        ) -> Result<CompletionResponse> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            assert_eq!(self.applied_effort.as_ref(), Some(&self.expected_effort));
            let text = match &self.result {
                Ok(title) => Some((*title).to_string()),
                Err(e) => anyhow::bail!(e.to_string()),
            };
            Ok(CompletionResponse {
                text,
                tool_calls: Vec::new(),
                usage: Usage::default(),
            })
        }

        fn stream(
            &self,
            _messages: Vec<ChatMessage>,
        ) -> Pin<Box<dyn Stream<Item = StreamEvent> + Send + '_>> {
            Box::pin(tokio_stream::empty())
        }

        fn with_reasoning_effort(
            self: Arc<Self>,
            effort: ReasoningEffort,
        ) -> Option<Arc<dyn LlmProvider>> {
            Some(Arc::new(Self {
                applied_effort: Some(effort),
                ..self.as_ref().clone()
            }))
        }
    }

    async fn test_state(
        provider: Arc<dyn LlmProvider>,
        title_config: Option<ConfigModelOverride>,
        supported_effort: &str,
    ) -> (Arc<GatewayState>, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let session_store = Arc::new(SessionStore::new(dir.path().to_path_buf()));
        let pool = sqlx::SqlitePool::connect("sqlite::memory:").await.unwrap();
        chelix_projects::run_migrations(&pool).await.unwrap();
        SqliteSessionMetadata::init(&pool).await.unwrap();
        let session_metadata = Arc::new(SqliteSessionMetadata::new(pool));
        let services = GatewayServices::noop()
            .with_session_store(Arc::clone(&session_store))
            .with_session_metadata(Arc::clone(&session_metadata));
        let mut config = chelix_config::ChelixConfig::default();
        config.sandbox.mode = chelix_config::schema::SandboxMode::Off;
        config.auxiliary.title_generation = title_config;
        let state = GatewayState::with_options(
            ResolvedAuth {
                mode: AuthMode::Token,
                token: None,
                password: None,
            },
            services,
            config,
            Arc::new(chelix_tools::sandbox::SandboxRouter::disabled()),
            None,
            false,
            false,
            false,
            None,
            None,
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

        let mut registry = ProviderRegistry::empty();
        registry.register(
            ModelInfo {
                id: "mock-title".to_string(),
                provider: "mock".to_string(),
                metadata: ModelMetadata {
                    context_length: 8_192,
                    max_input_tokens: 7_168,
                    max_output_tokens: 1_024,
                    input_modalities: vec![ModelModality::Text],
                    output_modalities: vec![ModelModality::Text],
                    tool_calling: false,
                    streaming: true,
                    zero_data_retention_enabled: true,
                    reasoning_supported_efforts: vec![supported_effort.into()],
                    reasoning_summary: None,
                    reasoning_include: None,
                },
            },
            provider,
        );
        state.inner.write().await.llm_providers = Some(Arc::new(RwLock::new(registry)));

        session_metadata
            .create_llm_session("session:test", None, &title_pair(), Some("main"))
            .await
            .unwrap();
        session_store
            .append(
                "session:test",
                &serde_json::json!({"role": "user", "content": "How do I deploy Chelix?"}),
            )
            .await
            .unwrap();
        session_store
            .append(
                "session:test",
                &serde_json::json!({"role": "assistant", "content": "Use the Docker image."}),
            )
            .await
            .unwrap();
        (state, dir)
    }

    #[tokio::test]
    async fn generate_title_for_session_applies_auxiliary_effort_and_persists_label() {
        for effort in ["low", "off"] {
            let provider = Arc::new(MockTitleProvider::new(Ok("Docker Deployment"), effort));
            let (state, _dir) =
                test_state(provider.clone(), Some(title_config(effort)), effort).await;

            let title = generate_title_for_session(&state, "session:test")
                .await
                .unwrap();

            assert_eq!(title.as_deref(), Some("Docker Deployment"));
            assert_eq!(provider.calls.load(Ordering::SeqCst), 1);
            let label = state
                .services
                .session_metadata
                .as_ref()
                .unwrap()
                .get("session:test")
                .await
                .unwrap()
                .and_then(|entry| entry.label);
            assert_eq!(label.as_deref(), Some("Docker Deployment"));
        }
    }

    #[tokio::test]
    async fn generate_title_for_session_rejects_invalid_pair_before_llm_and_label_update() {
        let cases = [
            (None, "auxiliary.title_generation is not configured"),
            (Some(title_config("")), "reasoning effort must not be empty"),
            (
                Some(title_config("unsupported")),
                "does not support reasoning effort",
            ),
            (
                Some(ConfigModelOverride {
                    model: "missing::title".to_string(),
                    ..title_config("low")
                }),
                "is not registered",
            ),
            (
                Some(ConfigModelOverride {
                    model: String::new(),
                    ..title_config("low")
                }),
                "is not registered",
            ),
            (
                Some(ConfigModelOverride {
                    model: "mock-title".to_string(),
                    ..title_config("low")
                }),
                "is not canonical",
            ),
        ];
        for (pair, expected_error) in cases {
            let provider = Arc::new(MockTitleProvider::new(Ok("Unused Title"), "low"));
            let (state, _dir) = test_state(provider.clone(), pair, "low").await;
            let metadata = state.services.session_metadata.as_ref().unwrap();
            let before = metadata
                .update_label("session:test", Some("Existing Label"))
                .await
                .unwrap();

            let error = generate_title_for_session(&state, "session:test")
                .await
                .unwrap_err();

            assert!(error.to_string().contains(expected_error), "{error}");
            assert_eq!(provider.calls.load(Ordering::SeqCst), 0);
            let after = metadata.get("session:test").await.unwrap().unwrap();
            assert_eq!(after.label, before.label);
            assert_eq!(after.version, before.version);
        }
    }

    #[tokio::test]
    async fn generate_title_for_session_returns_provider_errors() {
        let provider = Arc::new(MockTitleProvider::new(Err("provider unavailable"), "low"));
        let (state, _dir) = test_state(provider.clone(), Some(title_config("low")), "low").await;

        let err = generate_title_for_session(&state, "session:test")
            .await
            .unwrap_err();

        assert_eq!(err.to_string(), "provider unavailable");
        assert_eq!(provider.calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn generate_title_for_session_skips_main_session() {
        let (state, _dir) = test_state(
            Arc::new(MockTitleProvider::new(Ok("Should Not Be Used"), "low")),
            Some(title_config("low")),
            "low",
        )
        .await;
        let metadata = state.services.session_metadata.as_ref().unwrap();
        let store = state.services.session_store.as_ref().unwrap();

        metadata
            .create_llm_session("main", None, &title_pair(), Some("main"))
            .await
            .unwrap();
        store
            .append(
                "main",
                &serde_json::json!({"role": "user", "content": "How do I configure Chelix?"}),
            )
            .await
            .unwrap();
        store
            .append(
                "main",
                &serde_json::json!({"role": "assistant", "content": "Open the settings page."}),
            )
            .await
            .unwrap();

        let title = generate_title_for_session(&state, "main").await.unwrap();

        assert_eq!(title, None);
        let label = metadata
            .get("main")
            .await
            .unwrap()
            .and_then(|entry| entry.label);
        assert_eq!(label, None);
    }

    #[tokio::test]
    async fn generate_title_for_session_skip_keeps_existing_label() {
        let (state, _dir) = test_state(
            Arc::new(MockTitleProvider::new(Ok("Should Not Be Used"), "low")),
            Some(title_config("low")),
            "low",
        )
        .await;
        let metadata = state.services.session_metadata.as_ref().unwrap();
        metadata
            .create_llm_session(
                "session:short",
                Some("Existing Label"),
                &title_pair(),
                Some("main"),
            )
            .await
            .unwrap();

        let title = generate_title_for_session(&state, "session:short")
            .await
            .unwrap();

        assert_eq!(title, None);
        let label = metadata
            .get("session:short")
            .await
            .unwrap()
            .and_then(|entry| entry.label);
        assert_eq!(label.as_deref(), Some("Existing Label"));
    }
}
