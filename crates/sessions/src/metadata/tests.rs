#![allow(clippy::unwrap_used, clippy::expect_used)]

use {super::*, sqlx::Row};

fn pair(model: &str, effort: &str) -> ResolvedModelReasoning {
    ResolvedModelReasoning::try_new(model.to_string(), ReasoningEffort::from(effort)).unwrap()
}

async fn sqlite_pool() -> sqlx::SqlitePool {
    let pool = sqlx::SqlitePool::connect("sqlite::memory:").await.unwrap();
    sqlx::query("CREATE TABLE projects (id TEXT PRIMARY KEY)")
        .execute(&pool)
        .await
        .unwrap();
    SqliteSessionMetadata::init(&pool).await.unwrap();
    pool
}

#[tokio::test]
async fn storage_accepts_exactly_three_backing_states() {
    let metadata = SqliteSessionMetadata::new(sqlite_pool().await);
    let llm_pair = pair("test::llm", "low");

    let llm = metadata
        .create_llm_session("session:llm", None, &llm_pair, Some("main"))
        .await
        .unwrap();
    assert!(matches!(llm.backing, SessionBacking::Llm { .. }));

    let external = metadata
        .bind_external(
            "session:external",
            None,
            &ExternalSessionIdentity::new(ExternalAgentKind::Codex, None),
        )
        .await
        .unwrap();
    assert!(matches!(external.backing, SessionBacking::External { .. }));

    let llm_external = metadata
        .bind_external(
            "session:llm",
            None,
            &ExternalSessionIdentity::new(ExternalAgentKind::Acp, Some("external-1".to_string())),
        )
        .await
        .unwrap();
    assert!(matches!(
        llm_external.backing,
        SessionBacking::LlmExternal { .. }
    ));
    assert_eq!(llm_external.model(), Some("test::llm"));
    assert_eq!(
        llm_external.reasoning_effort().map(ReasoningEffort::as_str),
        Some("low")
    );
}

#[tokio::test]
async fn concurrent_ensure_creates_one_valid_row_and_one_event() {
    let pool = sqlite_pool().await;
    let event_bus = crate::session_events::SessionEventBus::new();
    let mut events = event_bus.subscribe();
    let metadata = SqliteSessionMetadata::with_event_bus(pool, event_bus);
    let model_reasoning = pair("test::concurrent", "low");

    let (first, second) = tokio::join!(
        metadata.ensure_llm_session(
            "session:concurrent",
            Some("Concurrent"),
            &model_reasoning,
            Some("main"),
        ),
        metadata.ensure_llm_session(
            "session:concurrent",
            Some("Concurrent"),
            &model_reasoning,
            Some("main"),
        ),
    );
    let first = first.unwrap();
    let second = second.unwrap();
    assert_ne!(first.created(), second.created());
    assert!(first.entry().model_reasoning().is_some());
    assert!(second.entry().model_reasoning().is_some());
    assert_eq!(
        metadata
            .list()
            .await
            .unwrap()
            .into_iter()
            .filter(|entry| entry.key == "session:concurrent")
            .count(),
        1
    );
    assert!(matches!(
        events.recv().await.unwrap(),
        crate::session_events::SessionEvent::Created { session_key }
            if session_key == "session:concurrent"
    ));
    assert!(matches!(
        events.try_recv(),
        Err(tokio::sync::broadcast::error::TryRecvError::Empty)
    ));
}

