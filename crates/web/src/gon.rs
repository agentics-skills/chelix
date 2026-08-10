//! `/api/gon` handler — returns server-side gon data as JSON.

use {
    axum::{
        Json,
        extract::State,
        http::StatusCode,
        response::{IntoResponse, Response},
    },
    chelix_httpd::AppState,
};

use crate::templates::build_gon_data;

pub async fn api_gon_handler(State(state): State<AppState>) -> Response {
    match build_gon_data(&state.gateway).await {
        Ok(data) => Json(data).into_response(),
        Err(error) => (StatusCode::INTERNAL_SERVER_ERROR, error.to_string()).into_response(),
    }
}

#[derive(serde::Serialize)]
struct PublicIdentityPayload {
    identity: PublicIdentity,
    graphql_enabled: bool,
}

#[derive(serde::Serialize)]
struct PublicIdentity {
    name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    emoji: Option<String>,
}

/// Public branding payload for unauthenticated discovery clients.
pub async fn api_public_identity_handler(State(state): State<AppState>) -> Response {
    let identity = match crate::resolve_default_agent_presentation(&state.gateway).await {
        Ok(identity) => identity,
        Err(error) => {
            return (StatusCode::INTERNAL_SERVER_ERROR, error.to_string()).into_response();
        },
    };

    let emoji = identity.emoji.and_then(|raw| {
        let trimmed = raw.trim().to_owned();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed)
        }
    });

    #[cfg(feature = "graphql")]
    let graphql_enabled = state.gateway.is_graphql_enabled();
    #[cfg(not(feature = "graphql"))]
    let graphql_enabled = false;

    Json(PublicIdentityPayload {
        identity: PublicIdentity {
            name: identity.name,
            emoji,
        },
        graphql_enabled,
    })
    .into_response()
}
