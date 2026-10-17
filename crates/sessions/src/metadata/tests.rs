#![allow(clippy::unwrap_used, clippy::expect_used)]

use super::*;

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
async fn concurrent_ensure_creates_one_valid_row_and_one_event() {
    let pool = sqlite_pool().await;
    let event_bus = crate::session_events::SessionEventBus::new();
    let mut events = event_bus.subscribe();
    let metadata = SqliteSessionMetadata::with_event_bus(pool, event_bus);
    let model_reasoning = pair("test::concurrent", "low");

    let (first, second) = tokio::join!(
        metadata.ensure_session(
            "session:concurrent",
            Some("Concurrent"),
            &model_reasoning,
            Some("main"),
        ),
        metadata.ensure_session(
            "session:concurrent",
            Some("Concurrent"),
            &model_reasoning,
            Some("main"),
        ),
    );
    let first = first.unwrap();
    let second = second.unwrap();
    assert_ne!(first.created(), second.created());
    assert_eq!(first.entry().model_reasoning(), &model_reasoning);
    assert_eq!(second.entry().model_reasoning(), &model_reasoning);
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
async fn schema_rejects_invalid_model_reasoning_rows() {
    let pool = sqlite_pool().await;
    let cases = [
        ("partial model", Some("test::model"), None),
        ("partial reasoning", None, Some("low")),
        ("empty model", Some(""), Some("low")),
        ("empty reasoning", Some("test::model"), Some("")),
        ("no model/reasoning pair", None, None),
    ];

    for (name, model, reasoning) in cases {
        let result = sqlx::query(
            r#"INSERT INTO sessions (
                   key, id, model, reasoning_effort, created_at, updated_at
               ) VALUES (?, ?, ?, ?, 1, 1)"#,
        )
        .bind(format!("session:{name}"))
        .bind(format!("id:{name}"))
        .bind(model)
        .bind(reasoning)
        .execute(&pool)
        .await;
        assert!(result.is_err(), "{name} unexpectedly passed");
    }
}

#[tokio::test]
async fn agent_reassignment_replaces_the_complete_pair_atomically() {
    let metadata = SqliteSessionMetadata::new(sqlite_pool().await);
    let original = pair("test::original", "low");
    let replacement = pair("test::replacement", "high");
    metadata
        .create_session("session:agent", None, &original, Some("first"))
        .await
        .unwrap();

    let entry = metadata
        .assign_agent("session:agent", "second", &replacement)
        .await
        .unwrap();
    assert_eq!(entry.agent_id.as_deref(), Some("second"));
    assert_eq!(entry.model(), "test::replacement");
    assert_eq!(entry.reasoning_effort().as_str(), "high");

    let missing = metadata
        .assign_agent("session:missing", "second", &original)
        .await;
    assert!(missing.is_err());
    let unchanged = metadata.get("session:agent").await.unwrap().unwrap();
    assert_eq!(unchanged.model(), "test::replacement");
}

#[tokio::test]
async fn atomic_patch_rolls_back_every_field_on_late_storage_error() {
    let pool = sqlite_pool().await;
    let metadata = SqliteSessionMetadata::new(pool.clone());
    let original = pair("test::original", "low");
    metadata
        .create_session("session:patch", Some("Original"), &original, Some("main"))
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
            parent_session_key: None,
            tool_permission_mode: None,
            tool_permission_type: None,
        })
        .await;
    assert!(result.is_err());

    let after = metadata.get("session:patch").await.unwrap().unwrap();
    assert_eq!(after.id, before.id);
    assert_eq!(after.label, before.label);
    assert_eq!(after.model_reasoning, before.model_reasoning);
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
        .create_session(
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
async fn label_update_never_creates_a_session() {
    let metadata = SqliteSessionMetadata::new(sqlite_pool().await);
    let llm_pair = pair("test::model", "none");
    metadata
        .create_session("session:label", None, &llm_pair, Some("main"))
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

#[tokio::test]
async fn new_session_defaults_tool_permission_to_auto_manual() {
    let metadata = SqliteSessionMetadata::new(sqlite_pool().await);
    let entry = metadata
        .create_session(
            "session:perm",
            None,
            &pair("test::model", "low"),
            Some("main"),
        )
        .await
        .unwrap();
    assert_eq!(entry.tool_permission_mode, ToolPermissionMode::Auto);
    assert_eq!(entry.tool_permission_type, ToolPermissionType::Manual);
}

#[tokio::test]
async fn patch_updates_tool_permission_mode_and_type() {
    let metadata = SqliteSessionMetadata::new(sqlite_pool().await);
    metadata
        .create_session(
            "session:perm",
            None,
            &pair("test::model", "low"),
            Some("main"),
        )
        .await
        .unwrap();
    let entry = metadata
        .patch_session("session:perm", SessionMetadataPatch {
            tool_permission_mode: Some(ToolPermissionMode::Moderated),
            tool_permission_type: Some(ToolPermissionType::Manual),
            ..SessionMetadataPatch::default()
        })
        .await
        .unwrap();
    assert_eq!(entry.tool_permission_mode, ToolPermissionMode::Moderated);
    assert_eq!(entry.tool_permission_type, ToolPermissionType::Manual);
    let loaded = metadata.get("session:perm").await.unwrap().unwrap();
    assert_eq!(loaded.tool_permission_mode, ToolPermissionMode::Moderated);
    assert_eq!(loaded.tool_permission_type, ToolPermissionType::Manual);
}
