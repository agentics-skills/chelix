#![allow(clippy::unwrap_used, clippy::expect_used)]

use {
    super::*,
    crate::{
        auth::{AuthMode, ResolvedAuth},
        services::GatewayServices,
    },
    chelix_common::tool_lifecycle::{ToolLifecycleEvent, ToolLifecycleUpdate},
    chelix_sessions::{PersistedMessage, store::SessionStore, ui_history_types::UiRunMetadata},
    serde_json::{Value, json},
};

fn state() -> Arc<GatewayState> {
    GatewayState::new(
        ResolvedAuth {
            mode: AuthMode::Token,
            token: None,
            password: None,
        },
        GatewayServices::noop(),
    )
}

fn request(range: UiHistoryRange, limit: usize) -> HistorySubscriptionRequest {
    HistorySubscriptionRequest {
        key: "main".into(),
        subscription_id: "subscription-1".into(),
        sequence: 1,
        range,
        limit,
    }
}

async fn receive(receiver: &mut mpsc::Receiver<String>) -> Value {
    let frame = tokio::time::timeout(std::time::Duration::from_secs(5), receiver.recv())
        .await
        .unwrap()
        .unwrap();
    let frame: Value = serde_json::from_str(&frame).unwrap();
    assert_eq!(frame["event"], "ui_history");
    assert_eq!(frame["payload"]["subscriptionId"], "subscription-1");
    frame["payload"].clone()
}

#[tokio::test]
async fn late_baseline_and_slow_queue_preserve_the_complete_tool_input() {
    let directory = tempfile::tempdir().unwrap();
    let store = SessionStore::new(directory.path().into());
    let session = store.ui_history.session("main").await.unwrap();
    let run = session
        .begin_run(UiRunMetadata {
            run_id: "run-1".into(),
            model: "model".into(),
            provider: "provider".into(),
            reasoning_effort: None,
        })
        .unwrap();
    let lifecycle = |sequence, update| PersistedMessage::ToolLifecycle {
        lifecycle: ToolLifecycleEvent {
            tool_call_id: "call-1".into(),
            tool_name: "multiedit_file".into(),
            sequence,
            emitted_at_ms: sequence,
            run_id: Some("run-1".into()),
            context_budget: None,
            update,
        },
    };
    run.copy(lifecycle(0, ToolLifecycleUpdate::Created {
        provider_index: Some(0),
    }))
    .unwrap();
    run.copy(lifecycle(1, ToolLifecycleUpdate::InputStreaming {
        arguments_delta: "{\"path\":".into(),
    }))
    .unwrap();
    let changes = session.subscribe();
    let page = session.page(UiHistoryRange::Latest, 4).await.unwrap();
    assert_eq!(
        page.history[0].accumulated_arguments.as_deref(),
        Some("{\"path\":")
    );
    let (sender, mut receiver) = mpsc::channel(1);
    sender.send("occupied".into()).await.unwrap();
    let delivery_session = Arc::clone(&session);
    let delivery = tokio::spawn(async move {
        deliver(
            &state(),
            delivery_session,
            sender,
            changes,
            request(UiHistoryRange::Latest, 4),
            page,
        )
        .await
    });
    run.copy(lifecycle(2, ToolLifecycleUpdate::InputStreaming {
        arguments_delta: "\"file".into(),
    }))
    .unwrap();
    tokio::task::yield_now().await;
    run.copy(lifecycle(3, ToolLifecycleUpdate::InputStreaming {
        arguments_delta: ".rs\"}".into(),
    }))
    .unwrap();
    assert_eq!(receiver.recv().await.as_deref(), Some("occupied"));
    let payload = receive(&mut receiver).await;
    let page = payload
        .get("update")
        .or_else(|| payload.get("snapshot"))
        .unwrap();
    assert_eq!(
        page["history"][0]["accumulatedArguments"],
        "{\"path\":\"file.rs\"}"
    );
    assert_eq!(page["history"][0]["stage"], "input_streaming");
    assert_eq!(page["totalMessages"], 1);
    assert!(store.read("main").await.unwrap().is_empty());
    delivery.abort();
    assert!(delivery.await.unwrap_err().is_cancelled());
    session.truncate(0).await.unwrap();
}

#[tokio::test]
async fn lag_recovers_the_selected_window_and_clear_replaces_its_generation() {
    let directory = tempfile::tempdir().unwrap();
    let store = SessionStore::new(directory.path().into());
    for index in 0..8 {
        store
            .append_typed("main", &PersistedMessage::user(format!("message {index}")))
            .await
            .unwrap();
    }
    let session = store.ui_history.session("main").await.unwrap();
    let changes = session.subscribe();
    let range = UiHistoryRange::Window {
        start: 1,
        end: Some(3),
    };
    let page = session.page(range.clone(), 2).await.unwrap();
    let generation = page.generation.clone();
    for index in 8..11 {
        store
            .append_typed("main", &PersistedMessage::user(format!("new tail {index}")))
            .await
            .unwrap();
    }
    let (sender, mut receiver) = mpsc::channel(1);
    let delivery_session = Arc::clone(&session);
    let delivery = tokio::spawn(async move {
        deliver(
            &state(),
            delivery_session,
            sender,
            changes,
            request(range, 2),
            page,
        )
        .await
    });
    let payload = receive(&mut receiver).await;
    assert_eq!(payload["snapshot"]["generation"], json!(generation));
    assert_eq!(payload["snapshot"]["firstPosition"], 1);
    assert_eq!(payload["snapshot"]["lastPosition"], 2);
    assert_eq!(payload["snapshot"]["totalMessages"], 11);
    store.clear("main").await.unwrap();
    let payload = receive(&mut receiver).await;
    assert_ne!(payload["snapshot"]["generation"], json!(generation));
    assert_eq!(payload["snapshot"]["history"], json!([]));
    assert_eq!(payload["snapshot"]["totalMessages"], 0);
    delivery.abort();
    assert!(delivery.await.unwrap_err().is_cancelled());
}
