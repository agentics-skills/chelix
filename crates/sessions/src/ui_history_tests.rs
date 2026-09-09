#![allow(clippy::unwrap_used, clippy::expect_used)]

use {
    chelix_common::{
        ProviderItemId, ProviderItemPosition, ProviderItemUpdate, ProviderItemUpdatePayload,
        ProviderSegmentId, ProviderSegmentOutcome,
        tool_lifecycle::{ToolLifecycleEvent, ToolLifecycleUpdate},
    },
    serde_json::json,
};

use {
    super::*,
    crate::store::{SessionStore, UserMessageTarget},
};

fn run(session: &Arc<UiHistorySession>) -> UiHistoryRun {
    session
        .begin_run(UiRunMetadata {
            run_id: "run-1".into(),
            model: "provider::actual-model".into(),
            provider: "provider".into(),
            reasoning_effort: Some("high".into()),
        })
        .unwrap()
}

fn update(sequence: u64, delta: &str) -> PersistedMessage {
    PersistedMessage::ProviderUpdate {
        update: ProviderItemUpdate {
            segment_id: ProviderSegmentId::new("segment-1"),
            item_id: ProviderItemId::new("item-1"),
            position: ProviderItemPosition(0),
            update_seq: sequence,
            payload: ProviderItemUpdatePayload::MessageDelta {
                delta: delta.into(),
            },
        },
        created_at: Some(123),
        seq: None,
        run_id: Some("run-1".into()),
    }
}

fn lifecycle(sequence: u64, update: ToolLifecycleUpdate) -> PersistedMessage {
    PersistedMessage::ToolLifecycle {
        lifecycle: ToolLifecycleEvent {
            tool_call_id: "call-1".into(),
            tool_name: "multiedit_file".into(),
            sequence,
            emitted_at_ms: 123 + sequence,
            run_id: Some("run-1".into()),
            context_budget: None,
            update,
        },
    }
}

#[tokio::test]
async fn schema_migration_is_tracked_and_validated_on_reopen() {
    let directory = tempfile::tempdir().unwrap();
    for _ in 0..2 {
        let engine = UiHistoryEngine::new(directory.path().into());
        engine.initialize().await.unwrap();
        let pool = engine.database.pool().await.unwrap();
        let migrations = sqlx::query_as::<_, (i64, bool)>(
            "SELECT version, success FROM _sqlx_migrations ORDER BY version",
        )
        .fetch_all(pool)
        .await
        .unwrap();
        assert_eq!(migrations, vec![(20260909113752, true)]);
        pool.close().await;
    }
}

#[tokio::test]
async fn search_excludes_only_sessions_without_ui_snapshots() {
    let directory = tempfile::tempdir().unwrap();
    let store = SessionStore::new(directory.path().into());
    let journal = b"{\"role\":\"user\",\"content\":\"needle\"}\n";
    for key in ["old-first", "old-middle"] {
        tokio::fs::write(directory.path().join(format!("{key}.jsonl")), journal)
            .await
            .unwrap();
    }
    for key in ["healthy-first", "healthy-last"] {
        store
            .append_typed(key, &PersistedMessage::user("needle"))
            .await
            .unwrap();
    }
    let keys = ["old-first", "healthy-first", "old-middle", "healthy-last"].map(String::from);
    let hits = store.search(&keys, "needle", 2).await.unwrap();
    assert_eq!(
        hits.iter()
            .map(|hit| hit.session_key.as_str())
            .collect::<Vec<_>>(),
        ["healthy-first", "healthy-last"]
    );
    assert_eq!(
        store.search(&keys, "needle", 1).await.unwrap()[0].session_key,
        "healthy-first"
    );
    for key in ["old-first", "old-middle"] {
        assert!(
            matches!(store.ui_history.page(key, UiHistoryRange::Latest, 1).await,
            Err(Error::MissingUiSnapshots { session_key }) if session_key == key)
        );
        assert_eq!(
            tokio::fs::read(directory.path().join(format!("{key}.jsonl")))
                .await
                .unwrap(),
            journal
        );
    }
    let imported: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM ui_history_sessions WHERE session_key IN ('old-first', 'old-middle')",
    )
    .fetch_one(store.ui_history.database.pool().await.unwrap())
    .await
    .unwrap();
    assert_eq!(imported, 0);

    let failed = store.ui_history.session("failed").await.unwrap();
    failed.fail(&Error::message("snapshot persistence failed"));
    assert_eq!(
        failed.flush().await.unwrap_err().to_string(),
        "snapshot persistence failed"
    );
    let error = store
        .search(
            &["old-first".into(), "failed".into(), "healthy-first".into()],
            "needle",
            2,
        )
        .await
        .unwrap_err();
    assert!(error.to_string().contains("snapshot persistence failed"));
    assert!(!matches!(error, Error::MissingUiSnapshots { .. }));
}

