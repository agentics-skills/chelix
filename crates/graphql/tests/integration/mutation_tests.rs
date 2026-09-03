use {async_graphql::Request, serde_json::json};

use crate::common::{MockDispatch, build_test_schema};

#[tokio::test]
async fn config_set_mutation() {
    let mock = MockDispatch::new();
    mock.set_response("config.set", json!({"ok": true}));
    let (schema, _) = build_test_schema(mock.clone());

    let res = schema
        .execute(Request::new(
            r#"mutation { config { set(path: "theme", value: "dark") { ok } } }"#,
        ))
        .await;

    assert!(res.errors.is_empty(), "errors: {:?}", res.errors);
    let (method, params) = mock.last_call().expect("should have called");
    assert_eq!(method, "config.set");
    assert_eq!(params["path"], "theme");
    assert_eq!(params["value"], "dark");
}

#[tokio::test]
async fn chat_send_mutation() {
    let mock = MockDispatch::new();
    mock.set_response("chat.send", json!({"ok": true, "sessionKey": "sess1"}));
    let (schema, _) = build_test_schema(mock.clone());

    let res = schema
        .execute(Request::new(
            r#"mutation { chat { send(message: "Hello", sessionKey: "sess1", modelOverride: { model: "test::model", reasoningEffort: "low" }) { ok } } }"#,
        ))
        .await;

    assert!(res.errors.is_empty(), "errors: {:?}", res.errors);
    let (method, params) = mock.last_call().expect("should have called");
    assert_eq!(method, "chat.send");
    assert_eq!(params["request"]["text"], "Hello");
    assert_eq!(params["request"]["modelOverride"]["model"], "test::model");
    assert_eq!(params["request"]["modelOverride"]["reasoningEffort"], "low");
    assert_eq!(params["context"]["sessionId"], "sess1");
    assert!(params["request"].get("sessionKey").is_none());
}

#[tokio::test]
async fn chat_send_model_override_rejects_additional_fields() {
    let mock = MockDispatch::new();
    let (schema, _) = build_test_schema(mock.clone());

    let res = schema
        .execute(Request::new(
            r#"mutation { chat { send(message: "Hello", sessionKey: "sess1", modelOverride: { model: "test::model", reasoningEffort: "low", unexpected: true }) { ok } } }"#,
        ))
        .await;

    assert!(!res.errors.is_empty());
    assert_eq!(mock.call_count(), 0);
}

async fn assert_requires_session_key(query: &str, label: &str) {
    let mock = MockDispatch::new();
    let (schema, _) = build_test_schema(mock);
    let res = schema.execute(Request::new(query)).await;
    assert!(
        !res.errors.is_empty(),
        "{label} without sessionKey should fail"
    );
}

#[tokio::test]
async fn chat_send_requires_session_key() {
    assert_requires_session_key(
        r#"mutation { chat { send(message: "Hello") { ok } } }"#,
        "send",
    )
    .await;
}

#[tokio::test]
async fn chat_abort_requires_session_key() {
    assert_requires_session_key(r#"mutation { chat { abort { ok } } }"#, "abort").await;
}

#[tokio::test]
async fn chat_remove_queued_prompt_uses_the_numeric_id() {
    let mock = MockDispatch::new();
    mock.set_response(
        "chat.queued_prompts.remove",
        json!({ "sessionKey": "session:one", "prompts": [] }),
    );
    let (schema, _) = build_test_schema(mock.clone());

    let res = schema
        .execute(Request::new(
            r#"mutation { chat { removeQueuedPrompt(id: 42) } }"#,
        ))
        .await;

    assert!(res.errors.is_empty(), "errors: {:?}", res.errors);
    let (method, params) = mock.last_call().expect("should have called");
    assert_eq!(method, "chat.queued_prompts.remove");
    assert_eq!(params, json!({ "id": 42 }));
}

#[tokio::test]
async fn chat_remove_queued_prompt_requires_id() {
    let mock = MockDispatch::new();
    let (schema, _) = build_test_schema(mock);
    let res = schema
        .execute(Request::new(r#"mutation { chat { removeQueuedPrompt } }"#))
        .await;
    assert!(
        !res.errors.is_empty(),
        "removeQueuedPrompt without id should fail"
    );
}

#[tokio::test]
async fn chat_clear_requires_session_key() {
    assert_requires_session_key(r#"mutation { chat { clear { ok } } }"#, "clear").await;
}

#[tokio::test]
async fn chat_compact_requires_session_key() {
    assert_requires_session_key(r#"mutation { chat { compact { ok } } }"#, "compact").await;
}

#[tokio::test]
async fn chat_history_requires_session_key() {
    assert_requires_session_key(r#"query { chat { history } }"#, "history").await;
}

#[tokio::test]
async fn chat_context_requires_session_key() {
    assert_requires_session_key(r#"query { chat { context } }"#, "context").await;
}

#[tokio::test]
async fn chat_raw_prompt_requires_session_key() {
    assert_requires_session_key(r#"query { chat { rawPrompt { prompt } } }"#, "rawPrompt").await;
}

#[tokio::test]
async fn chat_full_context_requires_session_key() {
    assert_requires_session_key(r#"query { chat { fullContext } }"#, "fullContext").await;
}

#[tokio::test]
async fn cron_add_mutation() {
    let mock = MockDispatch::new();
    mock.set_response("cron.add", json!({"ok": true}));
    let (schema, _) = build_test_schema(mock.clone());

    let res = schema
        .execute(Request::new(
            r#"mutation { cron { add(input: { name: "backup" }) { ok } } }"#,
        ))
        .await;

    assert!(res.errors.is_empty(), "errors: {:?}", res.errors);
    let (method, params) = mock.last_call().expect("should have called");
    assert_eq!(method, "cron.add");
    assert_eq!(params["name"], "backup");
}