#[tokio::test]
async fn promote_external_to_llm_preserves_identity_and_sets_agent() {
    let metadata = SqliteSessionMetadata::new(sqlite_pool().await);
    metadata
        .bind_external(
            "session:external-promotion",
            None,
            &ExternalSessionIdentity::new(ExternalAgentKind::Codex, Some("external-1".to_string())),
        )
        .await
        .unwrap();

    let outcome = metadata
        .promote_external_to_llm(
            "session:external-promotion",
            &pair("test::promoted", "high"),
            "main",
        )
        .await
        .unwrap();
    let entry = outcome.into_entry();
    assert!(matches!(entry.backing, SessionBacking::LlmExternal { .. }));
    assert_eq!(entry.model(), Some("test::promoted"));
    assert_eq!(
        entry.reasoning_effort().map(ReasoningEffort::as_str),
        Some("high")
    );
    assert_eq!(entry.external_agent_kind(), Some(ExternalAgentKind::Codex));
    assert_eq!(entry.external_session_id(), Some("external-1"));
    assert_eq!(entry.agent_id.as_deref(), Some("main"));

    let second = metadata
        .promote_external_to_llm(
            "session:external-promotion",
            &pair("test::ignored", "low"),
            "other",
        )
        .await
        .unwrap()
        .into_entry();
    assert_eq!(second.model(), Some("test::promoted"));
    assert_eq!(second.agent_id.as_deref(), Some("main"));
}

#[tokio::test]
async fn strict_external_transitions_reject_stale_version_without_mutation() {
    let metadata = SqliteSessionMetadata::new(sqlite_pool().await);
    let external = metadata
        .bind_external(
            "session:strict-external",
            None,
            &ExternalSessionIdentity::new(ExternalAgentKind::Codex, None),
        )
        .await
        .unwrap();
    let updated = metadata
        .update_external_session_id("session:strict-external", Some("external-2"))
        .await
        .unwrap();

    let result = metadata
        .replace_external_with_llm(
            "session:strict-external",
            external.version,
            "main",
            &pair("test::model", "low"),
        )
        .await;
    assert!(result.is_err());
    let unchanged = metadata
        .get("session:strict-external")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(unchanged.version, updated.version);
    assert!(matches!(unchanged.backing, SessionBacking::External { .. }));
    assert_eq!(unchanged.external_session_id(), Some("external-2"));
}

#[tokio::test]
async fn explicit_llm_agent_assignment_clears_external_identity() {
    let metadata = SqliteSessionMetadata::new(sqlite_pool().await);
    metadata
        .bind_external(
            "session:explicit-agent",
            None,
            &ExternalSessionIdentity::new(ExternalAgentKind::Acp, Some("external-1".to_string())),
        )
        .await
        .unwrap();

    let entry = metadata
        .create_or_assign_agent(
            "session:explicit-agent",
            "main",
            &pair("test::selected", "high"),
        )
        .await
        .unwrap();
    assert!(matches!(entry.backing, SessionBacking::Llm { .. }));
    assert_eq!(entry.model(), Some("test::selected"));
    assert_eq!(entry.agent_id.as_deref(), Some("main"));
    assert_eq!(entry.external_agent_kind(), None);
    assert_eq!(entry.external_session_id(), None);
}

#[tokio::test]
async fn schema_rejects_invalid_backing_rows() {
    let pool = sqlite_pool().await;
    let cases = [
        ("partial model", Some("test::model"), None, None, None),
        ("partial reasoning", None, Some("low"), None, None),
        ("empty model", Some(""), Some("low"), None, None),
        ("empty reasoning", Some("test::model"), Some(""), None, None),
        ("no backing", None, None, None, None),
        ("empty external kind", None, None, Some(""), None),
        ("unknown external kind", None, None, Some("unknown"), None),
        ("orphan external ID", None, None, None, Some("external-1")),
    ];

    for (name, model, reasoning, kind, external_id) in cases {
        let result = sqlx::query(
            r#"INSERT INTO sessions (
                   key, id, model, reasoning_effort, created_at, updated_at,
                   external_agent_kind, external_session_id
               ) VALUES (?, ?, ?, ?, 1, 1, ?, ?)"#,
        )
        .bind(format!("session:{name}"))
        .bind(format!("id:{name}"))
        .bind(model)
        .bind(reasoning)
        .bind(kind)
        .bind(external_id)
        .execute(&pool)
        .await;
        assert!(result.is_err(), "{name} unexpectedly passed");
    }
}