#[tokio::test]
async fn explicit_clear_discards_an_unreadable_journal_without_importing_it() {
    let directory = tempfile::tempdir().unwrap();
    let store = SessionStore::new(directory.path().into());
    let journal = directory.path().join("main.jsonl");
    tokio::fs::write(&journal, b"{\"role\":\"user\",\"content\":\"discard\"}\n")
        .await
        .unwrap();
    store
        .save_media("main", "discard.ogg", b"OggS")
        .await
        .unwrap();
    assert!(store.ui_history.session("main").await.is_err());
    store.clear("main").await.unwrap();
    assert!(!journal.exists());
    assert!(store.read_media("main", "discard.ogg").await.is_err());
    assert_eq!(store.ui_message_count("main").await.unwrap(), 0);
    store
        .append_typed("main", &PersistedMessage::user("new conversation"))
        .await
        .unwrap();
    assert_eq!(store.ui_message_count("main").await.unwrap(), 1);
}

#[tokio::test]
async fn clear_shares_the_live_registry_and_rotates_its_generation() {
    let directory = tempfile::tempdir().unwrap();
    let store = SessionStore::new(directory.path().into());
    let session = store.ui_history.session("main").await.unwrap();
    let discard = store.ui_history.session_for_clear("main").await.unwrap();
    assert!(Arc::ptr_eq(&session, &discard));
    let generation = session.subscribe().borrow().generation.clone();
    store.clear("main").await.unwrap();
    assert_ne!(session.subscribe().borrow().generation, generation);
    assert!(Arc::ptr_eq(
        &session,
        &store.ui_history.session("main").await.unwrap()
    ));
}

#[tokio::test]
async fn failed_clear_persists_failure_instead_of_exposing_an_empty_history() {
    let directory = tempfile::tempdir().unwrap();
    let store = SessionStore::new(directory.path().into());
    tokio::fs::create_dir(directory.path().join("main.jsonl"))
        .await
        .unwrap();
    assert!(store.clear("main").await.is_err());
    let row = sqlx::query_scalar::<_, Option<String>>(
        "SELECT failure FROM ui_history_sessions WHERE session_key = 'main'",
    )
    .fetch_one(store.ui_history.database.pool().await.unwrap())
    .await
    .unwrap();
    assert!(row.is_some());
    assert!(store.ui_message_count("main").await.is_err());
}

