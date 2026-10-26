//! API routes for configuration editing.
//!
//! Provides endpoints to get, validate, and save the full chelix config as TOML.
//!
//! SECURITY: These endpoints expose sensitive configuration including API keys.
//! They are protected by auth middleware, but also have explicit checks to ensure
//! they never work without authentication on non-localhost connections.

use {
    axum::{Json, extract::State, http::StatusCode, response::IntoResponse},
    chelix_gateway::auth::AuthIdentity,
    std::sync::Arc,
};

const CONFIG_AUTH_REQUIRED: &str = "CONFIG_AUTH_REQUIRED";
const CONFIG_READ_FAILED: &str = "CONFIG_READ_FAILED";
const CONFIG_LOAD_FAILED: &str = "CONFIG_LOAD_FAILED";
const CONFIG_TOML_REQUIRED: &str = "CONFIG_TOML_REQUIRED";
const CONFIG_INVALID_TOML: &str = "CONFIG_INVALID_TOML";
const CONFIG_INVALID_CONFIG: &str = "CONFIG_INVALID_CONFIG";
const CONFIG_SAVE_FAILED: &str = "CONFIG_SAVE_FAILED";
const CONFIG_RESTART_INVALID: &str = "CONFIG_RESTART_INVALID";
const CONFIG_RESTART_READ_FAILED: &str = "CONFIG_RESTART_READ_FAILED";

fn config_error(code: &str, error: impl Into<String>) -> serde_json::Value {
    serde_json::json!({
        "code": code,
        "error": error.into(),
    })
}

fn diagnostic_text(diagnostic: &chelix_config::Diagnostic) -> String {
    if diagnostic.path.is_empty() {
        diagnostic.message.clone()
    } else {
        format!("{}: {}", diagnostic.path, diagnostic.message)
    }
}

fn config_diagnose_response(result: chelix_config::ValidationResult) -> axum::response::Response {
    let mut syntax_errors = Vec::new();
    let mut config_errors = Vec::new();
    let mut warnings = Vec::new();
    for diagnostic in result.diagnostics {
        let text = diagnostic_text(&diagnostic);
        match (diagnostic.severity, diagnostic.category) {
            (
                chelix_config::Severity::Error,
                chelix_config::DiagnosticCategory::Syntax
                | chelix_config::DiagnosticCategory::TypeError,
            ) => syntax_errors.push(text),
            (chelix_config::Severity::Error, _) => config_errors.push(text),
            (chelix_config::Severity::Warning, _) => warnings.push(text),
            (chelix_config::Severity::Info, _) => {},
        }
    }

    if !syntax_errors.is_empty() && config_errors.is_empty() {
        return Json(serde_json::json!({
            "code": CONFIG_INVALID_TOML,
            "valid": false,
            "error": syntax_errors.join("; "),
        }))
        .into_response();
    }
    if !config_errors.is_empty() || !syntax_errors.is_empty() {
        let mut diagnostics = syntax_errors;
        diagnostics.extend(config_errors);
        return Json(serde_json::json!({
            "code": CONFIG_INVALID_CONFIG,
            "valid": false,
            "error": diagnostics.join("; "),
            "diagnostics": diagnostics,
        }))
        .into_response();
    }
    Json(serde_json::json!({
        "valid": true,
        "warnings": warnings,
    }))
    .into_response()
}

fn config_save_error(error: chelix_config_service::Error) -> axum::response::Response {
    if error.is_validation() {
        let code = match &error {
            chelix_config_service::Error::Config(chelix_config::Error::TomlDe(_))
            | chelix_config_service::Error::Config(chelix_config::Error::TomlEdit(_)) => {
                CONFIG_INVALID_TOML
            },
            _ => CONFIG_INVALID_CONFIG,
        };
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({
                "code": code,
                "error": error.to_string(),
                "valid": false,
            })),
        )
            .into_response();
    }
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(config_error(
            CONFIG_SAVE_FAILED,
            format!("failed to save config: {error}"),
        )),
    )
        .into_response()
}