#[tokio::test]
async fn unbind_updates_llm_external_and_deletes_external_only() {
    let metadata = SqliteSessionMetadata::new(sqlite_pool().await);
    let llm_pair = pair("test::model", "off");
    metadata
        .create_llm_session("session:llm", None, &llm_pair, Some("main"))
        .await
        .unwrap();
    metadata
        .bind_external(
            "session:llm",
            None,
            &ExternalSessionIdentity::new(ExternalAgentKind::Codex, None),
        )
        .await
        .unwrap();
    metadata
        .bind_external(
            "session:external",
            None,
            &ExternalSessionIdentity::new(ExternalAgentKind::Acp, None),
        )
        .await
        .unwrap();

    let retained = metadata
        .unbind_external("session:llm")
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(retained.backing, SessionBacking::Llm { .. }));
    assert!(
        metadata
            .unbind_external("session:external")
            .await
            .unwrap()
            .is_none()
    );
    assert!(metadata.get("session:external").await.unwrap().is_none());
}

#[tokio::test]
async fn agent_reassignment_replaces_the_complete_pair_atomically() {
    let metadata = SqliteSessionMetadata::new(sqlite_pool().await);
    let original = pair("test::original", "low");
    let replacement = pair("test::replacement", "high");
    metadata
        .create_llm_session("session:agent", None, &original, Some("first"))
        .await
        .unwrap();

    let entry = metadata
        .assign_agent("session:agent", "second", &replacement)
        .await
        .unwrap();
    assert_eq!(entry.agent_id.as_deref(), Some("second"));
    assert_eq!(entry.model(), Some("test::replacement"));
    assert_eq!(
        entry.reasoning_effort().map(ReasoningEffort::as_str),
        Some("high")
    );

    let missing = metadata
        .assign_agent("session:missing", "second", &original)
        .await;
    assert!(missing.is_err());
    let unchanged = metadata.get("session:agent").await.unwrap().unwrap();
    assert_eq!(unchanged.model(), Some("test::replacement"));
}

#[tokio::test]
async fn atomic_patch_rolls_back_every_field_on_late_storage_error() {
    let pool = sqlite_pool().await;
    let metadata = SqliteSessionMetadata::new(pool.clone());
    let original = pair("test::original", "low");
    metadata
        .create_llm_session("session:patch", Some("Original"), &original, Some("main"))
        .await
        .unwrap();
    sqlx::query(
        r#"CREATE TRIGGER reject_missing_project
           BEFORE UPDATE ON sessions
           WHEN NEW.project_id = 'missing'
           BEGIN
               SELECT RAISE(ABORT, 'missing project');
           END"#,
    )
    .execute(&pool)
    .await
    .unwrap();
    let before = metadata.get("session:patch").await.unwrap().unwrap();

    let result = metadata
        .patch_session("session:patch", SessionMetadataPatch {
            label: Some("Changed".to_string()),
            model_reasoning: Some(pair("test::replacement", "high")),
            archived: Some(true),
            project_id: Some(Some("missing".to_string())),
            worktree_branch: Some(Some("changed".to_string())),
            mcp_disabled: Some(Some(true)),
            parent_session_key: None,
        })
        .await;
    assert!(result.is_err());

    let after = metadata.get("session:patch").await.unwrap().unwrap();
    assert_eq!(after.id, before.id);
    assert_eq!(after.label, before.label);
    assert_eq!(after.backing, before.backing);
    assert_eq!(after.created_at, before.created_at);
    assert_eq!(after.updated_at, before.updated_at);
    assert_eq!(after.message_count, before.message_count);
    assert_eq!(
        after.last_seen_message_count,
        before.last_seen_message_count
    );
    assert_eq!(after.project_id, before.project_id);
    assert_eq!(after.archived, before.archived);
    assert_eq!(after.worktree_branch, before.worktree_branch);
    assert_eq!(after.channel_binding, before.channel_binding);
    assert_eq!(after.parent_session_key, before.parent_session_key);
    assert_eq!(after.sandbox_owner_key, before.sandbox_owner_key);
    assert_eq!(after.fork_point, before.fork_point);
    assert_eq!(after.mcp_disabled, before.mcp_disabled);
    assert_eq!(after.preview, before.preview);
    assert_eq!(after.agent_id, before.agent_id);
    assert_eq!(after.prompt_profile, before.prompt_profile);
    assert_eq!(after.version, before.version);
}