#[tokio::test]
async fn user_batch_validation_is_atomic_and_duplicate_ids_are_rejected() {
    let directory = tempfile::tempdir().unwrap();
    let store = SessionStore::new(directory.path().into());
    let id = uuid::Uuid::new_v4().to_string();
    let user = json!({"role": "user", "content": "hello", "clientMessageId": id});
    let duplicate_batch = vec![user.clone(), user.clone()];
    assert!(
        store
            .append_batch_at_index("main", &duplicate_batch, 0)
            .await
            .is_err()
    );
    assert_eq!(store.ui_message_count("main").await.unwrap(), 0);
    assert!(store.read("main").await.unwrap().is_empty());
    store.append("main", &user).await.unwrap();
    assert!(store.append("main", &user).await.is_err());
    assert!(
        store
            .append_at_index(
                "main",
                &PersistedMessage::user("wrong boundary").to_value(),
                0
            )
            .await
            .is_err()
    );
    store
        .append("main", &PersistedMessage::user("next").to_value())
        .await
        .unwrap();
    let page = store
        .ui_history
        .page("main", UiHistoryRange::Latest, 10)
        .await
        .unwrap();
    assert_eq!(page.total_messages, 2);
    assert_eq!(page.history[0].id.0, format!("user:{id}"));
    assert_eq!(store.read("main").await.unwrap().len(), 2);
}

#[tokio::test]
async fn live_provider_copy_precedes_journal_and_reloads_as_one_semantic_message() {
    let directory = tempfile::tempdir().unwrap();
    let store = SessionStore::new(directory.path().into());
    let session = store.ui_history.session("main").await.unwrap();
    let run = run(&session);
    let mut changes = session.subscribe();
    {
        let _journal = session.journal.lock().await;
        run.copy(update(1, "hello")).unwrap();
        assert!(changes.has_changed().unwrap());
        assert_eq!(changes.borrow_and_update().total_messages, 1);
    }
    run.copy(update(2, " world")).unwrap();
    store
        .append_typed("main", &update(1, "hello"))
        .await
        .unwrap();
    store
        .append_typed("main", &update(2, " world"))
        .await
        .unwrap();
    let page = session.page(UiHistoryRange::Latest, 1).await.unwrap();
    let value = page.history[0].public_value().unwrap();
    assert_eq!(value["content"], "hello world");
    assert_eq!(value["model"], "provider::actual-model");
    assert_eq!(value["reasoningEffort"], "high");
    assert!(!page.history[0].canonical_committed);
    let close = PersistedMessage::ProviderSegmentClose {
        segment_id: ProviderSegmentId::new("segment-1"),
        outcome: ProviderSegmentOutcome::Completed,
        created_at: Some(150),
        seq: None,
        run_id: Some("run-1".into()),
    };
    run.copy(close.clone()).unwrap();
    store.append_typed("main", &close).await.unwrap();
    let UiContent::Record(record) = &page.history[0].content else {
        panic!("assistant record required")
    };
    store.append_typed("main", &record.message).await.unwrap();
    run.finish().await.unwrap();
    assert!(run.copy(update(3, "late")).is_err());
    let reloaded = SessionStore::new(directory.path().into());
    let page = reloaded
        .ui_history
        .page("main", UiHistoryRange::Latest, 10)
        .await
        .unwrap();
    assert_eq!(page.history.len(), 1);
    assert_eq!(page.history[0].position, 0);
    assert!(page.history[0].canonical_committed);
    assert_eq!(
        page.history[0].public_value().unwrap()["content"],
        "hello world"
    );
    assert_eq!(reloaded.read("main").await.unwrap().len(), 4);
}

#[tokio::test]
async fn late_subscriber_and_search_receive_accumulated_uncommitted_tool_input() {
    let directory = tempfile::tempdir().unwrap();
    let store = SessionStore::new(directory.path().into());
    let session = store.ui_history.session("main").await.unwrap();
    let run = run(&session);
    run.copy(lifecycle(0, ToolLifecycleUpdate::Created {
        provider_index: Some(0),
    }))
    .unwrap();
    for (sequence, delta) in [(1, "{\"path\":"), (2, "\"needle\"}")] {
        run.copy(lifecycle(sequence, ToolLifecycleUpdate::InputStreaming {
            arguments_delta: delta.into(),
        }))
        .unwrap();
    }
    let _subscription = session.subscribe();
    let page = session.page(UiHistoryRange::Latest, 1).await.unwrap();
    assert_eq!(
        page.history[0].accumulated_arguments.as_deref(),
        Some("{\"path\":\"needle\"}")
    );
    assert_eq!(page.history[0].id, UiMessageId::tool("run-1", "call-1"));
    assert_eq!(page.total_messages, 1);
    let hits = store.search(&["main".into()], "needle", 1).await.unwrap();
    assert_eq!(hits[0].message_id, page.history[0].id);
    assert!(store.read("main").await.unwrap().is_empty());
    assert!(run.finish().await.is_err());
    session.truncate(0).await.unwrap();
}