fn config_read_error(error: chelix_config_service::Error) -> axum::response::Response {
    let (status, code, message) = match &error {
        chelix_config_service::Error::Read { source, .. } => (
            StatusCode::INTERNAL_SERVER_ERROR,
            CONFIG_READ_FAILED,
            format!("failed to read config: {source}"),
        ),
        _ => (
            StatusCode::INTERNAL_SERVER_ERROR,
            CONFIG_LOAD_FAILED,
            error.to_string(),
        ),
    };
    (status, Json(config_error(code, message))).into_response()
}

/// Check if the request should be allowed for config operations.
///
/// Config endpoints are ONLY allowed when:
/// - Running on localhost (loopback interface only), OR
/// - User is authenticated via session or API key
///
/// When `require_admin` is true, API keys must have the `operator.admin`
/// scope (password/passkey/loopback always have full access).
///
/// This is a defense-in-depth check on top of the auth middleware.
async fn require_config_access(
    state: &crate::server::AppState,
    identity: Option<&AuthIdentity>,
    require_admin: bool,
) -> Result<(), impl IntoResponse> {
    if require_admin
        && let Some(id) = identity
        && !id.has_scope("operator.admin")
    {
        return Err((
            StatusCode::FORBIDDEN,
            Json(config_error(
                "INSUFFICIENT_SCOPE",
                "operator.admin scope required",
            )),
        ));
    }
    let gw = &state.gateway;

    // On localhost with no password set, allow access (backward compat for initial setup)
    if gw.localhost_only {
        if let Some(ref cred_store) = gw.credential_store {
            // If auth is explicitly disabled, allow
            if cred_store.is_auth_disabled() {
                return Ok(());
            }
            // If no password set yet, allow (initial setup)
            if !cred_store.is_setup_complete() {
                return Ok(());
            }
        } else {
            // No credential store on localhost = allow
            return Ok(());
        }
    }

    // For non-localhost, we MUST have valid auth. The middleware should have
    // already blocked unauthenticated requests, but if somehow we got here
    // without localhost and without going through auth, block it.
    if !gw.localhost_only {
        if let Some(ref cred_store) = gw.credential_store {
            // Auth is configured but we're not on localhost - the middleware
            // should have verified auth. If auth is disabled, that's the user's
            // explicit choice.
            if cred_store.is_auth_disabled() {
                return Ok(());
            }
            // Setup complete means auth is enforced - middleware handles this
            if cred_store.is_setup_complete() {
                // Trust that middleware verified auth
                return Ok(());
            }
        } else {
            // Non-localhost without credential store is a misconfiguration
            // but we should not expose config in this case
            return Err((
                StatusCode::FORBIDDEN,
                Json(config_error(
                    CONFIG_AUTH_REQUIRED,
                    "Config access requires authentication on non-localhost connections",
                )),
            ));
        }
    }

    Ok(())
}

/// Get the current configuration as TOML.
pub async fn config_get(State(state): State<crate::server::AppState>) -> impl IntoResponse {
    // Extra security check for config access
    if let Err(resp) = require_config_access(&state, None, false).await {
        return resp.into_response();
    }

    let config = state.gateway.services.configuration();
    let document = match config.read_document().await {
        Ok(document) => document,
        Err(error) => return config_read_error(error),
    };

    Json(serde_json::json!({
        "toml": document.toml,
        "valid": true,
        "path": document.path.to_string_lossy(),
    }))
    .into_response()
}