#[tokio::test]
async fn empty_patch_is_a_no_op_without_event_or_version_bump() {
    let pool = sqlite_pool().await;
    let event_bus = crate::session_events::SessionEventBus::new();
    let metadata = SqliteSessionMetadata::with_event_bus(pool, event_bus.clone());
    metadata
        .create_llm_session(
            "session:no-op-patch",
            None,
            &pair("test::model", "low"),
            Some("main"),
        )
        .await
        .unwrap();
    let mut events = event_bus.subscribe();
    let before = metadata.get("session:no-op-patch").await.unwrap().unwrap();

    let after = metadata
        .patch_session("session:no-op-patch", SessionMetadataPatch::default())
        .await
        .unwrap();
    assert_eq!(after.version, before.version);
    assert_eq!(after.updated_at, before.updated_at);
    assert!(matches!(
        events.try_recv(),
        Err(tokio::sync::broadcast::error::TryRecvError::Empty)
    ));
}

#[tokio::test]
async fn session_tree_delete_rolls_back_rows_and_mappings() {
    let pool = sqlite_pool().await;
    let metadata = SqliteSessionMetadata::new(pool.clone());
    let model_reasoning = pair("test::delete", "off");
    metadata
        .create_llm_session("session:root", None, &model_reasoning, Some("main"))
        .await
        .unwrap();
    metadata
        .create_llm_session("session:child", None, &model_reasoning, Some("main"))
        .await
        .unwrap();
    metadata
        .set_parent("session:child", Some("session:root"), None)
        .await
        .unwrap();
    metadata
        .set_active_session("telegram", "account", "chat", None, "session:child")
        .await
        .unwrap();
    sqlx::query(
        r#"CREATE TRIGGER reject_session_delete
           BEFORE DELETE ON sessions
           WHEN OLD.key = 'session:root'
           BEGIN
               SELECT RAISE(ABORT, 'delete rejected');
           END"#,
    )
    .execute(&pool)
    .await
    .unwrap();

    let result = metadata
        .remove_session_tree("session:root", &[
            "session:child".to_string(),
            "session:root".to_string(),
        ])
        .await;
    assert!(result.is_err());
    assert!(metadata.get("session:root").await.unwrap().is_some());
    assert!(metadata.get("session:child").await.unwrap().is_some());
    assert_eq!(
        metadata
            .get_active_session("telegram", "account", "chat", None)
            .await
            .unwrap()
            .as_deref(),
        Some("session:child")
    );
}

#[tokio::test]
async fn label_update_never_creates_a_session() {
    let metadata = SqliteSessionMetadata::new(sqlite_pool().await);
    let llm_pair = pair("test::model", "none");
    metadata
        .create_llm_session("session:label", None, &llm_pair, Some("main"))
        .await
        .unwrap();

    let updated = metadata
        .update_label("session:label", Some("Updated"))
        .await
        .unwrap();
    assert_eq!(updated.label.as_deref(), Some("Updated"));
    assert!(
        metadata
            .update_label("session:missing", Some("Unexpected"))
            .await
            .is_err()
    );
    assert!(metadata.get("session:missing").await.unwrap().is_none());
}