#[tokio::test]
async fn receipt_cannot_commit_a_later_lifecycle_version_or_another_generation() {
    let directory = tempfile::tempdir().unwrap();
    let engine = UiHistoryEngine::new(directory.path().into());
    let session = engine.session("main").await.unwrap();
    let run = run(&session);
    let created = lifecycle(0, ToolLifecycleUpdate::Created {
        provider_index: Some(0),
    });
    run.copy(created.clone()).unwrap();
    let receipts = session.stage_batch(vec![created.into()]).await.unwrap();
    run.copy(lifecycle(1, ToolLifecycleUpdate::InputStreaming {
        arguments_delta: "{".into(),
    }))
    .unwrap();
    session.bind(&receipts[0], 0).unwrap();
    let page = session.page(UiHistoryRange::Latest, 1).await.unwrap();
    assert!(!page.history[0].canonical_committed);
    assert!(session.bind(&receipts[0], 0).is_err());
    let receipts = session
        .stage_batch(vec![PersistedMessage::user("pending").into()])
        .await
        .unwrap();
    session.truncate(0).await.unwrap();
    assert!(session.bind(&receipts[0], 1).is_err());
    assert!(run.copy(update(1, "obsolete")).is_err());
    assert_eq!(engine.count("main").await.unwrap(), 0);
}

#[tokio::test]
async fn ranges_and_lag_recovery_use_positions_and_generation() {
    let directory = tempfile::tempdir().unwrap();
    let store = SessionStore::new(directory.path().into());
    for index in 0..8 {
        store
            .append_typed("main", &PersistedMessage::user(format!("message {index}")))
            .await
            .unwrap();
    }
    let session = store.ui_history.session("main").await.unwrap();
    let latest = session.page(UiHistoryRange::Latest, 3).await.unwrap();
    assert_eq!(latest.first_position, Some(5));
    assert!(latest.has_older);
    assert!(!latest.has_newer);
    let before = session
        .page(UiHistoryRange::Before { position: 5 }, 3)
        .await
        .unwrap();
    assert_eq!(before.first_position, Some(2));
    assert_eq!(before.last_position, Some(4));
    let around = session
        .page(
            UiHistoryRange::Around {
                message_id: before.history[1].id.clone(),
            },
            1,
        )
        .await
        .unwrap();
    assert_eq!(around.history[0].id, before.history[1].id);
    let window = session
        .page(
            UiHistoryRange::Window {
                start: 2,
                end: Some(5),
            },
            3,
        )
        .await
        .unwrap();
    assert_eq!(
        window
            .history
            .iter()
            .map(|message| message.position)
            .collect::<Vec<_>>(),
        vec![2, 3, 4]
    );
    assert!(
        session
            .updates_since(&latest.generation, 0, 2)
            .await
            .unwrap()
            .is_none()
    );
    let target = UiHistoryTarget {
        message_id: latest.history[1].id.clone(),
        generation: latest.generation.clone(),
    };
    let index = session.canonical_index(&target).await.unwrap();
    store
        .truncate_from_user_message("main", UserMessageTarget::MessageIndex(index))
        .await
        .unwrap();
    assert!(session.canonical_index(&target).await.is_err());
    assert!(
        session
            .updates_since(&latest.generation, latest.revision, 10)
            .await
            .unwrap()
            .is_none()
    );
    let retained = session.page(UiHistoryRange::Latest, 10).await.unwrap();
    assert_ne!(retained.generation, latest.generation);
    assert_eq!(retained.total_messages, 6);
    assert_eq!(store.read("main").await.unwrap().len(), 6);
}

