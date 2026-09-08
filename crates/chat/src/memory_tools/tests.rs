#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::{
    pin::Pin,
    sync::{Arc, Mutex},
};

use {
    super::*,
    chelix_agents::{
        model::{StreamEvent, Usage, UserContent},
        tool_context::ToolExecutionContext,
    },
    chelix_common::{ModelMetadata, ModelModality, ReasoningEffort, ResolvedModelReasoning},
    chelix_memory::{
        config::MemoryConfig, embeddings::EmbeddingProvider, manager::MemoryManager,
        schema::run_migrations, store_sqlite::SqliteMemoryStore,
    },
    chelix_providers::ModelInfo,
    chelix_sessions::SessionKey,
    sqlx::SqlitePool,
    tempfile::TempDir,
    tokio_stream::Stream,
};

const KEYWORDS: [&str; 4] = ["dark", "spicy", "duplicate", "forget"];
struct DataDirGuard;

impl Drop for DataDirGuard {
    fn drop(&mut self) {
        chelix_config::clear_data_dir();
    }
}

fn memory_forget_context() -> ToolExecutionContext {
    ToolExecutionContext::for_session(SessionKey::new("agent:writer:main"))
}

#[test]
fn memory_forget_arguments_are_closed_and_match_the_schema() {
    let schema = memory_forget_parameters_schema();
    assert_eq!(schema["additionalProperties"], json!(false));

    let request = parse_forget_request(&json!({
        "request": "forget a saved preference",
        "dry_run": true,
        "limit": MEMORY_FORGET_MAX_LIMIT,
    }))
    .unwrap();
    assert_eq!(request.request, "forget a saved preference");
    assert!(request.dry_run);
    assert_eq!(request.limit, MEMORY_FORGET_MAX_LIMIT);

    let error = parse_forget_request(&json!({
        "request": "forget a saved preference",
        "unexpected": true,
    }))
    .unwrap_err();
    assert!(error.to_string().contains("unknown field `unexpected`"));
}

#[test]
fn memory_forget_arguments_reject_current_semantic_errors() {
    for params in [
        json!({ "request": " " }),
        json!({ "request": "forget a saved preference", "limit": 0 }),
        json!({
            "request": "forget a saved preference",
            "limit": MEMORY_FORGET_MAX_LIMIT + 1,
        }),
    ] {
        assert!(parse_forget_request(&params).is_err(), "accepted {params}");
    }
}

struct MockEmbedder;

#[async_trait]
impl EmbeddingProvider for MockEmbedder {
    async fn embed(&self, text: &str) -> chelix_memory::Result<Vec<f32>> {
        let lower = text.to_lowercase();
        Ok(KEYWORDS
            .iter()
            .map(|keyword| {
                if lower.contains(keyword) {
                    1.0
                } else {
                    0.0
                }
            })
            .collect())
    }

    fn model_name(&self) -> &str {
        "mock-model"
    }

    fn dimensions(&self) -> usize {
        KEYWORDS.len()
    }

    fn provider_key(&self) -> &str {
        "mock"
    }
}

#[derive(Deserialize)]
struct ForgetPromptCandidateOwned {
    chunk_id: String,
    text: String,
}

struct ForgetPlannerProvider {
    needle: String,
}

impl LlmProvider for ForgetPlannerProvider {
    fn name(&self) -> &str {
        "mock-memory-forget"
    }

    fn id(&self) -> &str {
        "mock-memory-forget"
    }

    fn stream_with_tools(
        &self,
        messages: Vec<ChatMessage>,
        _tools: Vec<Value>,
    ) -> Pin<Box<dyn Stream<Item = StreamEvent> + Send + '_>> {
        let user_text = messages
            .iter()
            .find_map(|message| match message {
                ChatMessage::User {
                    content: UserContent::Text(text),
                    ..
                } => Some(text.as_str()),
                _ => None,
            })
            .unwrap_or_default();
        let candidate_json = user_text
            .split("Candidate chunks:\n")
            .nth(1)
            .unwrap_or("[]");
        let candidates: Vec<ForgetPromptCandidateOwned> = match serde_json::from_str(candidate_json)
        {
            Ok(candidates) => candidates,
            Err(error) => {
                return Box::pin(tokio_stream::once(StreamEvent::Error(error.to_string())));
            },
        };
        let actions: Vec<Value> = candidates
            .iter()
            .filter(|candidate| candidate.text.contains(&self.needle))
            .map(|candidate| {
                json!({
                    "chunk_id": candidate.chunk_id.clone(),
                    "reason": format!("matched '{}'", self.needle),
                })
            })
            .collect();

