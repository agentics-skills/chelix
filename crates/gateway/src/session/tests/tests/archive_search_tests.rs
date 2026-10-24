use super::*;

enum ArchiveChat {
    Count(Arc<std::sync::atomic::AtomicUsize>),
    Wait {
        entered: Arc<tokio::sync::Notify>,
        release: Arc<tokio::sync::Notify>,
        mark: Arc<std::sync::atomic::AtomicBool>,
    },
    Fail,
}

#[async_trait::async_trait]
impl chelix_service_traits::ChatService for ArchiveChat {
    async fn send(
        &self,
        _request: chelix_service_traits::ChatSendRequest,
        _context: chelix_service_traits::ChatExecutionContext,
    ) -> ServiceResult {
        Err("chat not configured".into())
    }

    async fn send_sync(
        &self,
        _request: chelix_service_traits::ChatSendSyncRequest,
        _context: chelix_service_traits::ChatExecutionContext,
    ) -> ServiceResult {
        Err("chat not configured".into())
    }

    async fn abort(&self, _params: Value) -> ServiceResult {
        match self {
            Self::Count(calls) => {
                calls.fetch_add(1, Ordering::SeqCst);
                Ok(serde_json::json!({}))
            },
            Self::Wait {
                entered,
                release,
                mark,
            } => {
                let wait = release.notified();
                mark.store(true, Ordering::SeqCst);
                entered.notify_one();
                wait.await;
                Ok(serde_json::json!({}))
            },
            Self::Fail => Err(ServiceError::message("stop failed")),
        }
    }

    async fn history(&self, _params: Value) -> ServiceResult {
        Ok(serde_json::json!([]))
    }

    async fn inject(&self, _params: Value) -> ServiceResult {
        Err("chat not configured".into())
    }

    async fn clear(&self, _params: Value) -> ServiceResult {
        Ok(serde_json::json!({ "ok": true }))
    }

    async fn compact(
        &self,
        _request: chelix_service_traits::ChatCompactRequest,
        _context: chelix_service_traits::ChatExecutionContext,
    ) -> ServiceResult {
        Err("chat not configured".into())
    }

    async fn context(
        &self,
        _request: chelix_service_traits::ChatContextRequest,
        _context: chelix_service_traits::ChatExecutionContext,
    ) -> ServiceResult {
        Ok(serde_json::json!({ "session": {}, "project": null, "tools": [], "providers": [] }))
    }

    async fn raw_prompt(
        &self,
        _request: chelix_service_traits::ChatRawPromptRequest,
        _context: chelix_service_traits::ChatExecutionContext,
    ) -> ServiceResult {
        Err("chat not configured".into())
    }

    async fn full_context(
        &self,
        _request: chelix_service_traits::ChatFullContextRequest,
        _context: chelix_service_traits::ChatExecutionContext,
    ) -> ServiceResult {
        Err("chat not configured".into())
    }
}