/// Validate configuration TOML without saving.
pub async fn config_validate(
    State(state): State<crate::server::AppState>,
    Json(body): Json<serde_json::Value>,
) -> impl IntoResponse {
    // Extra security check for config access
    if let Err(resp) = require_config_access(&state, None, false).await {
        return resp.into_response();
    }

    let Some(toml_str) = body.get("toml").and_then(|v| v.as_str()) else {
        return (
            StatusCode::BAD_REQUEST,
            Json(config_error(CONFIG_TOML_REQUIRED, "missing 'toml' field")),
        )
            .into_response();
    };

    let config = state.gateway.services.configuration();
    let diagnosed = config.diagnose(toml_str).await;
    if diagnosed.has_errors() {
        return config_diagnose_response(diagnosed);
    }
    let parsed = match chelix_config::parse_raw_config(toml_str, config.location()) {
        Ok(parsed) => parsed,
        Err(error) => {
            let code = match &error {
                chelix_config::Error::TomlDe(_) | chelix_config::Error::TomlEdit(_) => {
                    CONFIG_INVALID_TOML
                },
                _ => CONFIG_INVALID_CONFIG,
            };
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({
                    "code": code,
                    "valid": false,
                    "error": error.to_string(),
                })),
            )
                .into_response();
        },
    };
    Json(serde_json::json!({
        "valid": true,
        "warnings": chelix_gateway::config_validation::configuration_editor_warnings(&parsed),
    }))
    .into_response()
}

/// Get the default configuration template with all options documented.
/// Preserves the current port from the existing config.
pub async fn config_template(State(state): State<crate::server::AppState>) -> impl IntoResponse {
    // Extra security check for config access
    if let Err(resp) = require_config_access(&state, None, false).await {
        return resp.into_response();
    }

    let config = state.gateway.services.configuration();
    let port = match config.read(|cfg| cfg.server.port).await {
        Ok(port) => port,
        Err(error) => return config_read_error(error),
    };
    let template = chelix_config::template::default_config_template(port);

    Json(serde_json::json!({
        "toml": template,
    }))
    .into_response()
}

/// Save configuration from TOML.
pub async fn config_save(
    identity: Option<axum::Extension<AuthIdentity>>,
    State(state): State<crate::server::AppState>,
    Json(body): Json<serde_json::Value>,
) -> impl IntoResponse {
    let id_ref = identity.as_ref().map(|axum::Extension(id)| id);
    if let Err(resp) = require_config_access(&state, id_ref, true).await {
        return resp.into_response();
    }

    let Some(toml_str) = body.get("toml").and_then(|v| v.as_str()) else {
        return (
            StatusCode::BAD_REQUEST,
            Json(config_error(CONFIG_TOML_REQUIRED, "missing 'toml' field")),
        )
            .into_response();
    };

    let config = state.gateway.services.configuration();
    if let Err(error) = config
        .replace_document_checked(
            toml_str.to_owned(),
            Some(Arc::new(
                chelix_providers::config_model_resolver::ModelConfigValidator,
            )),
        )
        .await
    {
        if error.is_validation() {
            let diagnosed = config.diagnose(toml_str).await;
            if diagnosed.has_errors() {
                let mut response = config_diagnose_response(diagnosed);
                *response.status_mut() = StatusCode::BAD_REQUEST;
                return response;
            }
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({
                    "code": CONFIG_INVALID_CONFIG,
                    "error": error.to_string(),
                    "valid": false,
                })),
            )
                .into_response();
        }
        return config_save_error(error);
    }

    let path = config.location().to_path_buf();
    tracing::info!(path = %path.display(), "saved config (raw)");
    Json(serde_json::json!({
        "ok": true,
        "path": path.to_string_lossy(),
        "restart_required": true,
    }))
    .into_response()
}

