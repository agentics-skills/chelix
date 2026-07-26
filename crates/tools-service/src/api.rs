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
        CreateToolsServiceTerminalRequest, CreateToolsServiceTerminalResponse, EditFileRequest,
        ExecuteCommandRequest, ListDirectoryRequest, ListDirectoryResponse, OverwriteFileRequest,
        ProcessRequest, ReadFileRequest, ReadFileResponse, ReadMediaRequest,
        ReadTerminalOutputRequest, RipgrepRequest, RipgrepResponse, TOOLS_SERVICE_EDIT_FILE_PATH,
        TOOLS_SERVICE_EXECUTE_COMMAND_PATH, TOOLS_SERVICE_HEALTH_PATH,
        TOOLS_SERVICE_LIST_DIRECTORY_PATH, TOOLS_SERVICE_OVERWRITE_FILE_PATH,
        TOOLS_SERVICE_PROCESS_PATH, TOOLS_SERVICE_PROTOCOL_VERSION, TOOLS_SERVICE_READ_FILE_PATH,
        TOOLS_SERVICE_READ_MEDIA_PATH, TOOLS_SERVICE_READ_TERMINAL_OUTPUT_PATH,
        TOOLS_SERVICE_RIPGREP_PATH, TOOLS_SERVICE_TERMINAL_WS_PATH, TOOLS_SERVICE_TERMINALS_PATH,
        ToolsServiceError, ToolsServiceHealth, ToolsServiceTerminalAttachQuery,
        ToolsServiceTerminalsResponse,
    },
};

#[cfg(test)]
use axum::serve;

use crate::{
    edit_file::{self, EditFileRuntime},
    interactive_terminal, list_directory, overwrite_file, process, read_file, read_media,
    ripgrep::{self, RipgrepRuntime},
    terminal::TerminalManager,
};

#[derive(Clone)]
struct ApiState {
    token: Arc<str>,
    edit_file_runtime: Arc<EditFileRuntime>,
    terminal_manager: Arc<TerminalManager>,
    ripgrep_runtime: Arc<RipgrepRuntime>,
}

