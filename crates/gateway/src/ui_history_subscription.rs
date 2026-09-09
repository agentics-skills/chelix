//! Per-client semantic history delivery with a subscribed snapshot baseline.

#[cfg(test)]
#[path = "ui_history_subscription_tests.rs"]
mod tests;

use std::sync::Arc;

use {
    chelix_protocol::{ErrorShape, EventFrame, error_codes},
    chelix_sessions::{
        ui_history_engine::UiHistorySession,
        ui_history_types::{UiHistoryPage, UiHistoryRange},
    },
    serde::Deserialize,
    tokio::sync::mpsc,
    tokio_util::sync::CancellationToken,
};

use crate::{
    methods::{MethodContext, MethodResult},
    state::GatewayState,
};

pub struct HistorySubscription {
    sequence: u64,
    cancellation: CancellationToken,
}

impl HistorySubscription {
    pub fn cancel(&self) {
        self.cancellation.cancel();
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct HistorySubscriptionRequest {
    key: String,
    subscription_id: String,
    sequence: u64,
    range: UiHistoryRange,
    limit: usize,
}

pub async fn subscribe(ctx: MethodContext) -> MethodResult {
    if ctx.transport.is_stateless_http() {
        return Err(ErrorShape::new(
            error_codes::INVALID_REQUEST,
            "UI history subscriptions require WebSocket transport",
        ));
    }
    let request: HistorySubscriptionRequest = serde_json::from_value(ctx.params)
        .map_err(|error| ErrorShape::new(error_codes::INVALID_REQUEST, error.to_string()))?;
    if request.limit == 0 || request.limit > 500 || request.subscription_id.is_empty() {
        return Err(ErrorShape::new(
            error_codes::INVALID_REQUEST,
            "invalid UI history subscription ID or limit",
        ));
    }
    let cancellation = CancellationToken::new();
    let sender = {
        let mut registry = ctx.state.client_registry.write().await;
        let sender = registry
            .clients
            .get(&ctx.client_conn_id)
            .ok_or_else(|| ErrorShape::new(error_codes::INVALID_REQUEST, "connection is closed"))?
            .ui_history_sender
            .clone();
        if registry
            .ui_history_subscriptions
            .get(&ctx.client_conn_id)
            .is_some_and(|previous| previous.sequence >= request.sequence)
        {
            return Err(ErrorShape::new(
                error_codes::INVALID_REQUEST,
                "obsolete history subscription request",
            ));
        }
        if let Some(previous) = registry.ui_history_subscriptions.insert(
            ctx.client_conn_id.clone(),
            HistorySubscription {
                sequence: request.sequence,
                cancellation: cancellation.clone(),
            },
        ) {
            previous.cancel();
        }
        sender
    };
    let metadata = ctx
        .state
        .services
        .session_metadata
        .as_ref()
        .ok_or_else(|| ErrorShape::new(error_codes::INTERNAL, "session metadata unavailable"))?;
    if metadata
        .get(&request.key)
        .await
        .map_err(history_error)?
        .is_none()
    {
        return Err(ErrorShape::new(
            error_codes::INVALID_REQUEST,
            "session not found",
        ));
    }
    let store = ctx
        .state
        .services
        .session_store
        .as_ref()
        .ok_or_else(|| ErrorShape::new(error_codes::INTERNAL, "session store unavailable"))?;
    let session = store
        .ui_history
        .session(&request.key)
        .await
        .map_err(history_error)?;
    let changes = session.subscribe();
    let page = session
        .page(request.range.clone(), request.limit)
        .await
        .map_err(history_error)?;
    if cancellation.is_cancelled() {
        return Err(ErrorShape::new(
            error_codes::INVALID_REQUEST,
            "history subscription was replaced",
        ));
    }
    let result = serde_json::json!({
        "subscriptionId": request.subscription_id,
        "sessionKey": request.key,
        "snapshot": page.public_value().map_err(history_error)?,
    });
    tokio::spawn(async move {
        let outcome = tokio::select! {
            () = cancellation.cancelled() => Ok(()),
            result = deliver(&ctx.state, session, sender, changes, request, page) => result,
        };
        if let Err(error) = outcome
            && !cancellation.is_cancelled()
        {
            tracing::error!(conn_id = ctx.client_conn_id, %error, "UI history subscription failed");
            ctx.state.close_client(&ctx.client_conn_id).await;
        }
    });
    Ok(result)
}

fn history_error(error: impl std::fmt::Display) -> ErrorShape {
    ErrorShape::new(error_codes::INTERNAL, error.to_string())
}

async fn deliver(
    state: &Arc<GatewayState>,
    session: Arc<UiHistorySession>,
    sender: mpsc::Sender<String>,
    mut changes: tokio::sync::watch::Receiver<chelix_sessions::ui_history_types::UiHistoryRevision>,
    request: HistorySubscriptionRequest,
    page: UiHistoryPage,
) -> chelix_sessions::Result<()> {
    let mut generation = page.generation;
    let mut revision = page.revision;
    let mut selected_range = request.range.clone();
    loop {
        changes
            .changed()
            .await
            .map_err(|error| chelix_sessions::Error::message(error.to_string()))?;
        let permit = sender
            .reserve()
            .await
            .map_err(|error| chelix_sessions::Error::message(error.to_string()))?;
        let current = changes.borrow_and_update().clone();
        if let Some(failure) = current.failure {
            return Err(chelix_sessions::Error::message(failure));
        }
        if current.generation == generation && current.revision <= revision {
            continue;
        }
        let payload = match session
            .updates_since(&generation, revision, request.limit)
            .await?
        {
            Some(batch) => {
                revision = batch.revision;
                serde_json::json!({
                    "subscriptionId": request.subscription_id, "sessionKey": request.key,
                    "update": batch.public_value()?,
                })
            },
            None => {
                if current.generation != generation {
                    selected_range = UiHistoryRange::Latest;
                }
                let page = session.page(selected_range.clone(), request.limit).await?;
                generation = page.generation.clone();
                revision = page.revision;
                serde_json::json!({
                    "subscriptionId": request.subscription_id, "sessionKey": request.key,
                    "snapshot": page.public_value()?,
                })
            },
        };
        let frame = EventFrame::new("ui_history", payload, state.broadcaster.next_seq());
        permit.send(serde_json::to_string(&frame)?);
    }
}