/// Restart the chelix process.
///
/// This re-runs the current binary with the same arguments. On Unix, it uses the exec
/// syscall to replace the current process. On other platforms, it spawns a new process.
///
/// Before restarting, the saved config is loaded from disk and validated. If the config
/// is invalid the restart is refused so the server doesn't crash on startup.
pub async fn restart(
    identity: Option<axum::Extension<AuthIdentity>>,
    State(state): State<crate::server::AppState>,
) -> impl IntoResponse {
    let id_ref = identity.as_ref().map(|axum::Extension(id)| id);
    if let Err(resp) = require_config_access(&state, id_ref, true).await {
        return resp.into_response();
    }

    // The writer validates the persisted file before restart.
    let config = state.gateway.services.configuration();
    match config
        .validate_persisted(Some(Arc::new(
            chelix_providers::config_model_resolver::ModelConfigValidator,
        )))
        .await
    {
        Ok(
            chelix_config_service::PersistedConfigStatus::Valid
            | chelix_config_service::PersistedConfigStatus::Missing,
        ) => {},
        Err(error) => return config_restart_error(error),
    }

    tracing::info!("restart requested via API");

    // Spawn a task to restart after a short delay, allowing the response to be sent first.
    tokio::spawn(async {
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        tracing::info!("restarting now");

        let exe = match std::env::current_exe() {
            Ok(path) => path,
            Err(e) => {
                tracing::error!("failed to get current executable path: {e}");
                std::process::exit(1);
            },
        };

        let args: Vec<String> = std::env::args().skip(1).collect();
        tracing::info!(exe = %exe.display(), args = ?args, "re-executing");

        // Use exec on Unix to replace the current process in-place
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            // This replaces the current process and never returns on success
            let err = std::process::Command::new(&exe).args(&args).exec();
            tracing::error!("failed to exec: {err}");
            std::process::exit(1);
        }

        // On non-Unix, spawn a new process and exit the current one
        #[cfg(not(unix))]
        {
            match std::process::Command::new(&exe).args(&args).spawn() {
                Ok(_) => {
                    tracing::info!("spawned new process, exiting current");
                    std::process::exit(0);
                },
                Err(e) => {
                    tracing::error!("failed to spawn new process: {e}");
                    std::process::exit(1);
                },
            }
        }
    });

    Json(serde_json::json!({
        "ok": true,
        "message": "Chelix is restarting..."
    }))
    .into_response()
}

/// Perform an in-place update to the latest (or specified) version.
///
/// Requires `operator.admin` scope. On success with binary replacement,
/// schedules a restart after 500 ms so the response can be sent first.
pub async fn update(
    identity: Option<axum::Extension<AuthIdentity>>,
    State(state): State<crate::server::AppState>,
    body: Option<Json<serde_json::Value>>,
) -> impl IntoResponse {
    let id_ref = identity.as_ref().map(|axum::Extension(id)| id);
    if let Err(resp) = require_config_access(&state, id_ref, true).await {
        return resp.into_response();
    }

    let requested_version = body
        .as_ref()
        .and_then(|Json(v)| v.get("version"))
        .and_then(|v| v.as_str());

    let releases_url = chelix_gateway::update_check::resolve_releases_url(
        state.gateway.config.server.update_releases_url.as_deref(),
    );

    let client = match reqwest::Client::builder()
        .user_agent(format!("chelix-gateway/{}", state.gateway.version))
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(config_error("UPDATE_CLIENT_ERROR", format!("{e}"))),
            )
                .into_response();
        },
    };

    match chelix_gateway::updater::perform_update(&client, &releases_url, requested_version).await {
        Ok(ref outcome @ chelix_gateway::updater::UpdateOutcome::Updated { .. })
        | Ok(ref outcome @ chelix_gateway::updater::UpdateOutcome::DockerUpdated { .. }) => {
            let restarting = true;
            tokio::spawn(async {
                tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                tracing::info!("restarting after update via API");
                chelix_gateway::updater::restart_process();
            });
            let mut json = serde_json::to_value(outcome).unwrap_or_default();
            if let serde_json::Value::Object(ref mut map) = json {
                map.insert("restarting".into(), serde_json::Value::Bool(restarting));
            }
            Json(json).into_response()
        },
        Ok(outcome) => Json(serde_json::to_value(&outcome).unwrap_or_default()).into_response(),
        Err(e) => (
            StatusCode::BAD_REQUEST,
            Json(config_error("UPDATE_FAILED", format!("{e}"))),
        )
            .into_response(),
    }
}