        Box::pin(tokio_stream::iter(vec![
            StreamEvent::Delta(
                json!({
                    "needs_confirmation": false,
                    "rationale": format!("selected chunks containing '{}'", self.needle),
                    "actions": actions,
                })
                .to_string(),
            ),
            StreamEvent::Done(Usage::default()),
        ]))
    }

    fn stream(
        &self,
        messages: Vec<ChatMessage>,
    ) -> Pin<Box<dyn Stream<Item = StreamEvent> + Send + '_>> {
        self.stream_with_tools(messages, Vec::new())
    }

    fn with_reasoning_effort(
        self: Arc<Self>,
        _effort: ReasoningEffort,
    ) -> Option<Arc<dyn LlmProvider>> {
        Some(Arc::new(Self {
            needle: self.needle.clone(),
        }))
    }
}

struct AppliedEffortProvider {
    applied_efforts: Arc<Mutex<Vec<String>>>,
}

impl LlmProvider for AppliedEffortProvider {
    fn name(&self) -> &str {
        "applied-effort"
    }

    fn id(&self) -> &str {
        "model"
    }

    fn supports_tools(&self) -> bool {
        false
    }

    fn stream_with_tools(
        &self,
        _messages: Vec<ChatMessage>,
        _tools: Vec<Value>,
    ) -> Pin<Box<dyn Stream<Item = StreamEvent> + Send + '_>> {
        Box::pin(tokio_stream::once(StreamEvent::Error(
            "unexpected planner invocation".into(),
        )))
    }

    fn stream(
        &self,
        messages: Vec<ChatMessage>,
    ) -> Pin<Box<dyn Stream<Item = StreamEvent> + Send + '_>> {
        self.stream_with_tools(messages, Vec::new())
    }

    fn reasoning_effort(&self) -> Option<ReasoningEffort> {
        self.applied_efforts
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .last()
            .cloned()
            .map(ReasoningEffort::from)
    }

    fn with_reasoning_effort(
        self: Arc<Self>,
        effort: ReasoningEffort,
    ) -> Option<Arc<dyn LlmProvider>> {
        self.applied_efforts
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .push(effort.as_str().to_string());
        Some(self)
    }
}

struct FailingForgetProvider;

impl LlmProvider for FailingForgetProvider {
    fn name(&self) -> &str {
        "failing-memory-forget"
    }

    fn id(&self) -> &str {
        "failing-memory-forget"
    }

    fn stream_with_tools(
        &self,
        _messages: Vec<ChatMessage>,
        _tools: Vec<Value>,
    ) -> Pin<Box<dyn Stream<Item = StreamEvent> + Send + '_>> {
        Box::pin(tokio_stream::once(StreamEvent::Error(
            "simulated memory_forget provider failure".into(),
        )))
    }

    fn stream(
        &self,
        messages: Vec<ChatMessage>,
    ) -> Pin<Box<dyn Stream<Item = StreamEvent> + Send + '_>> {
        self.stream_with_tools(messages, Vec::new())
    }

    fn with_reasoning_effort(
        self: Arc<Self>,
        _effort: ReasoningEffort,
    ) -> Option<Arc<dyn LlmProvider>> {
        Some(Arc::new(Self))
    }
}

fn memory_forget_model_metadata() -> ModelMetadata {
    ModelMetadata {
        context_length: 8_192,
        max_input_tokens: 4_096,
        max_output_tokens: 1_024,
        input_modalities: vec![ModelModality::Text],
        output_modalities: vec![ModelModality::Text],
        tool_calling: false,
        zero_data_retention_enabled: false,
        reasoning_supported_efforts: vec![ReasoningEffort::from("off")],
        reasoning_summary: None,
        reasoning_include: None,
    }
}