#[tokio::test]
async fn fork_preserves_semantic_errors_and_presentation_without_changing_provider_context() {
    let directory = tempfile::tempdir().unwrap();
    let store = SessionStore::new(directory.path().into());
    store
        .append_typed("parent", &PersistedMessage::user("question"))
        .await
        .unwrap();
    let session = store.ui_history.session("parent").await.unwrap();
    let run = run(&session);
    let error_id = run
        .error(UiProviderError {
            run_id: "run-1".into(),
            segment_id: None,
            created_at: 42,
            raw: "upstream failure".into(),
            details: json!({"status": 503}),
            retry_after_ms: Some(100),
        })
        .unwrap();
    let generation = session.subscribe().borrow().generation.clone();
    session
        .update_presentation(
            &UiHistoryTarget {
                message_id: error_id.clone(),
                generation: generation.clone(),
            },
            UiPresentation {
                document: Some(UiPresentationDocument::Text("display metadata".into())),
                metadata: BTreeMap::from([("retry".into(), json!(1))]),
            },
        )
        .await
        .unwrap();
    run.finish().await.unwrap();
    assert_eq!(
        store
            .fork_history("parent", "child", None)
            .await
            .unwrap()
            .fork_point,
        2
    );
    let child = store
        .ui_history
        .page("child", UiHistoryRange::Latest, 10)
        .await
        .unwrap();
    assert_ne!(child.generation, generation);
    assert_eq!(child.history[1].id, error_id);
    assert_eq!(child.history[1].presentation.metadata["retry"], 1);
    assert_eq!(
        store.read("parent").await.unwrap(),
        store.read("child").await.unwrap()
    );
    assert_eq!(store.read("child").await.unwrap().len(), 1);
    assert!(store.fork_history("parent", "child", None).await.is_err());
}

