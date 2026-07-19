use std::sync::Arc;

use {
    axum::{
        Json, Router,
        extract::State,
        http::{HeaderMap, StatusCode},
        response::{IntoResponse, Response},
        routing::{get, post},
    },
    chelix_protocol::{
        ListDirectoryRequest, ListDirectoryResponse, RipgrepRequest, RipgrepResponse,
        TOOLS_SERVICE_HEALTH_PATH, TOOLS_SERVICE_LIST_DIRECTORY_PATH,
        TOOLS_SERVICE_PROTOCOL_VERSION, TOOLS_SERVICE_RIPGREP_PATH, ToolsServiceError,
        ToolsServiceHealth,
    },
};

use crate::{list_directory, ripgrep};

#[derive(Clone)]
struct ApiState {
    token: Arc<str>,
}

pub fn router(token: String) -> Router {
    Router::new()
        .route(TOOLS_SERVICE_HEALTH_PATH, get(health))
        .route(TOOLS_SERVICE_LIST_DIRECTORY_PATH, post(run_list_directory))
        .route(TOOLS_SERVICE_RIPGREP_PATH, post(run_ripgrep))
        .with_state(ApiState {
            token: Arc::from(token),
        })
}

async fn health(State(state): State<ApiState>, headers: HeaderMap) -> Response {
    if !is_authorized(&state, &headers) {
        return unauthorized_response();
    }

    Json(ToolsServiceHealth {
        protocol_version: TOOLS_SERVICE_PROTOCOL_VERSION,
    })
    .into_response()
}

#[tracing::instrument(skip_all)]
async fn run_list_directory(
    State(state): State<ApiState>,
    headers: HeaderMap,
    Json(request): Json<ListDirectoryRequest>,
) -> Response {
    if !is_authorized(&state, &headers) {
        return unauthorized_response();
    }

    match list_directory::run_tool(&request.path).await {
        Ok(result) => Json(ListDirectoryResponse { result }).into_response(),
        Err(error) => tool_error_response(error),
    }
}

#[tracing::instrument(skip_all)]
async fn run_ripgrep(
    State(state): State<ApiState>,
    headers: HeaderMap,
    Json(request): Json<RipgrepRequest>,
) -> Response {
    if !is_authorized(&state, &headers) {
        return unauthorized_response();
    }

    match ripgrep::run_tool(request.params).await {
        Ok(result) => Json(RipgrepResponse { result }).into_response(),
        Err(error) => tool_error_response(error),
    }
}

fn tool_error_response(error: anyhow::Error) -> Response {
    (
        StatusCode::UNPROCESSABLE_ENTITY,
        Json(ToolsServiceError {
            error: error.to_string(),
        }),
    )
        .into_response()
}

fn is_authorized(state: &ApiState, headers: &HeaderMap) -> bool {
    let expected = format!("Bearer {}", state.token);
    headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value == expected)
}

fn unauthorized_response() -> Response {
    (
        StatusCode::UNAUTHORIZED,
        Json(ToolsServiceError {
            error: "unauthorized".into(),
        }),
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use std::net::Ipv4Addr;

    use super::*;

    async fn spawn_api() -> String {
        let listener = tokio::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .await
            .unwrap_or_else(|error| panic!("bind failed: {error}"));
        let address = listener
            .local_addr()
            .unwrap_or_else(|error| panic!("local address failed: {error}"));
        tokio::spawn(async move {
            if let Err(error) = axum::serve(listener, router("test-token".into())).await {
                panic!("test server failed: {error}");
            }
        });
        format!("http://{address}")
    }

    #[tokio::test]
    async fn health_requires_authorization() {
        let base_url = spawn_api().await;
        let response = reqwest::get(format!("{base_url}{TOOLS_SERVICE_HEALTH_PATH}"))
            .await
            .unwrap_or_else(|error| panic!("request failed: {error}"));

        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn list_directory_requires_authorization() {
        let base_url = spawn_api().await;
        let response = reqwest::Client::new()
            .post(format!("{base_url}{TOOLS_SERVICE_LIST_DIRECTORY_PATH}"))
            .json(&ListDirectoryRequest { path: "/".into() })
            .send()
            .await
            .unwrap_or_else(|error| panic!("request failed: {error}"));

        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn ripgrep_runs_with_authorization() {
        let dir = tempfile::tempdir().unwrap_or_else(|error| panic!("tempdir failed: {error}"));
        tokio::fs::write(dir.path().join("sample.txt"), "service-needle\n")
            .await
            .unwrap_or_else(|error| panic!("write failed: {error}"));
        let base_url = spawn_api().await;
        let response = reqwest::Client::new()
            .post(format!("{base_url}{TOOLS_SERVICE_RIPGREP_PATH}"))
            .bearer_auth("test-token")
            .json(&RipgrepRequest {
                params: serde_json::json!({
                    "pattern": "service-needle",
                    "fixedStrings": true,
                    "cwd": dir.path(),
                }),
            })
            .send()
            .await
            .unwrap_or_else(|error| panic!("request failed: {error}"));

        assert_eq!(response.status(), StatusCode::OK);
        let body = response
            .json::<RipgrepResponse>()
            .await
            .unwrap_or_else(|error| panic!("response decode failed: {error}"));
        assert_eq!(body.result["found"], true);
        assert_eq!(body.result["summary"]["matchCount"], 1);
    }

    #[tokio::test]
    async fn list_directory_runs_with_authorization() {
        let dir = tempfile::tempdir().unwrap_or_else(|error| panic!("tempdir failed: {error}"));
        tokio::fs::write(dir.path().join("sample.txt"), "first\nsecond")
            .await
            .unwrap_or_else(|error| panic!("write failed: {error}"));
        let base_url = spawn_api().await;
        let response = reqwest::Client::new()
            .post(format!("{base_url}{TOOLS_SERVICE_LIST_DIRECTORY_PATH}"))
            .bearer_auth("test-token")
            .json(&ListDirectoryRequest {
                path: dir.path().to_string_lossy().into_owned(),
            })
            .send()
            .await
            .unwrap_or_else(|error| panic!("request failed: {error}"));

        assert_eq!(response.status(), StatusCode::OK);
        let body = response
            .json::<ListDirectoryResponse>()
            .await
            .unwrap_or_else(|error| panic!("response decode failed: {error}"));
        assert_eq!(body.result, "sample.txt (2 lines)");
    }

    #[tokio::test]
    async fn list_directory_surfaces_filesystem_errors() {
        let base_url = spawn_api().await;
        let response = reqwest::Client::new()
            .post(format!("{base_url}{TOOLS_SERVICE_LIST_DIRECTORY_PATH}"))
            .bearer_auth("test-token")
            .json(&ListDirectoryRequest {
                path: "/definitely/not/a/real/list-directory-path".into(),
            })
            .send()
            .await
            .unwrap_or_else(|error| panic!("request failed: {error}"));

        assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
        let body = response
            .json::<ToolsServiceError>()
            .await
            .unwrap_or_else(|error| panic!("response decode failed: {error}"));
        assert!(body.error.contains("failed to read directory"));
    }
}
