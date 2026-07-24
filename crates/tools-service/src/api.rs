use std::sync::Arc;

use {
    anyhow::Error,
    axum::{
        Json, Router,
        extract::{Query, State, WebSocketUpgrade},
        http::{HeaderMap, StatusCode, header::AUTHORIZATION},
        response::{IntoResponse, Response},
        routing::{get, post},
    },
    chelix_protocol::{
        CreateToolsServiceTerminalRequest, CreateToolsServiceTerminalResponse,
        ExecuteCommandRequest, ListDirectoryRequest, ListDirectoryResponse, ProcessRequest,
        ReadTerminalOutputRequest, RipgrepRequest, RipgrepResponse,
        TOOLS_SERVICE_EXECUTE_COMMAND_PATH, TOOLS_SERVICE_HEALTH_PATH,
        TOOLS_SERVICE_LIST_DIRECTORY_PATH, TOOLS_SERVICE_PROCESS_PATH,
        TOOLS_SERVICE_PROTOCOL_VERSION, TOOLS_SERVICE_READ_TERMINAL_OUTPUT_PATH,
        TOOLS_SERVICE_RIPGREP_PATH, TOOLS_SERVICE_TERMINAL_WS_PATH, TOOLS_SERVICE_TERMINALS_PATH,
        ToolsServiceError, ToolsServiceHealth, ToolsServiceTerminalAttachQuery,
        ToolsServiceTerminalsResponse,
    },
};

#[cfg(test)]
use axum::serve;

use crate::{interactive_terminal, list_directory, process, ripgrep, terminal::TerminalManager};

#[derive(Clone)]
struct ApiState {
    token: Arc<str>,
    terminal_manager: Arc<TerminalManager>,
}