#[tokio::test]
async fn active_branch_selects_and_discloses_a_confirmed_noninterleaved_prefix() {
    use crate::ui_history_types::UiForkBoundaryReason;
    let directory = tempfile::tempdir().unwrap();
    let store = SessionStore::new(directory.path().into());
    store
        .append_typed(
            "parent",
            &PersistedMessage::user("branch this conversation"),
        )
        .await
        .unwrap();
    let session = store.ui_history.session("parent").await.unwrap();
    let run = run(&session);
    let provider = |sequence, payload| PersistedMessage::ProviderUpdate {
        update: ProviderItemUpdate {
            segment_id: ProviderSegmentId::new("segment-1"),
            item_id: ProviderItemId::new("call-1"),
            position: ProviderItemPosition(0),
            update_seq: sequence,
            payload,
        },
        created_at: Some(123),
        seq: None,
        run_id: Some("run-1".into()),
    };
    let tool = |sequence, update| PersistedMessage::ToolLifecycle {
        lifecycle: ToolLifecycleEvent {
            tool_call_id: "call-1".into(),
            tool_name: "branch_session".into(),
            sequence,
            emitted_at_ms: 123 + sequence,
            run_id: Some("run-1".into()),
            context_budget: None,
            update,
        },
    };
    for record in [
        provider(1, ProviderItemUpdatePayload::FunctionCallStart {
            name: "branch_session".into(),
        }),
        tool(0, ToolLifecycleUpdate::Created {
            provider_index: Some(0),
        }),
        provider(2, ProviderItemUpdatePayload::FunctionCallDone {
            arguments: "{\"label\":\"Branch\"}".into(),
        }),
        PersistedMessage::ProviderSegmentClose {
            segment_id: ProviderSegmentId::new("segment-1"),
            outcome: ProviderSegmentOutcome::Completed,
            created_at: Some(130),
            seq: None,
            run_id: Some("run-1".into()),
        },
    ] {
        run.copy(record.clone()).unwrap();
        store.append_typed("parent", &record).await.unwrap();
    }
    let page = session.page(UiHistoryRange::Latest, 10).await.unwrap();
    let UiContent::Record(assistant) = &page.history[1].content else {
        panic!("assistant required")
    };
    store
        .append_typed("parent", &assistant.message)
        .await
        .unwrap();
    for record in [
        tool(1, ToolLifecycleUpdate::InputReady {
            arguments: json!({"label": "Branch"}),
        }),
        tool(2, ToolLifecycleUpdate::Executing {
            arguments: json!({"label": "Branch"}),
            started_at_ms: 140,
        }),
    ] {
        run.copy(record.clone()).unwrap();
        store.append_typed("parent", &record).await.unwrap();
    }
    let parent_journal = store.read("parent").await.unwrap();
    let parent_page = session.page(UiHistoryRange::Latest, 10).await.unwrap();
    let fork = store.fork_history("parent", "default", None).await.unwrap();
    assert_eq!(fork.fork_point, 1);
    assert_eq!(fork.source_end, 3);
    assert!(fork.boundary_adjusted);
    assert_eq!(fork.boundary_reasons, vec![
        UiForkBoundaryReason::ActiveContent,
        UiForkBoundaryReason::InterleavedSegment
    ]);
    assert_eq!(store.read("default").await.unwrap(), parent_journal[..1]);
    assert_eq!(store.ui_message_count("default").await.unwrap(), 1);
    assert!(
        store
            .fork_history("parent", "active", Some(3))
            .await
            .is_err()
    );
    assert!(
        store
            .fork_history("parent", "interleaved", Some(2))
            .await
            .is_err()
    );
    let exact = store
        .fork_history("parent", "exact", Some(1))
        .await
        .unwrap();
    assert_eq!(exact.fork_point, 1);
    assert_eq!(exact.source_end, 3);
    assert!(!exact.boundary_adjusted);
    assert!(exact.boundary_reasons.is_empty());
    assert_eq!(store.read("parent").await.unwrap(), parent_journal);
    assert_eq!(
        session
            .page(UiHistoryRange::Latest, 10)
            .await
            .unwrap()
            .public_value()
            .unwrap(),
        parent_page.public_value().unwrap()
    );
    session.truncate(0).await.unwrap();
}

#[tokio::test]
async fn default_fork_refuses_unconfirmed_only_source_but_explicit_zero_and_empty_are_valid() {
    let directory = tempfile::tempdir().unwrap();
    let store = SessionStore::new(directory.path().into());
    let session = store.ui_history.session("active").await.unwrap();
    let run = run(&session);
    run.copy(update(1, "unfinished")).unwrap();
    assert!(store.fork_history("active", "refused", None).await.is_err());
    assert!(!store.list_keys().contains(&"refused".to_string()));
    let zero = store.fork_history("active", "zero", Some(0)).await.unwrap();
    assert_eq!(zero.fork_point, 0);
    assert_eq!(zero.source_end, 1);
    assert!(!zero.boundary_adjusted);
    assert!(zero.boundary_reasons.is_empty());
    let empty = store
        .fork_history("empty", "empty-child", None)
        .await
        .unwrap();
    assert_eq!(empty.fork_point, 0);
    assert_eq!(empty.source_end, 0);
    assert!(!empty.boundary_adjusted);
    assert!(empty.boundary_reasons.is_empty());
    session.truncate(0).await.unwrap();
}