fn config_restart_invalid(error: impl std::fmt::Display) -> axum::response::Response {
    tracing::warn!(error = %error, "restart refused: saved config is invalid");
    (
        StatusCode::BAD_REQUEST,
        Json(serde_json::json!({
            "code": CONFIG_RESTART_INVALID,
            "error": format!("Config is invalid, refusing to restart: {error}"),
            "valid": false,
        })),
    )
        .into_response()
}

fn config_restart_error(error: chelix_config_service::Error) -> axum::response::Response {
    match error {
        chelix_config_service::Error::Read { path, source } => {
            tracing::warn!(
                path = %path.display(),
                error = %source,
                "restart refused: cannot read config file"
            );
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(config_error(
                    CONFIG_RESTART_READ_FAILED,
                    format!("Cannot read config file: {source}"),
                )),
            )
                .into_response()
        },
        chelix_config_service::Error::Config(error) => config_restart_invalid(error),
        chelix_config_service::Error::Validation(error) => config_restart_invalid(error),
        error if error.is_validation() => config_restart_invalid(error),
        error => {
            tracing::warn!(error = %error, "restart refused: config check failed");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(config_error(CONFIG_LOAD_FAILED, error.to_string())),
            )
                .into_response()
        },
    }
}

#[cfg(test)]
mod tests {
    use {
        super::*,
        axum::{body::to_bytes, response::Response},
        chelix_gateway::{
            auth, methods::MethodRegistry, services::GatewayServices, state::GatewayState,
        },
        std::sync::Arc,
    };

    async fn response_json(response: Response) -> serde_json::Value {
        let body = match to_bytes(response.into_body(), usize::MAX).await {
            Ok(bytes) => bytes,
            Err(err) => panic!("failed to read body bytes: {err}"),
        };
        match serde_json::from_slice::<serde_json::Value>(&body) {
            Ok(value) => value,
            Err(err) => panic!("failed to parse json body: {err}"),
        }
    }

    const SERVICE_CONFIG_TOML: &str = "\
[server]\n\
port = 18789\n\
\n\
[tools.execute_command]\n\
terminal_size = \"115x58\"\n\
\n\
[sandbox]\n\
archived_session_retention_days = 3\n\
";

    use crate::test_config::ConfigDirGuard;

    async fn localhost_app_state() -> (crate::server::AppState, ConfigDirGuard) {
        let guard = ConfigDirGuard::install(SERVICE_CONFIG_TOML).await;
        let service = match chelix_config_service::ConfigService::open(
            chelix_config_service::ConfigValidation::default(),
            chelix_config_service::ConfigInitialization::default(),
        )
        .await
        {
            Ok(service) => service,
            Err(error) => panic!("config service should open: {error}"),
        };
        let mut config = chelix_config::ChelixConfig::default();
        config.sandbox.mode = chelix_config::schema::SandboxMode::Off;
        let gateway = GatewayState::with_options(
            auth::resolve_auth(None, None),
            GatewayServices::noop(service),
            config,
            Arc::new(chelix_tools::sandbox::SandboxRouter::disabled()),
            None,
            true,
            false,
            false,
            None,
            None,
            Arc::new(chelix_code_index::CodeIndex::config_only(
                chelix_code_index::CodeIndexConfig::default(),
            )),
            18789,
            false,
            None,
            None,
            #[cfg(feature = "metrics")]
            None,
            #[cfg(feature = "metrics")]
            None,
            #[cfg(feature = "vault")]
            None,
        );
        (
            crate::server::AppState {
                gateway,
                methods: Arc::new(MethodRegistry::new()),
                request_throttle: Arc::new(crate::request_throttle::RequestThrottle::new()),
                webauthn_registry: None,
                #[cfg(feature = "push-notifications")]
                push_service: None,
            },
            guard,
        )
    }