pub fn router(
    token: String,
    terminal_manager: Arc<TerminalManager>,
    ripgrep_runtime: Arc<RipgrepRuntime>,
) -> Router {
    Router::new()
        .route(TOOLS_SERVICE_HEALTH_PATH, get(health))
        .route(TOOLS_SERVICE_EDIT_FILE_PATH, post(run_edit_file))
        .route(TOOLS_SERVICE_LIST_DIRECTORY_PATH, post(run_list_directory))
        .route(TOOLS_SERVICE_OVERWRITE_FILE_PATH, post(run_overwrite_file))
        .route(TOOLS_SERVICE_READ_FILE_PATH, post(run_read_file))
        .route(TOOLS_SERVICE_READ_MEDIA_PATH, post(run_read_media))
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
            edit_file_runtime: Arc::new(EditFileRuntime::default()),
            terminal_manager,
            ripgrep_runtime,
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
async fn run_edit_file(
    State(state): State<ApiState>,
    headers: HeaderMap,
    Json(request): Json<EditFileRequest>,
) -> Response {
    if !is_authorized(&state, &headers) {
        return unauthorized_response();
    }

    match edit_file::run_tool(request, &state.edit_file_runtime).await {
        Ok(response) => Json(response).into_response(),
        Err(error) => tool_error_response(error),
    }
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
async fn run_overwrite_file(
    State(state): State<ApiState>,
    headers: HeaderMap,
    Json(request): Json<OverwriteFileRequest>,
) -> Response {
    if !is_authorized(&state, &headers) {
        return unauthorized_response();
    }

    match overwrite_file::run_tool(request).await {
        Ok(response) => Json(response).into_response(),
        Err(error) => tool_error_response(error),
    }
}

#[tracing::instrument(skip_all)]
async fn run_read_file(
    State(state): State<ApiState>,
    headers: HeaderMap,
    Json(request): Json<ReadFileRequest>,
) -> Response {
    if !is_authorized(&state, &headers) {
        return unauthorized_response();
    }

    match read_file::run_tool(request).await {
        Ok(result) => Json(ReadFileResponse { result }).into_response(),
        Err(error) => tool_error_response(error),
    }
}

#[tracing::instrument(skip_all)]
async fn run_read_media(
    State(state): State<ApiState>,
    headers: HeaderMap,
    Json(request): Json<ReadMediaRequest>,
) -> Response {
    if !is_authorized(&state, &headers) {
        return unauthorized_response();
    }

    match read_media::run_tool(request).await {
        Ok(result) => Json(result).into_response(),
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

    match ripgrep::run_tool(request.params, &state.ripgrep_runtime).await {
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
        EditFileResponse, ExecuteCommandResponse, OverwriteFileResponse, ProcessAction,
        ProcessResponse, ReadMediaResponse, ReadTerminalOutputResponse,
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
            let working_dir = std::env::temp_dir();
            let terminal_manager = Arc::new(
                TerminalManager::new(working_dir.clone())
                    .unwrap_or_else(|error| panic!("terminal manager failed: {error}")),
            );
            let ripgrep_runtime = RipgrepRuntime::initialize(working_dir)
                .await
                .unwrap_or_else(|error| panic!("ripgrep runtime failed: {error}"));
            if let Err(error) = serve(
                listener,
                router("test-token".into(), terminal_manager, ripgrep_runtime),
            )
            .await
            {
                panic!("test server failed: {error}");
            }
        });
        format!("http://{address}")
    }

    fn tiny_jpeg_bytes() -> Vec<u8> {
        #[rustfmt::skip]
        const TINY_JPEG: &[u8] = &[
            0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x10, 0x4A, 0x46, 0x49, 0x46, 0x00, 0x01, 0x01, 0x00, 0x00,
            0x01, 0x00, 0x01, 0x00, 0x00, 0xFF, 0xDB, 0x00, 0x43, 0x00, 0x08, 0x06, 0x06, 0x07, 0x06,
            0x05, 0x08, 0x07, 0x07, 0x07, 0x09, 0x09, 0x08, 0x0A, 0x0C, 0x14, 0x0D, 0x0C, 0x0B, 0x0B,
            0x0C, 0x19, 0x12, 0x13, 0x0F, 0x14, 0x1D, 0x1A, 0x1F, 0x1E, 0x1D, 0x1A, 0x1C, 0x1C, 0x20,
            0x24, 0x2E, 0x27, 0x20, 0x22, 0x2C, 0x23, 0x1C, 0x1C, 0x28, 0x37, 0x29, 0x2C, 0x30, 0x31,
            0x34, 0x34, 0x34, 0x1F, 0x27, 0x39, 0x3D, 0x38, 0x32, 0x3C, 0x2E, 0x33, 0x34, 0x32, 0xFF,
            0xC0, 0x00, 0x0B, 0x08, 0x00, 0x01, 0x00, 0x01, 0x01, 0x01, 0x11, 0x00, 0xFF, 0xC4, 0x00,
            0x1F, 0x00, 0x00, 0x01, 0x05, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0A, 0x0B,
            0xFF, 0xC4, 0x00, 0xB5, 0x10, 0x00, 0x02, 0x01, 0x03, 0x03, 0x02, 0x04, 0x03, 0x05, 0x05,
            0x04, 0x04, 0x00, 0x00, 0x01, 0x7D, 0x01, 0x02, 0x03, 0x00, 0x04, 0x11, 0x05, 0x12, 0x21,
            0x31, 0x41, 0x06, 0x13, 0x51, 0x61, 0x07, 0x22, 0x71, 0x14, 0x32, 0x81, 0x91, 0xA1, 0x08,
            0x23, 0x42, 0xB1, 0xC1, 0x15, 0x52, 0xD1, 0xF0, 0x24, 0x33, 0x62, 0x72, 0x82, 0x09, 0x0A,
            0x16, 0x17, 0x18, 0x19, 0x1A, 0x25, 0x26, 0x27, 0x28, 0x29, 0x2A, 0x34, 0x35, 0x36, 0x37,
            0x38, 0x39, 0x3A, 0x43, 0x44, 0x45, 0x46, 0x47, 0x48, 0x49, 0x4A, 0x53, 0x54, 0x55, 0x56,
            0x57, 0x58, 0x59, 0x5A, 0x63, 0x64, 0x65, 0x66, 0x67, 0x68, 0x69, 0x6A, 0x73, 0x74, 0x75,
            0x76, 0x77, 0x78, 0x79, 0x7A, 0x83, 0x84, 0x85, 0x86, 0x87, 0x88, 0x89, 0x8A, 0x92, 0x93,
            0x94, 0x95, 0x96, 0x97, 0x98, 0x99, 0x9A, 0xA2, 0xA3, 0xA4, 0xA5, 0xA6, 0xA7, 0xA8, 0xA9,
            0xAA, 0xB2, 0xB3, 0xB4, 0xB5, 0xB6, 0xB7, 0xB8, 0xB9, 0xBA, 0xC2, 0xC3, 0xC4, 0xC5, 0xC6,
            0xC7, 0xC8, 0xC9, 0xCA, 0xD2, 0xD3, 0xD4, 0xD5, 0xD6, 0xD7, 0xD8, 0xD9, 0xDA, 0xE1, 0xE2,
            0xE3, 0xE4, 0xE5, 0xE6, 0xE7, 0xE8, 0xE9, 0xEA, 0xF1, 0xF2, 0xF3, 0xF4, 0xF5, 0xF6, 0xF7,
            0xF8, 0xF9, 0xFA, 0xFF, 0xDA, 0x00, 0x08, 0x01, 0x01, 0x00, 0x00, 0x3F, 0x00, 0xFB, 0xD5,
            0xDB, 0x20, 0xA8, 0xBA, 0xA3, 0xE8, 0xEB, 0xEC, 0x00, 0x3C, 0xF4, 0x76, 0x19, 0xE8, 0x78,
            0xAD, 0x99, 0xA0, 0x19, 0xE0, 0xD0, 0x6A, 0x40, 0x23, 0x9C, 0xD0, 0x07, 0xFF, 0xD9,
        ];

        TINY_JPEG.to_vec()
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
    async fn edit_file_requires_authorization() {
        let base_url = spawn_api().await;
        let response = reqwest::Client::new()
            .post(format!("{base_url}{TOOLS_SERVICE_EDIT_FILE_PATH}"))
            .json(&serde_json::json!({
                "filePath": "/tmp/file.txt",
                "edit": {
                    "oldString": "old",
                    "newString": "new"
                }
            }))
            .send()
            .await
            .unwrap_or_else(|error| panic!("request failed: {error}"));

        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn read_file_requires_authorization() {
        let base_url = spawn_api().await;
        let response = reqwest::Client::new()
            .post(format!("{base_url}{TOOLS_SERVICE_READ_FILE_PATH}"))
            .json(&serde_json::json!({ "filePath": "/tmp/file.txt" }))
            .send()
            .await
            .unwrap_or_else(|error| panic!("request failed: {error}"));

        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn overwrite_file_requires_authorization() {
        let base_url = spawn_api().await;
        let response = reqwest::Client::new()
            .post(format!("{base_url}{TOOLS_SERVICE_OVERWRITE_FILE_PATH}"))
            .json(&serde_json::json!({
                "filePath": "/tmp/file.txt",
                "content": "value"
            }))
            .send()
            .await
            .unwrap_or_else(|error| panic!("request failed: {error}"));

        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn read_media_requires_authorization() {
        let base_url = spawn_api().await;
        let response = reqwest::Client::new()
            .post(format!("{base_url}{TOOLS_SERVICE_READ_MEDIA_PATH}"))
            .json(&serde_json::json!({ "filePath": "/tmp/image.png" }))
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
            .json(&serde_json::json!({
                "params": {
                    "pattern": "service-needle",
                    "fixedStrings": true,
                    "cwd": dir.path(),
                }
            }))
            .send()
            .await
            .unwrap_or_else(|error| panic!("request failed: {error}"));

        assert_eq!(response.status(), StatusCode::OK);
        let body = response
            .json::<RipgrepResponse>()
            .await
            .unwrap_or_else(|error| panic!("response decode failed: {error}"));
        assert!(body.result.found);
        assert_eq!(body.result.summary.match_count, 1);
    }

    #[tokio::test]
    async fn ripgrep_rejects_null_and_unknown_parameters() {
        let base_url = spawn_api().await;
        let client = reqwest::Client::new();
        for params in [
            serde_json::json!({ "pattern": "needle", "cwd": null }),
            serde_json::json!({ "pattern": "needle", "obsolete": true }),
        ] {
            let response = client
                .post(format!("{base_url}{TOOLS_SERVICE_RIPGREP_PATH}"))
                .bearer_auth("test-token")
                .json(&serde_json::json!({ "params": params }))
                .send()
                .await
                .unwrap_or_else(|error| panic!("request failed: {error}"));

            assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
        }
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
    async fn read_file_runs_with_authorization_and_surfaces_errors() {
        let dir = tempfile::tempdir().unwrap_or_else(|error| panic!("tempdir failed: {error}"));
        let path = dir.path().join("sample.txt");
        tokio::fs::write(&path, "first\nsecond\nthird")
            .await
            .unwrap_or_else(|error| panic!("write failed: {error}"));
        let base_url = spawn_api().await;
        let client = reqwest::Client::new();
        let response = client
            .post(format!("{base_url}{TOOLS_SERVICE_READ_FILE_PATH}"))
            .bearer_auth("test-token")
            .json(&serde_json::json!({
                "filePath": path,
                "offset": 2,
                "limit": 2
            }))
            .send()
            .await
            .unwrap_or_else(|error| panic!("request failed: {error}"));

        assert_eq!(response.status(), StatusCode::OK);
        let body = response
            .json::<ReadFileResponse>()
            .await
            .unwrap_or_else(|error| panic!("response decode failed: {error}"));
        assert_eq!(body.result, "second\nthird");

        let response = client
            .post(format!("{base_url}{TOOLS_SERVICE_READ_FILE_PATH}"))
            .bearer_auth("test-token")
            .json(&serde_json::json!({
                "filePath": "/definitely/not/a/real/read-file-path"
            }))
            .send()
            .await
            .unwrap_or_else(|error| panic!("request failed: {error}"));
        assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
        let body = response
            .json::<ToolsServiceError>()
            .await
            .unwrap_or_else(|error| panic!("response decode failed: {error}"));
        assert!(body.error.contains("failed to open file"));
    }

    #[tokio::test]
    async fn edit_file_runs_with_authorization_and_surfaces_errors() {
        let directory =
            tempfile::tempdir().unwrap_or_else(|error| panic!("tempdir failed: {error}"));
        let path = directory.path().join("sample.txt");
        tokio::fs::write(&path, "old value and old value")
            .await
            .unwrap_or_else(|error| panic!("write failed: {error}"));
        let base_url = spawn_api().await;
        let client = reqwest::Client::new();
        let response = client
            .post(format!("{base_url}{TOOLS_SERVICE_EDIT_FILE_PATH}"))
            .bearer_auth("test-token")
            .json(&serde_json::json!({
                "filePath": path,
                "edit": {
                    "oldString": "old value and old value",
                    "newString": "new value"
                }
            }))
            .send()
            .await
            .unwrap_or_else(|error| panic!("request failed: {error}"));

        assert_eq!(response.status(), StatusCode::OK);
        let body = response
            .json::<EditFileResponse>()
            .await
            .unwrap_or_else(|error| panic!("response decode failed: {error}"));
        assert_eq!(body.replacements, 1);
        assert_eq!(
            tokio::fs::read_to_string(&path)
                .await
                .unwrap_or_else(|error| panic!("read failed: {error}")),
            "new value"
        );

        tokio::fs::write(&path, "duplicate duplicate")
            .await
            .unwrap_or_else(|error| panic!("write failed: {error}"));
        let response = client
            .post(format!("{base_url}{TOOLS_SERVICE_EDIT_FILE_PATH}"))
            .bearer_auth("test-token")
            .json(&serde_json::json!({
                "filePath": path,
                "edit": {
                    "oldString": "duplicate",
                    "newString": "replacement"
                }
            }))
            .send()
            .await
            .unwrap_or_else(|error| panic!("request failed: {error}"));
        assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
        let body = response
            .json::<ToolsServiceError>()
            .await
            .unwrap_or_else(|error| panic!("response decode failed: {error}"));
        assert!(body.error.contains("matches 2 locations"));
        assert_eq!(
            tokio::fs::read_to_string(&path)
                .await
                .unwrap_or_else(|error| panic!("read failed: {error}")),
            "duplicate duplicate"
        );
    }

    #[tokio::test]
    async fn overwrite_file_runs_with_authorization_and_surfaces_errors() {
        let directory =
            tempfile::tempdir().unwrap_or_else(|error| panic!("tempdir failed: {error}"));
        let path = directory.path().join("sample.txt");
        tokio::fs::write(&path, "old")
            .await
            .unwrap_or_else(|error| panic!("write failed: {error}"));
        let base_url = spawn_api().await;
        let client = reqwest::Client::new();
        let response = client
            .post(format!("{base_url}{TOOLS_SERVICE_OVERWRITE_FILE_PATH}"))
            .bearer_auth("test-token")
            .json(&serde_json::json!({
                "filePath": path,
                "content": "new value"
            }))
            .send()
            .await
            .unwrap_or_else(|error| panic!("request failed: {error}"));

        assert_eq!(response.status(), StatusCode::OK);
        let body = response
            .json::<OverwriteFileResponse>()
            .await
            .unwrap_or_else(|error| panic!("response decode failed: {error}"));
        assert_eq!(body.bytes_written, 9);
        assert_eq!(
            tokio::fs::read_to_string(&path)
                .await
                .unwrap_or_else(|error| panic!("read failed: {error}")),
            "new value"
        );

        let response = client
            .post(format!("{base_url}{TOOLS_SERVICE_OVERWRITE_FILE_PATH}"))
            .bearer_auth("test-token")
            .json(&serde_json::json!({
                "filePath": "relative.txt",
                "content": "value"
            }))
            .send()
            .await
            .unwrap_or_else(|error| panic!("request failed: {error}"));
        assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
        let body = response
            .json::<ToolsServiceError>()
            .await
            .unwrap_or_else(|error| panic!("response decode failed: {error}"));
        assert_eq!(body.error, "filePath must be absolute.");
    }

    #[tokio::test]
    async fn read_media_runs_with_authorization_and_surfaces_decode_errors() {
        let dir = tempfile::tempdir().unwrap_or_else(|error| panic!("tempdir failed: {error}"));
        let image_path = dir.path().join("sample.bin");
        tokio::fs::write(&image_path, tiny_jpeg_bytes())
            .await
            .unwrap_or_else(|error| panic!("write failed: {error}"));
        let broken_path = dir.path().join("broken.png");
        tokio::fs::write(&broken_path, b"not actually png data")
            .await
            .unwrap_or_else(|error| panic!("write failed: {error}"));
        let base_url = spawn_api().await;
        let client = reqwest::Client::new();

        let response = client
            .post(format!("{base_url}{TOOLS_SERVICE_READ_MEDIA_PATH}"))
            .bearer_auth("test-token")
            .json(&serde_json::json!({
                "filePath": image_path,
            }))
            .send()
            .await
            .unwrap_or_else(|error| panic!("request failed: {error}"));

        assert_eq!(response.status(), StatusCode::OK);
        let body = response
            .json::<ReadMediaResponse>()
            .await
            .unwrap_or_else(|error| panic!("response decode failed: {error}"));
        match body {
            ReadMediaResponse::Image {
                media_type,
                original_width,
                original_height,
                ..
            } => {
                assert_eq!(media_type, "image/jpeg");
                assert_eq!(original_width, 1);
                assert_eq!(original_height, 1);
            },
            other => panic!("expected image response, got {other:?}"),
        }

        let response = client
            .post(format!("{base_url}{TOOLS_SERVICE_READ_MEDIA_PATH}"))
            .bearer_auth("test-token")
            .json(&serde_json::json!({
                "filePath": broken_path,
            }))
            .send()
            .await
            .unwrap_or_else(|error| panic!("request failed: {error}"));

        assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
        let body = response
            .json::<ToolsServiceError>()
            .await
            .unwrap_or_else(|error| panic!("response decode failed: {error}"));
        assert!(body.error.contains("failed to decode or optimize image"));
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