#[tokio::test]
async fn dirty_presentation_removal_keeps_the_next_persisted_search_candidate() {
    let directory = tempfile::tempdir().unwrap();
    let store = SessionStore::new(directory.path().into());
    for _ in 0..2 {
        store
            .append_typed("main", &PersistedMessage::user("record"))
            .await
            .unwrap();
    }
    let session = store.ui_history.session("main").await.unwrap();
    let page = session.page(UiHistoryRange::Latest, 10).await.unwrap();
    for snapshot in &page.history {
        session
            .update_presentation(
                &UiHistoryTarget {
                    message_id: snapshot.id.clone(),
                    generation: page.generation.clone(),
                },
                UiPresentation {
                    document: Some(UiPresentationDocument::Text("needle".into())),
                    ..UiPresentation::default()
                },
            )
            .await
            .unwrap();
    }
    let flush_guard = session.flush_progress.lock().await;
    let mut changes = session.subscribe();
    let target = UiHistoryTarget {
        message_id: page.history[0].id.clone(),
        generation: page.generation.clone(),
    };
    let editing = Arc::clone(&session);
    let edit = tokio::spawn(async move {
        editing
            .update_presentation(&target, UiPresentation::default())
            .await
    });
    changes.changed().await.unwrap();
    let hits = store.search(&["main".into()], "needle", 1).await.unwrap();
    assert_eq!(hits[0].message_id, page.history[1].id);
    drop(flush_guard);
    edit.await.unwrap().unwrap();
}

#[tokio::test]
async fn assistant_debug_payload_stays_in_the_journal_across_snapshot_updates() {
    let directory = tempfile::tempdir().unwrap();
    let store = SessionStore::new(directory.path().into());
    let message = json!({
        "role": "assistant", "content": "Complete answer", "inputTokens": 100,
        "llmApiResponse": [{"type": "response.output_text.delta", "delta": "Complete"}]
    });
    let record = UiRecord::try_from(message.clone()).unwrap();
    for ingress in [UiIngress::Live, UiIngress::Journal] {
        let mut entry = projection::initial_entry(UiMessageId("reply".into()), 0, record.clone());
        assert!(projection::project(&mut entry, record.clone(), ingress, None).unwrap());
        assert!(matches!(&entry.snapshot.content, UiContent::Record(record)
            if matches!(&record.message, PersistedMessage::Assistant { llm_api_response: None, .. })));
    }
    assert_eq!(serde_json::to_value(&record).unwrap(), message);
    store.append("main", &message).await.unwrap();
    assert_eq!(store.read("main").await.unwrap(), vec![message.clone()]);
    let stored: String = sqlx::query_scalar(
        "SELECT snapshot_json FROM ui_history_snapshots WHERE session_key = 'main'",
    )
    .fetch_one(store.ui_history.database.pool().await.unwrap())
    .await
    .unwrap();
    let stored: serde_json::Value = serde_json::from_str(&stored).unwrap();
    assert!(stored["snapshot"].get("llmApiResponse").is_none());
    assert_eq!(stored["snapshot"]["content"], "Complete answer");
    assert_eq!(stored["snapshot"]["inputTokens"], 100);

    store
        .update_typed_at("main", 0, |mut message| {
            if let PersistedMessage::Assistant { audio, .. } = &mut message {
                *audio = Some("media/main/voice.ogg".into());
            }
            message
        })
        .await
        .unwrap();
    let mut expected = message;
    expected["audio"] = json!("media/main/voice.ogg");
    assert_eq!(store.read("main").await.unwrap(), vec![expected]);
    let reloaded = UiHistoryEngine::new(directory.path().into());
    let page = reloaded
        .page("main", UiHistoryRange::Latest, 1)
        .await
        .unwrap();
    assert!(matches!(&page.history[0].content, UiContent::Record(record)
        if matches!(&record.message, PersistedMessage::Assistant { llm_api_response: None, .. })));
    let value = page.public_value().unwrap();
    assert_eq!(value["history"][0]["content"], "Complete answer");
    assert_eq!(value["history"][0]["audio"], "media/main/voice.ogg");
    assert!(value["history"][0].get("llmApiResponse").is_none());
}