pub fn router(token: String, terminal_manager: Arc<TerminalManager>) -> Router {
    Router::new()
        .route(TOOLS_SERVICE_HEALTH_PATH, get(health))
        .route(TOOLS_SERVICE_LIST_DIRECTORY_PATH, post(run_list_directory))
        .route(TOOLS_SERVICE_RIPGREP_PATH, post(run_ripgrep))
        .route(
            TOOLS_SERVICE_EXECUTE_COMMAND_PATH,
            post(run_execute_command),
        )
        .route(
            TOOLS_SERVICE_READ_TERMINAL_OUTPUT_PATH,
            post(run_read_terminal_output),
        )
        .route(TOOLS_SERVICE_PROCESS_PATH, post(run_process))
        .route(
            TOOLS_SERVICE_TERMINALS_PATH,
            get(list_terminals).post(create_terminal),
        )
        .route(TOOLS_SERVICE_TERMINAL_WS_PATH, get(attach_terminal))
        .with_state(ApiState {
            token: Arc::from(token),
            terminal_manager,
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

#[tracing::instrument(skip_all)]
async fn run_execute_command(
    State(state): State<ApiState>,
    headers: HeaderMap,
    Json(request): Json<ExecuteCommandRequest>,
) -> Response {
    if !is_authorized(&state, &headers) {
        return unauthorized_response();
    }

    match state.terminal_manager.execute_command(request).await {
        Ok(result) => Json(result).into_response(),
        Err(error) => tool_error_response(error),
    }
}

#[tracing::instrument(skip_all)]
async fn run_read_terminal_output(
    State(state): State<ApiState>,
    headers: HeaderMap,
    Json(request): Json<ReadTerminalOutputRequest>,
) -> Response {
    if !is_authorized(&state, &headers) {
        return unauthorized_response();
    }

    match state.terminal_manager.read_terminal_output(request).await {
        Ok(result) => Json(result).into_response(),
        Err(error) => tool_error_response(error),
    }
}

#[tracing::instrument(skip_all)]
async fn run_process(
    State(state): State<ApiState>,
    headers: HeaderMap,
    Json(request): Json<ProcessRequest>,
) -> Response {
    if !is_authorized(&state, &headers) {
        return unauthorized_response();
    }

    match process::run(&state.terminal_manager, request).await {
        Ok(result) => Json(result).into_response(),
        Err(error) => tool_error_response(error),
    }
}

#[tracing::instrument(skip_all)]
async fn list_terminals(State(state): State<ApiState>, headers: HeaderMap) -> Response {
    if !is_authorized(&state, &headers) {
        return unauthorized_response();
    }

    let result = state
        .terminal_manager
        .terminal_infos()
        .await
        .map(|terminals| ToolsServiceTerminalsResponse { terminals });
    match result {
        Ok(response) => Json(response).into_response(),
        Err(error) => tool_error_response(error),
    }
}

#[tracing::instrument(skip_all)]
async fn create_terminal(
    State(state): State<ApiState>,
    headers: HeaderMap,
    Json(request): Json<CreateToolsServiceTerminalRequest>,
) -> Response {
    if !is_authorized(&state, &headers) {
        return unauthorized_response();
    }

    match state
        .terminal_manager
        .create_interactive_terminal(&request.session_key, &request.env)
        .await
    {
        Ok(terminal) => Json(CreateToolsServiceTerminalResponse { terminal }).into_response(),
        Err(error) => tool_error_response(error),
    }
}

#[tracing::instrument(skip_all)]
async fn attach_terminal(
    State(state): State<ApiState>,
    headers: HeaderMap,
    Query(query): Query<ToolsServiceTerminalAttachQuery>,
    websocket: WebSocketUpgrade,
) -> Response {
    if !is_authorized(&state, &headers) {
        return unauthorized_response();
    }

    let terminal = state
        .terminal_manager
        .terminal_info(&query.session_key, &query.id)
        .await;
    let terminal = match terminal {
        Ok(terminal) => terminal,
        Err(error) => return tool_error_response(error),
    };
    let terminal_manager = Arc::clone(&state.terminal_manager);
    websocket
        .on_upgrade(move |socket| interactive_terminal::handle(socket, terminal_manager, terminal))
        .into_response()
}

fn tool_error_response(error: Error) -> Response {
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
        .get(AUTHORIZATION)
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

    use chelix_protocol::{
        ExecuteCommandResponse, ProcessAction, ProcessResponse, ReadTerminalOutputResponse,
    };

    use super::*;

    async fn spawn_api() -> String {
        let listener = tokio::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .await
            .unwrap_or_else(|error| panic!("bind failed: {error}"));
        let address = listener
            .local_addr()
            .unwrap_or_else(|error| panic!("local address failed: {error}"));
        tokio::spawn(async move {
            let terminal_manager = Arc::new(
                TerminalManager::new(std::env::temp_dir())
                    .unwrap_or_else(|error| panic!("terminal manager failed: {error}")),
            );
            if let Err(error) = serve(listener, router("test-token".into(), terminal_manager)).await
            {
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
    async fn terminal_inventory_requires_authorization() {
        let base_url = spawn_api().await;
        let response = reqwest::get(format!("{base_url}{TOOLS_SERVICE_TERMINALS_PATH}"))
            .await
            .unwrap_or_else(|error| panic!("request failed: {error}"));

        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn terminal_tool_routes_require_authorization() {
        let base_url = spawn_api().await;
        let client = reqwest::Client::new();
        let requests = [
            client
                .post(format!("{base_url}{TOOLS_SERVICE_EXECUTE_COMMAND_PATH}"))
                .json(&ExecuteCommandRequest {
                    session_key: "session:http".into(),
                    command: "printf ok".into(),
                    custom_cwd: None,
                    new_terminal: true,
                    background: false,
                    timeout_millis: 5_000,
                    terminal_id: None,
                    env: Vec::new(),
                }),
            client
                .post(format!(
                    "{base_url}{TOOLS_SERVICE_READ_TERMINAL_OUTPUT_PATH}"
                ))
                .json(&ReadTerminalOutputRequest {
                    session_key: "session:http".into(),
                    terminal_id: "1".into(),
                    max_lines: None,
                }),
            client
                .post(format!("{base_url}{TOOLS_SERVICE_PROCESS_PATH}"))
                .json(&ProcessRequest {
                    session_key: "session:http".into(),
                    action: ProcessAction::List,
                }),
        ];

        for request in requests {
            let response = request
                .send()
                .await
                .unwrap_or_else(|error| panic!("request failed: {error}"));
            assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
            let body = response
                .json::<ToolsServiceError>()
                .await
                .unwrap_or_else(|error| panic!("response decode failed: {error}"));
            assert_eq!(body.error, "unauthorized");
        }
    }

    #[tokio::test]
    async fn terminal_tool_routes_return_typed_success_responses() {
        let base_url = spawn_api().await;
        let client = reqwest::Client::new();
        let execute = client
            .post(format!("{base_url}{TOOLS_SERVICE_EXECUTE_COMMAND_PATH}"))
            .bearer_auth("test-token")
            .json(&ExecuteCommandRequest {
                session_key: "session:http".into(),
                command: "printf 'api-output\\n'".into(),
                custom_cwd: None,
                new_terminal: true,
                background: false,
                timeout_millis: 5_000,
                terminal_id: None,
                env: Vec::new(),
            })
            .send()
            .await
            .unwrap_or_else(|error| panic!("execute request failed: {error}"));
        assert_eq!(execute.status(), StatusCode::OK);
        let execute = execute
            .json::<ExecuteCommandResponse>()
            .await
            .unwrap_or_else(|error| panic!("execute response decode failed: {error}"));
        assert!(execute.completed);
        assert_eq!(execute.output.trim(), "api-output");
        assert!(execute.terminal_id.parse::<u64>().is_ok());

        let read = client
            .post(format!(
                "{base_url}{TOOLS_SERVICE_READ_TERMINAL_OUTPUT_PATH}"
            ))
            .bearer_auth("test-token")
            .json(&ReadTerminalOutputRequest {
                session_key: "session:http".into(),
                terminal_id: execute.terminal_id.clone(),
                max_lines: Some(20),
            })
            .send()
            .await
            .unwrap_or_else(|error| panic!("read request failed: {error}"));
        assert_eq!(read.status(), StatusCode::OK);
        let read = read
            .json::<ReadTerminalOutputResponse>()
            .await
            .unwrap_or_else(|error| panic!("read response decode failed: {error}"));
        assert_eq!(read.terminal_id, execute.terminal_id);
        assert!(read.output.contains("api-output"));
        assert!(read.completed);
        assert!(!read.running);

        let list = client
            .post(format!("{base_url}{TOOLS_SERVICE_PROCESS_PATH}"))
            .bearer_auth("test-token")
            .json(&ProcessRequest {
                session_key: "session:http".into(),
                action: ProcessAction::List,
            })
            .send()
            .await
            .unwrap_or_else(|error| panic!("process request failed: {error}"));
        assert_eq!(list.status(), StatusCode::OK);
        assert_eq!(
            list.json::<ProcessResponse>()
                .await
                .unwrap_or_else(|error| panic!("process response decode failed: {error}")),
            ProcessResponse::List {
                terminal_ids: vec![execute.terminal_id],
            }
        );
    }

    #[tokio::test]
    async fn terminal_tool_routes_return_typed_unprocessable_errors() {
        let base_url = spawn_api().await;
        let client = reqwest::Client::new();
        let responses = [
            client
                .post(format!("{base_url}{TOOLS_SERVICE_EXECUTE_COMMAND_PATH}"))
                .bearer_auth("test-token")
                .json(&ExecuteCommandRequest {
                    session_key: "session:http".into(),
                    command: String::new(),
                    custom_cwd: None,
                    new_terminal: true,
                    background: false,
                    timeout_millis: 5_000,
                    terminal_id: None,
                    env: Vec::new(),
                })
                .send()
                .await
                .unwrap_or_else(|error| panic!("execute request failed: {error}")),
            client
                .post(format!(
                    "{base_url}{TOOLS_SERVICE_READ_TERMINAL_OUTPUT_PATH}"
                ))
                .bearer_auth("test-token")
                .json(&ReadTerminalOutputRequest {
                    session_key: "session:http".into(),
                    terminal_id: "404".into(),
                    max_lines: None,
                })
                .send()
                .await
                .unwrap_or_else(|error| panic!("read request failed: {error}")),
            client
                .post(format!("{base_url}{TOOLS_SERVICE_PROCESS_PATH}"))
                .bearer_auth("test-token")
                .json(&ProcessRequest {
                    session_key: String::new(),
                    action: ProcessAction::List,
                })
                .send()
                .await
                .unwrap_or_else(|error| panic!("process request failed: {error}")),
        ];
        let expected_errors = [
            "command cannot be empty",
            "terminal 404 was not found",
            "session_key cannot be empty",
        ];

        for (response, expected_error) in responses.into_iter().zip(expected_errors) {
            assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
            let body = response
                .json::<ToolsServiceError>()
                .await
                .unwrap_or_else(|error| panic!("response decode failed: {error}"));
            assert_eq!(body.error, expected_error);
        }
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