#[tokio::test]
async fn archive_patch_error_does_not_call_stop_session() {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(SessionStore::new(dir.path().to_path_buf()));
    let pool = sqlite_pool().await;
    let metadata = Arc::new(SqliteSessionMetadata::new(pool));
    let binding = r#"{"channel_type":"telegram","account_id":"bot1","chat_id":"123"}"#.to_string();
    create_test_session(&metadata, "telegram:bot1:123", Some("Telegram current")).await;
    metadata
        .set_channel_binding("telegram:bot1:123", Some(&binding))
        .await
        .unwrap();
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let svc = LiveSessionService::new(store, metadata)
        .with_chat(Arc::new(ArchiveChat::Count(Arc::clone(&calls))));
    let error = svc
        .patch(serde_json::json!({ "key": "telegram:bot1:123", "archived": true }))
        .await
        .unwrap_err();
    assert!(error.to_string().contains("cannot be archived"));
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn patch_archived_allows_unarchive_for_current_channel_session() {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(SessionStore::new(dir.path().to_path_buf()));
    let pool = sqlite_pool().await;
    let metadata = Arc::new(SqliteSessionMetadata::new(pool));
    let binding = r#"{"channel_type":"telegram","account_id":"bot1","chat_id":"123"}"#.to_string();
    create_test_session(&metadata, "telegram:bot1:123", Some("Telegram current")).await;
    metadata
        .set_channel_binding("telegram:bot1:123", Some(&binding))
        .await
        .unwrap();
    metadata
        .set_archived("telegram:bot1:123", true)
        .await
        .unwrap();

    let svc = LiveSessionService::new(Arc::clone(&store), Arc::clone(&metadata));

    let result = svc
        .patch(serde_json::json!({ "key": "telegram:bot1:123", "archived": false }))
        .await
        .unwrap();
    assert_eq!(
        result.get("archived").and_then(|v| v.as_bool()),
        Some(false)
    );
    assert!(
        !metadata
            .get("telegram:bot1:123")
            .await
            .unwrap()
            .unwrap()
            .archived
    );
}

#[tokio::test]
async fn patch_archived_rejection_does_not_partially_mutate_session() {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(SessionStore::new(dir.path().to_path_buf()));
    let pool = sqlite_pool().await;
    let metadata = Arc::new(SqliteSessionMetadata::new(pool));
    create_test_session(&metadata, "t:1", Some("Main")).await;

    let svc = LiveSessionService::new(Arc::clone(&store), Arc::clone(&metadata));

    let error = svc
        .patch(serde_json::json!({
            "key": "t:1",
            "label": "Mutated?",
            "model": "gpt-5",
            "archived": true
        }))
        .await
        .unwrap_err();

    assert!(
        error
            .to_string()
            .contains("archive accepts only key and archived")
    );

    let entry = metadata.get("t:1").await.unwrap().unwrap();
    assert_eq!(entry.label.as_deref(), Some("Main"));
    assert_eq!(entry.model(), "example-patch::reasoning");
    assert!(!entry.archived);
}

#[tokio::test]
async fn search_excludes_archived_sessions_unless_requested() {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(SessionStore::new(dir.path().to_path_buf()));
    let pool = sqlite_pool().await;
    let metadata = Arc::new(SqliteSessionMetadata::new(pool));
    create_test_session(&metadata, "session:visible", Some("Visible")).await;
    create_test_session(&metadata, "session:hidden", Some("Hidden")).await;
    metadata.set_archived("session:hidden", true).await.unwrap();
    store
        .append(
            "session:visible",
            &serde_json::json!({"role": "user", "content": "archive needle visible"}),
        )
        .await
        .unwrap();
    store
        .append(
            "session:hidden",
            &serde_json::json!({"role": "user", "content": "archive needle hidden"}),
        )
        .await
        .unwrap();

    let svc = LiveSessionService::new(Arc::clone(&store), Arc::clone(&metadata));

    let default_results = svc
        .search(serde_json::json!({ "query": "needle", "limit": 10 }))
        .await
        .unwrap()
        .as_array()
        .cloned()
        .unwrap();
    assert_eq!(default_results.len(), 1);
    assert_eq!(default_results[0]["sessionKey"], "session:visible");
    assert_eq!(default_results[0]["archived"], false);

    let include_archived_results = svc
        .search(serde_json::json!({
            "query": "needle",
            "limit": 10,
            "includeArchived": true
        }))
        .await
        .unwrap()
        .as_array()
        .cloned()
        .unwrap();
    assert_eq!(include_archived_results.len(), 2);
    assert!(
        include_archived_results
            .iter()
            .any(|entry| entry["sessionKey"] == "session:hidden" && entry["archived"] == true)
    );
}

#[tokio::test]
async fn search_excludes_results_without_metadata_rows() {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(SessionStore::new(dir.path().to_path_buf()));
    let pool = sqlite_pool().await;
    let metadata = Arc::new(SqliteSessionMetadata::new(pool));
    store
        .append(
            "session:orphaned",
            &serde_json::json!({"role": "user", "content": "needle without metadata"}),
        )
        .await
        .unwrap();

    let svc = LiveSessionService::new(Arc::clone(&store), Arc::clone(&metadata));

    let results = svc
        .search(serde_json::json!({ "query": "needle", "limit": 10 }))
        .await
        .unwrap()
        .as_array()
        .cloned()
        .unwrap();
    assert!(results.is_empty());
}

fn archive_router(backend: Arc<RecordingSandbox>) -> Arc<SandboxRouter> {
    Arc::new(
        SandboxRouter::with_backend(
            chelix_tools::sandbox::SandboxConfig::default(),
            backend,
            Some(Arc::new(FailingSandboxOwnerResolver)),
        )
        .unwrap(),
    )
}

#[tokio::test(flavor = "current_thread")]
async fn archive_patch_error_does_not_stop_sandbox() {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(SessionStore::new(dir.path().to_path_buf()));
    let metadata = Arc::new(SqliteSessionMetadata::new(sqlite_pool().await));
    let binding = r#"{"channel_type":"telegram","account_id":"bot1","chat_id":"123"}"#.to_string();
    create_test_session(&metadata, "telegram:bot1:123", Some("Telegram current")).await;
    metadata
        .set_channel_binding("telegram:bot1:123", Some(&binding))
        .await
        .unwrap();
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let backend = Arc::new(RecordingSandbox::new());
    let router = archive_router(Arc::clone(&backend));
    let watch = router.watch_background_stops();
    let svc = LiveSessionService::new(store, metadata)
        .with_chat(Arc::new(ArchiveChat::Count(Arc::clone(&calls))))
        .with_sandbox_router(router);
    let error = svc
        .patch(serde_json::json!({ "key": "telegram:bot1:123", "archived": true }))
        .await
        .unwrap_err();
    watch.finish().await;
    assert!(error.to_string().contains("cannot be archived"));
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert_eq!(backend.stops.load(Ordering::SeqCst), 0);
}

#[tokio::test(flavor = "current_thread")]
async fn archive_calls_stop_session_before_background_stop() {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(SessionStore::new(dir.path().to_path_buf()));
    let metadata = Arc::new(SqliteSessionMetadata::new(sqlite_pool().await));
    create_test_session(&metadata, "session:owner", Some("Owner")).await;
    let release = Arc::new(tokio::sync::Notify::new());
    let entered = Arc::new(tokio::sync::Notify::new());
    let backend = Arc::new(RecordingSandbox::new());
    let router = archive_router(Arc::clone(&backend));
    let watch = router.watch_background_stops();
    let svc = LiveSessionService::new(store, Arc::clone(&metadata))
        .with_chat(Arc::new(ArchiveChat::Wait {
            entered: Arc::clone(&entered),
            release: Arc::clone(&release),
            mark: Arc::clone(&backend.mark),
        }))
        .with_sandbox_router(router);
    let entered_wait = entered.notified();
    let archived = tokio::spawn(async move {
        svc.patch(serde_json::json!({ "key": "session:owner", "archived": true }))
            .await
    });
    tokio::time::timeout(std::time::Duration::from_secs(2), entered_wait)
        .await
        .expect("session stop did not start");
    assert_eq!(backend.stops.load(Ordering::SeqCst), 0);
    let stopped = backend.stop_notify.notified();
    release.notify_one();
    stopped.await;
    archived
        .await
        .unwrap()
        .unwrap_or_else(|error| panic!("archive: {error}"));
    watch.finish().await;
    assert_eq!(backend.stops.load(Ordering::SeqCst), 1);
    assert!(
        metadata
            .get("session:owner")
            .await
            .unwrap()
            .unwrap()
            .archived
    );
}

#[tokio::test(flavor = "current_thread")]
async fn archive_stop_session_error_skips_background_stop() {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(SessionStore::new(dir.path().to_path_buf()));
    let metadata = Arc::new(SqliteSessionMetadata::new(sqlite_pool().await));
    create_test_session(&metadata, "session:owner", Some("Owner")).await;
    let backend = Arc::new(RecordingSandbox::new());
    let router = archive_router(Arc::clone(&backend));
    let watch = router.watch_background_stops();
    let svc = LiveSessionService::new(store, Arc::clone(&metadata))
        .with_chat(Arc::new(ArchiveChat::Fail))
        .with_sandbox_router(router);
    let error = svc
        .patch(serde_json::json!({ "key": "session:owner", "archived": true }))
        .await
        .unwrap_err();
    watch.finish().await;
    assert!(error.to_string().contains("stop failed"));
    assert_eq!(backend.stops.load(Ordering::SeqCst), 0);
    assert!(
        metadata
            .get("session:owner")
            .await
            .unwrap()
            .unwrap()
            .archived
    );
}

#[tokio::test(flavor = "current_thread")]
async fn archive_foreign_owner_skips_background_stop() {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(SessionStore::new(dir.path().to_path_buf()));
    let metadata = Arc::new(SqliteSessionMetadata::new(sqlite_pool().await));
    create_test_session(&metadata, "session:parent", Some("Parent")).await;
    create_test_session(&metadata, "session:child", Some("Child")).await;
    metadata
        .set_sandbox_owner_key("session:child", Some("session:parent"))
        .await
        .unwrap();
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let backend = Arc::new(RecordingSandbox::new());
    let router = archive_router(Arc::clone(&backend));
    let watch = router.watch_background_stops();
    let svc = LiveSessionService::new(store, metadata)
        .with_chat(Arc::new(ArchiveChat::Count(Arc::clone(&calls))))
        .with_sandbox_router(router);
    svc.patch(serde_json::json!({ "key": "session:child", "archived": true }))
        .await
        .unwrap();
    watch.finish().await;
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(backend.stops.load(Ordering::SeqCst), 0);
}
