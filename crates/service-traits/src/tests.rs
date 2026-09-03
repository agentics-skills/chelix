use serde_json::json;

use crate::{
    BrowserService, ChatExecutionContext, ChatSendMessage, ChatSendRequest, ChatSendSyncRequest,
    ChatService, NoopBrowserService, ServiceResult, SessionBusyReason, SessionMutationCoordinator,
    interfaces::model_service_not_configured_error,
};

struct SlowShutdownBrowserService;

struct DefaultRefreshChatService;

#[async_trait::async_trait]
impl ChatService for DefaultRefreshChatService {
    async fn send(
        &self,
        _request: ChatSendRequest,
        _context: ChatExecutionContext,
    ) -> ServiceResult {
        Ok(json!({}))
    }

    async fn send_sync(
        &self,
        _request: ChatSendSyncRequest,
        _context: ChatExecutionContext,
    ) -> ServiceResult {
        Ok(json!({}))
    }

    async fn abort(&self, _params: serde_json::Value) -> ServiceResult {
        Ok(json!({}))
    }

    async fn history(&self, _params: serde_json::Value) -> ServiceResult {
        Ok(json!([]))
    }

    async fn inject(&self, _params: serde_json::Value) -> ServiceResult {
        Ok(json!({}))
    }

    async fn clear(&self, _params: serde_json::Value) -> ServiceResult {
        Ok(json!({}))
    }

    async fn compact(&self, _params: serde_json::Value) -> ServiceResult {
        Ok(json!({}))
    }

    async fn context(&self, _params: serde_json::Value) -> ServiceResult {
        Ok(json!({}))
    }

    async fn raw_prompt(&self, _params: serde_json::Value) -> ServiceResult {
        Ok(json!({}))
    }

    async fn full_context(&self, _params: serde_json::Value) -> ServiceResult {
        Ok(json!({}))
    }
}

#[async_trait::async_trait]
impl BrowserService for SlowShutdownBrowserService {
    async fn request(&self, _p: serde_json::Value) -> ServiceResult {
        Err("not used".into())
    }

    async fn shutdown(&self) {
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
}

#[tokio::test]
async fn noop_browser_service_lifecycle_methods() {
    let svc = NoopBrowserService;
    svc.cleanup_idle().await;
    svc.shutdown().await;
    assert!(
        svc.shutdown_with_grace(std::time::Duration::from_millis(10))
            .await
    );
    svc.close_all().await;
}

#[tokio::test]
async fn noop_browser_service_request_returns_error() {
    let svc = NoopBrowserService;
    let result = svc.request(serde_json::json!({})).await;
    assert!(result.is_err());
}

#[tokio::test]
async fn browser_shutdown_with_grace_times_out() {
    let svc = SlowShutdownBrowserService;
    assert!(
        !svc.shutdown_with_grace(std::time::Duration::from_millis(5))
            .await
    );
}

#[test]
fn model_service_not_configured_error_returns_expected_message() {
    let error = model_service_not_configured_error("models.disable");
    assert_eq!(error.to_string(), "model service not configured");
}

#[tokio::test]
async fn chat_service_default_refresh_prompt_memory_returns_not_configured() {
    let svc = DefaultRefreshChatService;
    let error = match svc
        .refresh_prompt_memory(json!({ "sessionKey": "session-a" }))
        .await
    {
        Ok(value) => panic!("default refresh should be unavailable, got {value:?}"),
        Err(error) => error,
    };
    assert_eq!(error.to_string(), "chat not configured");
}

#[test]
fn chat_send_request_accepts_the_closed_public_payload() {
    let request: ChatSendRequest = match serde_json::from_value(json!({
        "text": "Hello",
        "modelOverride": {
            "model": "test::model",
            "reasoningEffort": "low"
        },
        "clientSequence": 7
    })) {
        Ok(request) => request,
        Err(error) => panic!("valid chat.send payload should deserialize: {error}"),
    };

    assert_eq!(request.message, ChatSendMessage::Text("Hello".into()));
    let Some(model_override) = request.model_override else {
        panic!("valid override should be present");
    };
    assert_eq!(model_override.model, "test::model");
    assert_eq!(model_override.reasoning_effort.as_str(), "low");
    assert_eq!(request.client_sequence, Some(7));
}

#[test]
fn chat_send_request_rejects_invalid_message_or_override_shapes() {
    let cases = [
        json!({}),
        json!({ "text": "Hello", "content": [{ "type": "text", "text": "Hello" }] }),
        json!({ "text": "Hello", "modelOverride": { "model": "test::model" } }),
    ];

    for value in cases {
        assert!(serde_json::from_value::<ChatSendRequest>(value).is_err());
    }
}

#[test]
fn chat_send_request_rejects_an_additional_field() {
    let result = serde_json::from_value::<ChatSendRequest>(json!({
        "text": "Hello",
        "unexpected": true
    }));

    assert!(result.is_err());
}

#[test]
fn chat_send_sync_request_accepts_the_closed_public_payload() {
    let request: ChatSendSyncRequest = match serde_json::from_value(json!({
        "text": "Hello",
        "modelOverride": {
            "model": "test::model",
            "reasoningEffort": "low"
        }
    })) {
        Ok(request) => request,
        Err(error) => panic!("valid chat.send_sync payload should deserialize: {error}"),
    };

    assert_eq!(request.text, "Hello");
    let Some(model_override) = request.model_override else {
        panic!("valid override should be present");
    };
    assert_eq!(model_override.model, "test::model");
    assert_eq!(model_override.reasoning_effort.as_str(), "low");
}

#[test]
fn chat_send_sync_request_rejects_invalid_public_shapes() {
    let cases = [
        json!({}),
        json!({ "text": "Hello", "modelOverride": { "model": "test::model" } }),
    ];

    for value in cases {
        assert!(serde_json::from_value::<ChatSendSyncRequest>(value).is_err());
    }
}

#[test]
fn chat_send_sync_request_rejects_an_additional_field() {
    let result = serde_json::from_value::<ChatSendSyncRequest>(json!({
        "text": "Hello",
        "unexpected": true
    }));

    assert!(result.is_err());
}

#[tokio::test]
async fn session_mutation_coordinator_reports_active_turn_busy() {
    let coordinator = SessionMutationCoordinator::default();
    let turn = match coordinator.try_acquire_turn("s").await {
        Ok(turn) => turn,
        Err(error) => panic!("first turn should acquire, got {error}"),
    };

    let error = match coordinator.try_acquire_turn("s").await {
        Ok(_) => panic!("second turn should be busy"),
        Err(error) => error,
    };

    assert_eq!(error.reason(), SessionBusyReason::ActiveTurn);
    drop(turn);
}

#[tokio::test]
async fn session_mutation_coordinator_reports_reserved_mutation_busy() {
    let coordinator = SessionMutationCoordinator::default();
    let reservation = coordinator.reserve_mutation("s").await;

    let error = match coordinator.try_acquire_turn("s").await {
        Ok(_) => panic!("turn should be blocked by mutation reservation"),
        Err(error) => error,
    };

    assert_eq!(error.reason(), SessionBusyReason::ReservedMutation);
    let permit = match reservation.acquire().await {
        Ok(permit) => permit,
        Err(error) => panic!("reserved mutation should acquire, got {error}"),
    };
    drop(permit);
}