    #[tokio::test]
    async fn config_save_rejects_invalid_documents_and_publishes_valid_document() {
        let (state, guard) = localhost_app_state().await;
        let service = state.gateway.services.configuration().clone();
        for (raw, code) in [
            ("[server\nport = 12345\n", CONFIG_INVALID_TOML),
            ("[server]\nremoved_option = true\n", CONFIG_INVALID_CONFIG),
        ] {
            let response = config_save(
                None,
                State(state.clone()),
                Json(serde_json::json!({"toml": raw})),
            )
            .await
            .into_response();
            assert_eq!(response.status(), StatusCode::BAD_REQUEST);
            assert_eq!(response_json(response).await["code"], code);
            let persisted = std::fs::read_to_string(guard.path().join("chelix.toml"))
                .unwrap_or_else(|error| panic!("read rejected document: {error}"));
            assert_eq!(persisted, SERVICE_CONFIG_TOML);
            let snapshot = service
                .read_document()
                .await
                .unwrap_or_else(|error| panic!("read unchanged snapshot: {error}"));
            assert_eq!(snapshot.toml, SERVICE_CONFIG_TOML);
        }
        let raw = "[server]\nport = 12345\n";
        let response = config_save(None, State(state), Json(serde_json::json!({"toml": raw})))
            .await
            .into_response();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response_json(response).await["restart_required"], true);
        assert_eq!(
            std::fs::read_to_string(guard.path().join("chelix.toml"))
                .unwrap_or_else(|error| panic!("read saved document: {error}")),
            raw
        );
        assert_eq!(
            service
                .read_document()
                .await
                .unwrap_or_else(|error| panic!("read published document: {error}"))
                .toml,
            raw
        );
        assert_eq!(
            service
                .read(|config| config.server.port)
                .await
                .unwrap_or_else(|error| panic!("read published config: {error}")),
            12345
        );
    }

    #[tokio::test]
    async fn config_service_errors_keep_endpoint_status_and_code() {
        use chelix_config_service::Error;
        let cases = [
            (
                config_save_error(Error::Write {
                    path: "chelix.toml".into(),
                    source: std::io::Error::other("write failed"),
                }),
                StatusCode::INTERNAL_SERVER_ERROR,
                CONFIG_SAVE_FAILED,
            ),
            (
                config_restart_error(Error::Read {
                    path: "chelix.toml".into(),
                    source: std::io::Error::other("read failed"),
                }),
                StatusCode::INTERNAL_SERVER_ERROR,
                CONFIG_RESTART_READ_FAILED,
            ),
            (
                config_restart_error(Error::Validation("owner rejected candidate".into())),
                StatusCode::BAD_REQUEST,
                CONFIG_RESTART_INVALID,
            ),
            (
                config_read_error(Error::QueueClosed),
                StatusCode::INTERNAL_SERVER_ERROR,
                CONFIG_LOAD_FAILED,
            ),
        ];
        for (response, status, code) in cases {
            assert_eq!(response.status(), status);
            assert_eq!(response_json(response).await["code"], code);
        }
    }

    #[tokio::test]
    async fn config_validate_rejects_unknown_memory_field() {
        let (state, _guard) = localhost_app_state().await;
        let body = serde_json::json!({
            "toml": "[memory]\nunexpected_field = \"x\"\n",
        });

        let response = config_validate(State(state), Json(body))
            .await
            .into_response();
        let json = response_json(response).await;

        assert_eq!(json["valid"], false);
        assert_eq!(json["code"], CONFIG_INVALID_CONFIG);
        let diagnostics = match json["diagnostics"].as_array() {
            Some(diagnostics) => diagnostics,
            None => panic!("expected diagnostics array: {json}"),
        };
        assert!(
            diagnostics.iter().any(|d| d
                .as_str()
                .is_some_and(|s| s.contains("memory.unexpected_field"))),
            "diagnostics should mention the unknown memory field: {diagnostics:?}"
        );
    }
}