async fn legacy_sessions_pool() -> sqlx::SqlitePool {
    let pool = sqlx::SqlitePool::connect("sqlite::memory:").await.unwrap();
    sqlx::query("CREATE TABLE projects (id TEXT PRIMARY KEY)")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query(
        r#"CREATE TABLE sessions (
            key TEXT PRIMARY KEY,
            id TEXT NOT NULL,
            label TEXT,
            model TEXT,
            reasoning_effort TEXT,
            created_at INTEGER NOT NULL,
            updated_at INTEGER NOT NULL,
            message_count INTEGER NOT NULL DEFAULT 0,
            last_seen_message_count INTEGER NOT NULL DEFAULT 0,
            project_id TEXT REFERENCES projects(id) ON DELETE SET NULL,
            archived INTEGER NOT NULL DEFAULT 0,
            worktree_branch TEXT,
            channel_binding TEXT,
            parent_session_key TEXT,
            sandbox_owner_key TEXT,
            fork_point INTEGER,
            mcp_disabled INTEGER,
            preview TEXT,
            agent_id TEXT,
            prompt_profile TEXT NOT NULL DEFAULT 'chat',
            external_agent_kind TEXT,
            external_session_id TEXT,
            version INTEGER NOT NULL DEFAULT 0
        )"#,
    )
    .execute(&pool)
    .await
    .unwrap();
    pool
}

#[tokio::test]
async fn migration_copies_valid_rows_unchanged_and_rejects_invalid_rows() {
    let valid_pool = legacy_sessions_pool().await;
    sqlx::query(
        r#"INSERT INTO sessions (
               key, id, label, model, reasoning_effort, created_at, updated_at,
               message_count, last_seen_message_count, archived, prompt_profile,
               external_agent_kind, external_session_id, version
           ) VALUES ('session:valid', 'id-valid', 'Valid', 'test::model', 'off',
                     10, 20, 3, 2, 1, 'subagent', 'codex', 'external-1', 7)"#,
    )
    .execute(&valid_pool)
    .await
    .unwrap();
    sqlx::raw_sql(include_str!(
        "../../migrations/20260831023000_session_backing_constraints.sql"
    ))
    .execute(&valid_pool)
    .await
    .unwrap();
    let copied = sqlx::query(
        r#"SELECT key, id, label, model, reasoning_effort, created_at, updated_at,
                  message_count, last_seen_message_count, archived, prompt_profile,
                  external_agent_kind, external_session_id, version
           FROM sessions WHERE key = 'session:valid'"#,
    )
    .fetch_one(&valid_pool)
    .await
    .unwrap();
    assert_eq!(copied.get::<String, _>("key"), "session:valid");
    assert_eq!(copied.get::<String, _>("id"), "id-valid");
    assert_eq!(
        copied.get::<Option<String>, _>("label").as_deref(),
        Some("Valid")
    );
    assert_eq!(
        copied.get::<Option<String>, _>("model").as_deref(),
        Some("test::model")
    );
    assert_eq!(
        copied
            .get::<Option<String>, _>("reasoning_effort")
            .as_deref(),
        Some("off")
    );
    assert_eq!(copied.get::<i64, _>("created_at"), 10);
    assert_eq!(copied.get::<i64, _>("updated_at"), 20);
    assert_eq!(copied.get::<i64, _>("message_count"), 3);
    assert_eq!(copied.get::<i64, _>("last_seen_message_count"), 2);
    assert_eq!(copied.get::<i64, _>("archived"), 1);
    assert_eq!(copied.get::<String, _>("prompt_profile"), "subagent");
    assert_eq!(
        copied
            .get::<Option<String>, _>("external_agent_kind")
            .as_deref(),
        Some("codex")
    );
    assert_eq!(
        copied
            .get::<Option<String>, _>("external_session_id")
            .as_deref(),
        Some("external-1")
    );
    assert_eq!(copied.get::<i64, _>("version"), 7);

    let invalid_pool = legacy_sessions_pool().await;
    sqlx::query(
        "INSERT INTO sessions (key, id, created_at, updated_at) VALUES ('invalid', 'id', 1, 1)",
    )
    .execute(&invalid_pool)
    .await
    .unwrap();
    let migration = sqlx::raw_sql(include_str!(
        "../../migrations/20260831023000_session_backing_constraints.sql"
    ))
    .execute(&invalid_pool)
    .await;
    assert!(migration.is_err());
    let unchanged: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM sessions WHERE key = 'invalid'")
        .fetch_one(&invalid_pool)
        .await
        .unwrap();
    assert_eq!(unchanged, 1);
}