#[tokio::test]
async fn snapshot_serde_round_trip_preserves_envelope_and_record_metadata() {
    let directory = tempfile::tempdir().unwrap();
    let store = SessionStore::new(directory.path().into());
    let client_id = uuid::Uuid::new_v4().to_string();
    store.append("main", &json!({"role": "user", "content": "voice", "audio": "media/main/input.ogg", "clientMessageId": client_id})).await.unwrap();
    let page = store
        .ui_history
        .page("main", UiHistoryRange::Latest, 1)
        .await
        .unwrap();
    let value = page.history[0].public_value().unwrap();
    let round_trip: UiSnapshot = serde_json::from_value(value.clone()).unwrap();
    assert_eq!(round_trip.public_value().unwrap(), value);
    store
        .update_typed_at("main", 0, |mut message| {
            if let PersistedMessage::User { content, .. } = &mut message {
                *content = crate::MessageContent::Text("transcribed".into());
            }
            message
        })
        .await
        .unwrap();
    let history = store.ui_history.history("main").await.unwrap();
    assert_eq!(history[0]["clientMessageId"], client_id);
    assert_eq!(history[0]["audio"], "media/main/input.ogg");
    assert_eq!(history[0]["content"], "transcribed");
}

#[tokio::test]
async fn finalization_refusal_survives_releasing_the_last_session_owner() {
    let directory = tempfile::tempdir().unwrap();
    let engine = UiHistoryEngine::new(directory.path().into());
    let session = engine.session("main").await.unwrap();
    let run = run(&session);
    run.copy(update(1, "unfinished")).unwrap();
    let error = run.finish().await.unwrap_err().to_string();
    drop(run);
    drop(session);
    let retained: String =
        sqlx::query_scalar("SELECT failure FROM ui_history_sessions WHERE session_key = 'main'")
            .fetch_one(engine.database.pool().await.unwrap())
            .await
            .unwrap();
    assert_eq!(retained, error);
    let reloaded = UiHistoryEngine::new(directory.path().into());
    assert_eq!(reloaded.count("main").await.unwrap_err().to_string(), error);
}

#[tokio::test]
async fn rejected_addressed_identity_mutations_leave_journal_and_snapshots_unchanged() {
    let directory = tempfile::tempdir().unwrap();
    let store = SessionStore::new(directory.path().into());
    store.append("main", &json!({"role": "user", "content": "question", "clientMessageId": uuid::Uuid::new_v4().to_string()})).await.unwrap();
    let journal = store.read("main").await.unwrap();
    let page = store
        .ui_history
        .page("main", UiHistoryRange::Latest, 10)
        .await
        .unwrap()
        .public_value()
        .unwrap();
    assert!(
        store
            .update_typed_at("main", 0, |_| PersistedMessage::system("wrong role"))
            .await
            .is_err()
    );
    assert!(
        store
            .update_value_at("main", 0, |mut record| {
                record["clientMessageId"] = json!(uuid::Uuid::new_v4().to_string());
                Ok(record)
            })
            .await
            .is_err()
    );
    assert_eq!(store.read("main").await.unwrap(), journal);
    assert_eq!(
        store
            .ui_history
            .page("main", UiHistoryRange::Latest, 10)
            .await
            .unwrap()
            .public_value()
            .unwrap(),
        page
    );
}

#[tokio::test]
async fn persistence_failure_is_published_and_refuses_later_ingress() {
    let directory = tempfile::tempdir().unwrap();
    let engine = UiHistoryEngine::new(directory.path().into());
    let session = engine.session("main").await.unwrap();
    let run = run(&session);
    sqlx::query("CREATE TRIGGER reject_ui_snapshot BEFORE INSERT ON ui_history_snapshots BEGIN SELECT RAISE(FAIL, 'write refused'); END")
        .execute(session.database.pool().await.unwrap()).await.unwrap();
    run.copy(update(1, "visible before disk")).unwrap();
    let failure = session.flush().await.unwrap_err();
    session.fail(&failure);
    assert!(session.subscribe().borrow().failure.is_some());
    assert!(run.copy(update(2, "refused")).is_err());
    assert!(run.finish().await.is_err());
}