async fn setup_session_metadata() -> Arc<SqliteSessionMetadata> {
    let pool = SqlitePool::connect("sqlite::memory:").await.unwrap();
    sqlx::query("CREATE TABLE projects (id TEXT PRIMARY KEY)")
        .execute(&pool)
        .await
        .unwrap();
    chelix_sessions::run_migrations(&pool).await.unwrap();
    Arc::new(SqliteSessionMetadata::new(pool))
}

async fn setup_memory_forget_provider_resolver(
    provider: Arc<dyn LlmProvider>,
) -> (
    MemoryForgetProviderResolver,
    Arc<RwLock<ProviderRegistry>>,
    Arc<SqliteSessionMetadata>,
) {
    let metadata = setup_session_metadata().await;
    let model_reasoning =
        ResolvedModelReasoning::try_new("test::model".to_string(), ReasoningEffort::from("off"))
            .unwrap();
    metadata
        .create_llm_session("agent:writer:main", None, &model_reasoning, Some("writer"))
        .await
        .unwrap();

    let mut registry = ProviderRegistry::empty();
    registry.register(
        ModelInfo {
            id: "model".to_string(),
            provider: "test".to_string(),
            metadata: memory_forget_model_metadata(),
        },
        provider,
    );
    let providers = Arc::new(RwLock::new(registry));
    (
        MemoryForgetProviderResolver::new(Arc::clone(&providers), Arc::clone(&metadata)),
        providers,
        metadata,
    )
}

