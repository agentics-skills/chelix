use std::net::SocketAddr;

use {
    axum::{
        Json,
        extract::{ConnectInfo, Path, Query, State, WebSocketUpgrade},
        http::{
            HeaderMap, StatusCode,
            header::{HOST, ORIGIN},
        },
        response::{IntoResponse, Response},
    },
    chelix_httpd::AppState,
    tracing::warn,
};

use super::{
    auth::{is_local_connection, is_same_origin, websocket_header_authenticated},
    types::{
        CreateTerminalRequest, TERMINAL_DISABLED, TERMINAL_REQUEST_FAILED,
        TERMINAL_SERVICE_UNAVAILABLE, TerminalSessionQuery, TerminalWsQuery, terminal_error,
    },
    websocket,
};

fn terminal_disabled_response(state: &AppState) -> Option<Response> {
    if state.gateway.config.server.is_terminal_enabled() {
        return None;
    }
    Some(
        (
            StatusCode::FORBIDDEN,
            Json(terminal_error(
                TERMINAL_DISABLED,
                "terminal has been disabled by the server administrator",
            )),
        )
            .into_response(),
    )
}

fn tools_service(
    state: &AppState,
) -> Option<&std::sync::Arc<chelix_tools::tools_service::ManagedToolsService>> {
    state.gateway.tools_service()
}

fn tools_service_unavailable_response() -> Response {
    (
        StatusCode::SERVICE_UNAVAILABLE,
        Json(terminal_error(
            TERMINAL_SERVICE_UNAVAILABLE,
            "managed tools service is unavailable",
        )),
    )
        .into_response()
}

pub async fn api_terminal_instances_handler(State(state): State<AppState>) -> Response {
    if let Some(response) = terminal_disabled_response(&state) {
        return response;
    }
    let service = match tools_service(&state) {
        Some(service) => service,
        None => return tools_service_unavailable_response(),
    };
    match service.terminal_instances().await {
        Ok(instances) => Json(serde_json::json!({ "instances": instances })).into_response(),
        Err(error) => (
            StatusCode::BAD_GATEWAY,
            Json(terminal_error(TERMINAL_REQUEST_FAILED, error.to_string())),
        )
            .into_response(),
    }
}

pub async fn api_session_terminals_handler(
    State(state): State<AppState>,
    Query(query): Query<TerminalSessionQuery>,
) -> Response {
    if let Some(response) = terminal_disabled_response(&state) {
        return response;
    }
    let service = match tools_service(&state) {
        Some(service) => service,
        None => return tools_service_unavailable_response(),
    };
    match service.session_terminals(&query.session_key).await {
        Ok(response) => Json(response).into_response(),
        Err(error) => (
            StatusCode::BAD_GATEWAY,
            Json(terminal_error(TERMINAL_REQUEST_FAILED, error.to_string())),
        )
            .into_response(),
    }
}

pub async fn api_session_terminal_create_handler(
    State(state): State<AppState>,
    Json(request): Json<CreateTerminalRequest>,
) -> Response {
    if let Some(response) = terminal_disabled_response(&state) {
        return response;
    }
    let service = match tools_service(&state) {
        Some(service) => service,
        None => return tools_service_unavailable_response(),
    };
    match service.create_session_terminal(&request.session_key).await {
        Ok(response) => Json(response).into_response(),
        Err(error) => (
            StatusCode::BAD_GATEWAY,
            Json(terminal_error(TERMINAL_REQUEST_FAILED, error.to_string())),
        )
            .into_response(),
    }
}

pub async fn api_terminal_create_handler(
    State(state): State<AppState>,
    Path(instance_id): Path<String>,
    Json(request): Json<CreateTerminalRequest>,
) -> Response {
    if let Some(response) = terminal_disabled_response(&state) {
        return response;
    }
    let service = match tools_service(&state) {
        Some(service) => service,
        None => return tools_service_unavailable_response(),
    };
    match service
        .create_terminal(&instance_id, &request.session_key)
        .await
    {
        Ok(response) => Json(response).into_response(),
        Err(error) => (
            StatusCode::BAD_GATEWAY,
            Json(terminal_error(TERMINAL_REQUEST_FAILED, error.to_string())),
        )
            .into_response(),
    }
}

pub async fn api_terminal_ws_upgrade_handler(
    websocket_upgrade: WebSocketUpgrade,
    Query(query): Query<TerminalWsQuery>,
    headers: HeaderMap,
    ConnectInfo(address): ConnectInfo<SocketAddr>,
    State(state): State<AppState>,
) -> Response {
    if let Some(response) = terminal_disabled_response(&state) {
        return response;
    }

    if let Some(origin) = headers.get(ORIGIN).and_then(|value| value.to_str().ok()) {
        let host = headers
            .get(HOST)
            .and_then(|value| value.to_str().ok())
            .unwrap_or("");
        if !is_same_origin(origin, host) {
            warn!(origin, host, remote = %address, "rejected cross-origin terminal websocket upgrade");
            return (
                StatusCode::FORBIDDEN,
                "cross-origin WebSocket connections are not allowed",
            )
                .into_response();
        }
    }

    let is_local = is_local_connection(&headers, address, state.gateway.behind_proxy);
    if !websocket_header_authenticated(&headers, state.gateway.credential_store.as_ref(), is_local)
        .await
    {
        return (
            StatusCode::UNAUTHORIZED,
            Json(terminal_error(
                "AUTH_NOT_AUTHENTICATED",
                "not authenticated",
            )),
        )
            .into_response();
    }

    let service = match tools_service(&state) {
        Some(service) => service,
        None => return tools_service_unavailable_response(),
    };
    let instance_id = query.instance_id.clone();
    let attach_query = query.into();
    let upstream = match service.connect_terminal(&instance_id, &attach_query).await {
        Ok(upstream) => upstream,
        Err(error) => {
            return (
                StatusCode::BAD_GATEWAY,
                Json(terminal_error(TERMINAL_REQUEST_FAILED, error.to_string())),
            )
                .into_response();
        },
    };

    websocket_upgrade
        .on_upgrade(move |browser| websocket::proxy(browser, upstream))
        .into_response()
}