struct NamedTool(&'static str);

#[async_trait]
impl AgentTool for NamedTool {
    fn name(&self) -> &str {
        self.0
    }

    fn description(&self) -> &str {
        "stub"
    }

    fn parameters_schema(&self) -> Value {
        json!({"type": "object", "properties": {}})
    }

    async fn execute(&self, _params: Value) -> anyhow::Result<Value> {
        Ok(json!({}))
    }
}

async fn setup_agent_memory(
    agent_id: &str,
    content: &str,
    chunk_size: usize,
) -> (
    chelix_memory::runtime::DynMemoryRuntime,
    TempDir,
    std::path::PathBuf,
) {
    let tmp = TempDir::new().unwrap();
    chelix_config::set_data_dir(tmp.path().to_path_buf());

    let workspace = chelix_config::agent_workspace_dir(agent_id);
    std::fs::create_dir_all(workspace.join("memory")).unwrap();
    let memory_path = workspace.join("MEMORY.md");
    std::fs::write(&memory_path, content).unwrap();

    let pool = SqlitePool::connect(":memory:").await.unwrap();
    run_migrations(&pool).await.unwrap();
    let config = MemoryConfig {
        db_path: ":memory:".into(),
        data_dir: Some(tmp.path().to_path_buf()),
        memory_dirs: vec![workspace.join("MEMORY.md"), workspace.join("memory")],
        chunk_size,
        chunk_overlap: 0,
        vector_weight: 0.7,
        keyword_weight: 0.3,
        ..Default::default()
    };

    let manager = Arc::new(MemoryManager::new(
        config,
        Box::new(SqliteMemoryStore::new(pool)),
        Box::new(MockEmbedder),
    ));
    manager.sync().await.unwrap();
    (manager, tmp, memory_path)
}

#[tokio::test]
async fn memory_forget_provider_resolver_applies_persisted_off_pair() {
    let applied_efforts = Arc::new(Mutex::new(Vec::new()));
    let (resolver, _providers, _metadata) =
        setup_memory_forget_provider_resolver(Arc::new(AppliedEffortProvider {
            applied_efforts: Arc::clone(&applied_efforts),
        }))
        .await;

    resolver
        .resolve(&SessionKey::new("agent:writer:main"))
        .await
        .unwrap();

    assert_eq!(
        *applied_efforts
            .lock()
            .unwrap_or_else(|error| error.into_inner()),
        vec!["off".to_string()]
    );
}

#[tokio::test]
async fn memory_forget_provider_resolver_rejects_missing_session_and_invalid_pair() {
    let metadata = setup_session_metadata().await;
    let invalid_session_key = "agent:invalid:main";
    let invalid_pair =
        ResolvedModelReasoning::try_new("test::missing".to_string(), ReasoningEffort::from("off"))
            .unwrap();
    metadata
        .create_llm_session(invalid_session_key, None, &invalid_pair, Some("writer"))
        .await
        .unwrap();
    let resolver = MemoryForgetProviderResolver::new(
        Arc::new(RwLock::new(ProviderRegistry::empty())),
        metadata,
    );

    for (session_key, expected_error) in [
        (
            "agent:missing:main",
            "session 'agent:missing:main' not found",
        ),
        (
            invalid_session_key,
            "model `test::missing` is not registered",
        ),
    ] {
        let Err(error) = resolver.resolve(&SessionKey::new(session_key)).await else {
            panic!("resolver accepted invalid session '{session_key}'");
        };
        assert_eq!(error.to_string(), expected_error);
    }
}

#[tokio::test]
async fn global_memory_forget_delegates_to_provider_resolver() {
    let _lock = crate::DATA_DIR_TEST_LOCK.lock().await;
    let _guard = DataDirGuard;
    let (manager, _tmp, _memory_path) = setup_agent_memory("writer", "", 4).await;
    let (_resolver, providers, metadata) =
        setup_memory_forget_provider_resolver(Arc::new(FailingForgetProvider)).await;
    let tool = MemoryForgetTool::new(manager, providers, metadata);

    let result = tool
        .execute_with_context(
            json!({ "request": "forget dark mode" }),
            &memory_forget_context(),
        )
        .await
        .unwrap();

    assert_eq!(result["candidate_count"], json!(0));
}

#[tokio::test]
async fn memory_forget_propagates_provider_failure() {
    let _lock = crate::DATA_DIR_TEST_LOCK.lock().await;
    let _guard = DataDirGuard;
    let (manager, _tmp, _memory_path) =
        setup_agent_memory("writer", "Color preference dark mode\n", 4).await;
    let (provider_resolver, _providers, _metadata) =
        setup_memory_forget_provider_resolver(Arc::new(FailingForgetProvider)).await;
    let mut registry = ToolRegistry::new();
    registry.register(Box::new(NamedTool("memory_forget")));
    install_agent_scoped_memory_tools(
        &mut registry,
        &manager,
        provider_resolver,
        "writer",
        MemoryStyle::Hybrid,
        AgentMemoryWriteMode::Hybrid,
    );
    let tool = registry.get("memory_forget").unwrap();
    let context = memory_forget_context();

    let error = tool
        .execute_with_context(json!({ "request": "forget dark mode" }), &context)
        .await
        .unwrap_err();

    assert_eq!(
        error.to_string(),
        "simulated memory_forget provider failure"
    );
}

#[tokio::test]
async fn memory_forget_deletes_selected_scoped_chunk() {
    let _lock = crate::DATA_DIR_TEST_LOCK.lock().await;
    let _guard = DataDirGuard;
    let (manager, _tmp, memory_path) = setup_agent_memory(
        "writer",
        "Color preference dark mode\nFood preference spicy food\n",
        4,
    )
    .await;
    let (provider_resolver, _providers, _metadata) =
        setup_memory_forget_provider_resolver(Arc::new(ForgetPlannerProvider {
            needle: "dark mode".to_string(),
        }))
        .await;

    let mut registry = ToolRegistry::new();
    registry.register(Box::new(NamedTool("memory_forget")));
    install_agent_scoped_memory_tools(
        &mut registry,
        &manager,
        provider_resolver,
        "writer",
        MemoryStyle::Hybrid,
        AgentMemoryWriteMode::Hybrid,
    );

    let tool = registry.get("memory_forget").unwrap();
    let context = memory_forget_context();
    let result = tool
        .execute_with_context(
            json!({ "request": "forget that I prefer dark mode" }),
            &context,
        )
        .await
        .unwrap();

    assert_eq!(result["deleted"], json!(true));
    assert_eq!(result["needs_confirmation"], json!(false));
    assert_eq!(
        result["planned_matches"]
            .as_array()
            .map(|items| items.len()),
        Some(1)
    );
    assert!(result["planned_matches"][0].get("path").is_none());

    let updated = std::fs::read_to_string(memory_path).unwrap();
    assert!(!updated.contains("dark mode"));
    assert!(updated.contains("spicy food"));
}

#[tokio::test]
async fn memory_forget_refuses_ambiguous_exact_text() {
    let _lock = crate::DATA_DIR_TEST_LOCK.lock().await;
    let _guard = DataDirGuard;
    let (manager, _tmp, memory_path) = setup_agent_memory(
        "writer",
        "duplicate memory line\nduplicate memory line\n",
        4,
    )
    .await;
    let (provider_resolver, _providers, _metadata) =
        setup_memory_forget_provider_resolver(Arc::new(ForgetPlannerProvider {
            needle: "duplicate".to_string(),
        }))
        .await;

    let mut registry = ToolRegistry::new();
    registry.register(Box::new(NamedTool("memory_forget")));
    install_agent_scoped_memory_tools(
        &mut registry,
        &manager,
        provider_resolver,
        "writer",
        MemoryStyle::Hybrid,
        AgentMemoryWriteMode::Hybrid,
    );

    let tool = registry.get("memory_forget").unwrap();
    let context = memory_forget_context();
    let result = tool
        .execute_with_context(
            json!({ "request": "forget the duplicate memory line" }),
            &context,
        )
        .await
        .unwrap();

    assert_eq!(result["deleted"], json!(false));
    assert_eq!(result["needs_confirmation"], json!(true));
    assert!(!result["issues"].as_array().unwrap().is_empty());

    let updated = std::fs::read_to_string(memory_path).unwrap();
    assert_eq!(updated, "duplicate memory line\nduplicate memory line\n");
}

#[cfg(unix)]
#[tokio::test]
async fn agent_scoped_memory_mutations_reject_symlink_target() {
    use {chelix_agents::memory_writer::MemoryWriter, std::os::unix::fs::symlink};

    let _lock = crate::DATA_DIR_TEST_LOCK.lock().await;
    let _guard = DataDirGuard;
    let (manager, _tmp, memory_path) = setup_agent_memory("writer", "original memory\n", 4).await;
    std::fs::remove_file(&memory_path).unwrap();
    let outside = tempfile::tempdir().unwrap();
    let outside_file = outside.path().join("memory.md");
    std::fs::write(&outside_file, "outside content\n").unwrap();
    symlink(&outside_file, &memory_path).unwrap();

    let writer =
        AgentScopedMemoryWriter::new(manager, "writer".to_string(), AgentMemoryWriteMode::Hybrid);
    let write_result = writer.write_memory("MEMORY.md", "replacement", false).await;
    assert!(write_result.is_err());
    let delete_result = writer
        .delete_memory("MEMORY.md", None, true, false, true)
        .await;
    assert!(delete_result.is_err());
    assert_eq!(
        std::fs::read_to_string(outside_file).unwrap(),
        "outside content\n"
    );
}

#[test]
fn count_exact_occurrences_accepts_line_ending_variants() {
    assert_eq!(count_exact_occurrences("alpha\r\nbeta\r\n", "alpha\n"), 1);
    assert_eq!(count_exact_occurrences("alpha\nbeta\n", "alpha\r\n"), 1);
}

#[tokio::test]
async fn memory_forget_reports_unreadable_files_as_issues() {
    let _lock = crate::DATA_DIR_TEST_LOCK.lock().await;
    let _guard = DataDirGuard;
    let (manager, _tmp, memory_path) =
        setup_agent_memory("writer", "Color preference dark mode\n", 4).await;
    let (provider_resolver, _providers, _metadata) =
        setup_memory_forget_provider_resolver(Arc::new(ForgetPlannerProvider {
            needle: "dark mode".to_string(),
        }))
        .await;

    let mut registry = ToolRegistry::new();
    registry.register(Box::new(NamedTool("memory_forget")));
    install_agent_scoped_memory_tools(
        &mut registry,
        &manager,
        provider_resolver,
        "writer",
        MemoryStyle::Hybrid,
        AgentMemoryWriteMode::Hybrid,
    );

    std::fs::remove_file(&memory_path).unwrap();

    let tool = registry.get("memory_forget").unwrap();
    let context = memory_forget_context();
    let result = tool
        .execute_with_context(
            json!({ "request": "forget that I prefer dark mode" }),
            &context,
        )
        .await
        .unwrap();

    assert_eq!(result["deleted"], json!(false));
    assert_eq!(result["needs_confirmation"], json!(true));
    assert!(result["planned_matches"].as_array().unwrap().is_empty());
    assert!(!result["issues"].as_array().unwrap().is_empty());
}
