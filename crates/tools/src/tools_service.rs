use std::{path::PathBuf, process::Stdio, sync::Arc, time::Duration};

use {
    chelix_protocol::{
        CreateToolsServiceTerminalRequest, CreateToolsServiceTerminalResponse,
        ExecuteCommandRequest, ExecuteCommandResponse, ListDirectoryRequest, ListDirectoryResponse,
        ProcessRequest, ProcessResponse, ReadTerminalOutputRequest, ReadTerminalOutputResponse,
        RipgrepRequest, RipgrepResponse, TOOLS_SERVICE_BINARY_ENV,
        TOOLS_SERVICE_EXECUTE_COMMAND_PATH, TOOLS_SERVICE_HEALTH_PATH,
        TOOLS_SERVICE_LIST_DIRECTORY_PATH, TOOLS_SERVICE_PROCESS_PATH,
        TOOLS_SERVICE_PROTOCOL_VERSION, TOOLS_SERVICE_READ_TERMINAL_OUTPUT_PATH,
        TOOLS_SERVICE_RIPGREP_PATH, TOOLS_SERVICE_TERMINAL_WS_PATH, TOOLS_SERVICE_TERMINALS_PATH,
        ToolsServiceError, ToolsServiceHealth, ToolsServiceInstanceInfo, ToolsServiceReady,
        ToolsServiceTerminalAttachQuery, ToolsServiceTerminalKind, ToolsServiceTerminalsResponse,
    },
    serde::{Serialize, de::DeserializeOwned},
    tokio::{
        io::{AsyncBufReadExt, BufReader},
        process::{Child, Command},
        sync::Mutex,
    },
    tokio_tungstenite::tungstenite::{client::IntoClientRequest, http::HeaderValue},
    tracing::{info, warn},
};

use crate::{
    error::{Error, Result},
    sandbox::{ExecEnv, SandboxRouter, ToolsServiceEndpoint, ToolsServiceInstance},
};

const STARTUP_TIMEOUT: Duration = Duration::from_secs(10);
const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_secs(120);
const EXECUTE_COMMAND_RESPONSE_GRACE: Duration = Duration::from_secs(10);

#[derive(Debug)]
enum ToolsServiceCallError {
    Unavailable(Error),
    Tool(Error),
}

impl ToolsServiceCallError {
    fn into_error(self) -> Error {
        match self {
            Self::Unavailable(error) | Self::Tool(error) => error,
        }
    }
}

/// Concrete client for the versioned `chelix-tools-service` HTTP API.
///
/// This is the single transport boundary for all managed tool calls. It owns
/// endpoint selection, authentication, request recovery, and HTTP timeouts.
pub struct ManagedToolsService {
    router: Arc<SandboxRouter>,
    runtime: ManagedToolsRuntime,
    client: reqwest::Client,
}

enum ManagedToolsRuntime {
    Host(Box<HostToolsService>),
    Sandbox,
    #[cfg(test)]
    Fixed(ToolsServiceEndpoint),
}

impl ManagedToolsService {
    pub async fn start(router: Arc<SandboxRouter>) -> Result<Arc<Self>> {
        let client = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(2))
            .build()?;
        let runtime = if router.enabled() {
            info!(
                backend = %router.backend_id(),
                "managed tools service will run only inside sandbox containers"
            );
            ManagedToolsRuntime::Sandbox
        } else {
            ManagedToolsRuntime::Host(Box::new(HostToolsService::start(&client).await?))
        };
        Ok(Arc::new(Self {
            router,
            runtime,
            client,
        }))
    }

    async fn endpoint_for(&self, session_key: &str) -> Result<(ToolsServiceEndpoint, bool)> {
        match &self.runtime {
            ManagedToolsRuntime::Host(host) => Ok((host.endpoint(), false)),
            ManagedToolsRuntime::Sandbox => match self.router.resolve_env(session_key).await? {
                ExecEnv::Sandbox { backend, id } => {
                    Ok((backend.tools_service_endpoint(&id).await?, true))
                },
                ExecEnv::Host => Err(Error::message(
                    "sandbox tools service unexpectedly resolved to the host environment",
                )),
            },
            #[cfg(test)]
            ManagedToolsRuntime::Fixed(endpoint) => Ok((endpoint.clone(), false)),
        }
    }

    async fn existing_instances(&self) -> Result<Vec<ToolsServiceInstance>> {
        match &self.runtime {
            ManagedToolsRuntime::Host(host) => Ok(vec![ToolsServiceInstance {
                id: "host".into(),
                label: "Host".into(),
                endpoint: host.endpoint(),
            }]),
            ManagedToolsRuntime::Sandbox => self.router.tools_service_instances().await,
            #[cfg(test)]
            ManagedToolsRuntime::Fixed(endpoint) => Ok(vec![ToolsServiceInstance {
                id: "fixed".into(),
                label: "Fixed test service".into(),
                endpoint: endpoint.clone(),
            }]),
        }
    }

    async fn existing_instance(&self, instance_id: &str) -> Result<ToolsServiceInstance> {
        self.existing_instances()
            .await?
            .into_iter()
            .find(|instance| instance.id == instance_id)
            .ok_or_else(|| {
                Error::message(format!("tools service instance not found: {instance_id}"))
            })
    }

    async fn post_tool<Request, Response>(
        &self,
        endpoint: &ToolsServiceEndpoint,
        path: &str,
        request: &Request,
        timeout: Duration,
    ) -> std::result::Result<Response, ToolsServiceCallError>
    where
        Request: Serialize + Sync + ?Sized,
        Response: DeserializeOwned,
    {
        let response = self
            .client
            .post(format!("{}{path}", endpoint.base_url))
            .bearer_auth(&endpoint.token)
            .json(request)
            .timeout(timeout)
            .send()
            .await
            .map_err(Error::from)
            .map_err(ToolsServiceCallError::Unavailable)?;
        let status = response.status();
        if status.is_success() {
            return response
                .json::<Response>()
                .await
                .map_err(Error::from)
                .map_err(ToolsServiceCallError::Unavailable);
        }

        let error = response
            .json::<ToolsServiceError>()
            .await
            .map(|body| body.error)
            .unwrap_or_else(|decode_error| {
                format!("tools service returned {status}; error body decode failed: {decode_error}")
            });
        let error = Error::message(error);
        if status == reqwest::StatusCode::UNPROCESSABLE_ENTITY {
            Err(ToolsServiceCallError::Tool(error))
        } else {
            Err(ToolsServiceCallError::Unavailable(error))
        }
    }

    async fn get_tool<Response>(
        &self,
        endpoint: &ToolsServiceEndpoint,
        path: &str,
        timeout: Duration,
    ) -> Result<Response>
    where
        Response: DeserializeOwned,
    {
        let response = self
            .client
            .get(format!("{}{path}", endpoint.base_url))
            .bearer_auth(&endpoint.token)
            .timeout(timeout)
            .send()
            .await?;
        let status = response.status();
        if status.is_success() {
            return Ok(response.json::<Response>().await?);
        }
        let error = response
            .json::<ToolsServiceError>()
            .await
            .map(|body| body.error)
            .unwrap_or_else(|decode_error| {
                format!("tools service returned {status}; error body decode failed: {decode_error}")
            });
        Err(Error::message(error))
    }

    async fn call_tool<Request, Response>(
        &self,
        session_key: &str,
        path: &str,
        request: &Request,
        timeout: Duration,
    ) -> Result<Response>
    where
        Request: Serialize + Sync + ?Sized,
        Response: DeserializeOwned,
    {
        let (endpoint, sandboxed) = self.endpoint_for(session_key).await?;
        match self.post_tool(&endpoint, path, request, timeout).await {
            Ok(result) => Ok(result),
            Err(ToolsServiceCallError::Unavailable(error)) if sandboxed => {
                warn!(
                    session = session_key,
                    base_url = %endpoint.base_url,
                    %error,
                    "sandbox tools service call failed, re-preparing runtime and retrying once"
                );
                let (recovered_endpoint, recovered_sandboxed) =
                    self.endpoint_for(session_key).await?;
                if !recovered_sandboxed {
                    return Err(Error::message(format!(
                        "sandbox tools service recovery for session {session_key:?} resolved to the host environment"
                    )));
                }
                self.post_tool(&recovered_endpoint, path, request, timeout)
                    .await
                    .map_err(ToolsServiceCallError::into_error)
            },
            Err(error) => Err(error.into_error()),
        }
    }

    pub async fn list_directory(
        &self,
        session_key: &str,
        request: ListDirectoryRequest,
    ) -> Result<ListDirectoryResponse> {
        self.call_tool(
            session_key,
            TOOLS_SERVICE_LIST_DIRECTORY_PATH,
            &request,
            DEFAULT_REQUEST_TIMEOUT,
        )
        .await
    }

    pub async fn ripgrep(
        &self,
        session_key: &str,
        request: RipgrepRequest,
    ) -> Result<RipgrepResponse> {
        self.call_tool(
            session_key,
            TOOLS_SERVICE_RIPGREP_PATH,
            &request,
            DEFAULT_REQUEST_TIMEOUT,
        )
        .await
    }

    pub async fn execute_command(
        &self,
        session_key: &str,
        request: ExecuteCommandRequest,
    ) -> Result<ExecuteCommandResponse> {
        if request.session_key != session_key {
            return Err(Error::message("execute_command session key mismatch"));
        }
        let timeout = Duration::from_millis(request.timeout_millis)
            .saturating_add(EXECUTE_COMMAND_RESPONSE_GRACE);
        self.call_tool(
            session_key,
            TOOLS_SERVICE_EXECUTE_COMMAND_PATH,
            &request,
            timeout,
        )
        .await
    }

    pub async fn read_terminal_output(
        &self,
        session_key: &str,
        request: ReadTerminalOutputRequest,
    ) -> Result<ReadTerminalOutputResponse> {
        if request.session_key != session_key {
            return Err(Error::message("read_terminal_output session key mismatch"));
        }
        self.call_tool(
            session_key,
            TOOLS_SERVICE_READ_TERMINAL_OUTPUT_PATH,
            &request,
            DEFAULT_REQUEST_TIMEOUT,
        )
        .await
    }

    pub async fn process(
        &self,
        session_key: &str,
        request: ProcessRequest,
    ) -> Result<ProcessResponse> {
        if request.session_key != session_key {
            return Err(Error::message("process session key mismatch"));
        }
        self.call_tool(
            session_key,
            TOOLS_SERVICE_PROCESS_PATH,
            &request,
            DEFAULT_REQUEST_TIMEOUT,
        )
        .await
    }

    pub async fn terminal_instances(&self) -> Result<Vec<ToolsServiceInstanceInfo>> {
        let instances = self.existing_instances().await?;
        let mut result = Vec::with_capacity(instances.len());
        for instance in instances {
            let response = self
                .get_tool::<ToolsServiceTerminalsResponse>(
                    &instance.endpoint,
                    TOOLS_SERVICE_TERMINALS_PATH,
                    DEFAULT_REQUEST_TIMEOUT,
                )
                .await
                .map_err(|error| {
                    Error::message(format!(
                        "failed to read terminal inventory from tools service {}: {error}",
                        instance.id
                    ))
                })?;
            result.push(ToolsServiceInstanceInfo {
                id: instance.id,
                label: instance.label,
                terminals: response.terminals,
            });
        }
        Ok(result)
    }

    pub async fn create_terminal(
        &self,
        instance_id: &str,
        request: CreateToolsServiceTerminalRequest,
    ) -> Result<CreateToolsServiceTerminalResponse> {
        let instance = self.existing_instance(instance_id).await?;
        self.post_tool(
            &instance.endpoint,
            TOOLS_SERVICE_TERMINALS_PATH,
            &request,
            DEFAULT_REQUEST_TIMEOUT,
        )
        .await
        .map_err(ToolsServiceCallError::into_error)
    }

    pub async fn connect_terminal(
        &self,
        instance_id: &str,
        query: &ToolsServiceTerminalAttachQuery,
    ) -> Result<
        tokio_tungstenite::WebSocketStream<
            tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
        >,
    > {
        let instance = self.existing_instance(instance_id).await?;
        let mut url = url::Url::parse(&instance.endpoint.base_url)?;
        let websocket_scheme = match url.scheme() {
            "http" => "ws",
            "https" => "wss",
            scheme => {
                return Err(Error::message(format!(
                    "unsupported tools service URL scheme: {scheme}"
                )));
            },
        };
        url.set_scheme(websocket_scheme)
            .map_err(|_| Error::message("failed to set tools service WebSocket scheme"))?;
        url.set_path(TOOLS_SERVICE_TERMINAL_WS_PATH);
        url.query_pairs_mut()
            .append_pair("kind", match query.kind {
                ToolsServiceTerminalKind::Execute => "execute",
                ToolsServiceTerminalKind::Process => "process",
            })
            .append_pair("id", &query.id)
            .append_pair("sessionKey", &query.session_key);
        let mut request = url.as_str().into_client_request().map_err(|error| {
            Error::message(format!("invalid tools service WebSocket request: {error}"))
        })?;
        request.headers_mut().insert(
            tokio_tungstenite::tungstenite::http::header::AUTHORIZATION,
            HeaderValue::from_str(&format!("Bearer {}", instance.endpoint.token)).map_err(
                |error| Error::message(format!("invalid tools service auth header: {error}")),
            )?,
        );
        let (socket, _) = tokio_tungstenite::connect_async(request)
            .await
            .map_err(|error| {
                Error::message(format!("tools service terminal connection failed: {error}"))
            })?;
        Ok(socket)
    }

    #[cfg(test)]
    pub(crate) fn for_test(endpoint: ToolsServiceEndpoint) -> Result<Arc<Self>> {
        let client = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(1))
            .build()?;
        Ok(Arc::new(Self {
            router: Arc::new(SandboxRouter::disabled()),
            runtime: ManagedToolsRuntime::Fixed(endpoint),
            client,
        }))
    }
}

struct HostToolsService {
    endpoint: ToolsServiceEndpoint,
    _child: Mutex<Child>,
}

impl HostToolsService {
    async fn start(client: &reqwest::Client) -> Result<Self> {
        let binary = resolve_host_binary()?;
        let working_dir = chelix_config::home_dir()
            .ok_or_else(|| Error::message("cannot resolve host tools service working directory"))?;
        let mut child = Command::new(&binary)
            .arg("--shutdown-on-stdin-eof")
            .arg("--working-dir")
            .arg(&working_dir)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .kill_on_drop(true)
            .spawn()
            .map_err(|error| {
                Error::message(format!(
                    "failed to start tools service {}: {error}",
                    binary.display()
                ))
            })?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| Error::message("tools service stdout pipe missing"))?;
        let mut lines = BufReader::new(stdout).lines();
        let ready_line = tokio::time::timeout(STARTUP_TIMEOUT, lines.next_line())
            .await
            .map_err(|_| Error::message("timed out waiting for tools service startup message"))??
            .ok_or_else(|| Error::message("tools service exited before startup message"))?;
        let ready: ToolsServiceReady = serde_json::from_str(&ready_line).map_err(|error| {
            Error::message(format!(
                "invalid tools service startup message: {error}; line={ready_line:?}"
            ))
        })?;
        if ready.protocol_version != TOOLS_SERVICE_PROTOCOL_VERSION {
            return Err(Error::message(format!(
                "tools service protocol mismatch: expected {}, got {}",
                TOOLS_SERVICE_PROTOCOL_VERSION, ready.protocol_version
            )));
        }
        if ready.token.is_empty() {
            return Err(Error::message("tools service returned an empty auth token"));
        }
        let endpoint = ToolsServiceEndpoint {
            base_url: format!("http://127.0.0.1:{}", ready.port),
            token: ready.token,
        };
        verify_health(client, &endpoint).await?;
        info!(
            binary = %binary.display(),
            port = ready.port,
            "managed host tools service started"
        );

        Ok(Self {
            endpoint,
            _child: Mutex::new(child),
        })
    }

    fn endpoint(&self) -> ToolsServiceEndpoint {
        self.endpoint.clone()
    }
}

async fn verify_health(client: &reqwest::Client, endpoint: &ToolsServiceEndpoint) -> Result<()> {
    let response = client
        .get(format!(
            "{}{}",
            endpoint.base_url, TOOLS_SERVICE_HEALTH_PATH
        ))
        .bearer_auth(&endpoint.token)
        .send()
        .await?;
    if !response.status().is_success() {
        return Err(Error::message(format!(
            "tools service health check failed with status {}",
            response.status()
        )));
    }
    let health = response.json::<ToolsServiceHealth>().await?;
    if health.protocol_version != TOOLS_SERVICE_PROTOCOL_VERSION {
        return Err(Error::message(format!(
            "tools service health protocol mismatch: expected {}, got {}",
            TOOLS_SERVICE_PROTOCOL_VERSION, health.protocol_version
        )));
    }
    Ok(())
}

fn resolve_host_binary() -> Result<PathBuf> {
    if let Some(path) = non_empty_env_path(TOOLS_SERVICE_BINARY_ENV) {
        return require_file(path, TOOLS_SERVICE_BINARY_ENV);
    }

    if let Ok(current_exe) = std::env::current_exe()
        && let Some(directory) = current_exe.parent()
    {
        let sibling = directory.join("chelix-tools-service");
        if sibling.is_file() {
            return Ok(sibling);
        }
        if directory.file_name().is_some_and(|name| name == "deps")
            && let Some(profile_dir) = directory.parent()
        {
            let development_sibling = profile_dir.join("chelix-tools-service");
            if development_sibling.is_file() {
                return Ok(development_sibling);
            }
        }
    }

    which::which("chelix-tools-service").map_err(|_| {
        Error::message(format!(
            "chelix-tools-service binary not found; install it next to chelix or set {TOOLS_SERVICE_BINARY_ENV}"
        ))
    })
}

fn non_empty_env_path(name: &str) -> Option<PathBuf> {
    std::env::var_os(name)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

fn require_file(path: PathBuf, source: &str) -> Result<PathBuf> {
    if path.is_file() {
        Ok(path)
    } else {
        Err(Error::message(format!(
            "{source} points to a missing tools service binary: {}",
            path.display()
        )))
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    use {
        super::*,
        crate::{
            command::{CommandOptions, CommandOutput},
            sandbox::{Sandbox, SandboxConfig, SandboxId},
        },
        async_trait::async_trait,
    };

    struct RecoveringSandbox {
        endpoints: [ToolsServiceEndpoint; 2],
        ensure_ready_calls: AtomicUsize,
    }

    #[async_trait]
    impl Sandbox for RecoveringSandbox {
        fn backend_id(&self) -> crate::sandbox::SandboxBackendId {
            crate::sandbox::SandboxBackendId::Docker
        }

        fn provides_fs_isolation(&self) -> bool {
            true
        }

        async fn ensure_ready(&self, _id: &SandboxId) -> Result<()> {
            self.ensure_ready_calls.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }

        async fn tools_service_endpoint(&self, _id: &SandboxId) -> Result<ToolsServiceEndpoint> {
            let generation = self
                .ensure_ready_calls
                .load(Ordering::SeqCst)
                .saturating_sub(1)
                .min(1);
            Ok(self.endpoints[generation].clone())
        }

        async fn run_command(
            &self,
            _id: &SandboxId,
            _command: &str,
            _opts: &CommandOptions,
        ) -> Result<CommandOutput> {
            Err(Error::message("command execution is not used by this test"))
        }

        async fn cleanup(&self, _id: &SandboxId) -> Result<()> {
            Ok(())
        }
    }

    #[cfg(unix)]
    async fn test_managed_service(
        endpoints: [ToolsServiceEndpoint; 2],
    ) -> (ManagedToolsService, Arc<RecoveringSandbox>) {
        let backend = Arc::new(RecoveringSandbox {
            endpoints,
            ensure_ready_calls: AtomicUsize::new(0),
        });
        let routed_backend: Arc<dyn Sandbox> = backend.clone();
        let router = Arc::new(SandboxRouter::with_backend(
            SandboxConfig::default(),
            routed_backend,
        ));
        let client = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(1))
            .timeout(Duration::from_secs(5))
            .build()
            .unwrap_or_else(|error| panic!("test HTTP client failed: {error}"));
        (
            ManagedToolsService {
                router,
                runtime: ManagedToolsRuntime::Sandbox,
                client,
            },
            backend,
        )
    }

    #[tokio::test]
    async fn sandbox_mode_contains_no_host_service() {
        let backend = Arc::new(RecoveringSandbox {
            endpoints: [
                ToolsServiceEndpoint {
                    base_url: "http://127.0.0.1:1".into(),
                    token: "first".into(),
                },
                ToolsServiceEndpoint {
                    base_url: "http://127.0.0.1:2".into(),
                    token: "second".into(),
                },
            ],
            ensure_ready_calls: AtomicUsize::new(0),
        });
        let routed_backend: Arc<dyn Sandbox> = backend;
        let router = Arc::new(SandboxRouter::with_backend(
            SandboxConfig::default(),
            routed_backend,
        ));

        let service = ManagedToolsService::start(router)
            .await
            .unwrap_or_else(|error| panic!("sandbox-only service startup failed: {error}"));

        assert!(matches!(service.runtime, ManagedToolsRuntime::Sandbox));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn sandbox_call_recovers_with_fresh_endpoint_and_token() {
        let mut stale_server = mockito::Server::new_async().await;
        let stale_call = stale_server
            .mock("POST", TOOLS_SERVICE_RIPGREP_PATH)
            .match_header("authorization", "Bearer stale-token")
            .with_status(401)
            .with_header("content-type", "application/json")
            .with_body("{\"error\":\"unauthorized\"}")
            .expect(1)
            .create_async()
            .await;
        let mut recovered_server = mockito::Server::new_async().await;
        let recovered_call = recovered_server
            .mock("POST", TOOLS_SERVICE_RIPGREP_PATH)
            .match_header("authorization", "Bearer fresh-token")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body("{\"result\":{\"found\":true}}")
            .expect(1)
            .create_async()
            .await;
        let (service, backend) = test_managed_service([
            ToolsServiceEndpoint {
                base_url: stale_server.url(),
                token: "stale-token".into(),
            },
            ToolsServiceEndpoint {
                base_url: recovered_server.url(),
                token: "fresh-token".into(),
            },
        ])
        .await;

        let result = service
            .ripgrep("session:recovery", RipgrepRequest {
                params: serde_json::json!({ "pattern": "needle" }),
            })
            .await
            .unwrap_or_else(|error| panic!("sandbox recovery failed: {error}"));

        assert_eq!(result.result["found"], true);
        assert_eq!(backend.ensure_ready_calls.load(Ordering::SeqCst), 2);
        stale_call.assert_async().await;
        recovered_call.assert_async().await;
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn sandbox_tool_error_is_not_retried() {
        let mut server = mockito::Server::new_async().await;
        let tool_error = server
            .mock("POST", TOOLS_SERVICE_RIPGREP_PATH)
            .match_header("authorization", "Bearer tool-token")
            .with_status(422)
            .with_header("content-type", "application/json")
            .with_body("{\"error\":\"invalid pattern\"}")
            .expect(1)
            .create_async()
            .await;
        let endpoint = ToolsServiceEndpoint {
            base_url: server.url(),
            token: "tool-token".into(),
        };
        let (service, backend) = test_managed_service([endpoint.clone(), endpoint]).await;

        let error = match service
            .ripgrep("session:tool-error", RipgrepRequest {
                params: serde_json::json!({ "pattern": "[" }),
            })
            .await
        {
            Ok(_) => panic!("expected tool error"),
            Err(error) => error,
        };

        assert!(error.to_string().contains("invalid pattern"));
        assert_eq!(backend.ensure_ready_calls.load(Ordering::SeqCst), 1);
        tool_error.assert_async().await;
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn list_directory_calls_its_service_endpoint() {
        let mut server = mockito::Server::new_async().await;
        let call = server
            .mock("POST", TOOLS_SERVICE_LIST_DIRECTORY_PATH)
            .match_header("authorization", "Bearer list-token")
            .match_body(mockito::Matcher::Json(serde_json::json!({
                "path": "/workspace"
            })))
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body("{\"result\":\"src/\\nCargo.toml (1 line)\"}")
            .expect(1)
            .create_async()
            .await;
        let endpoint = ToolsServiceEndpoint {
            base_url: server.url(),
            token: "list-token".into(),
        };
        let (service, backend) = test_managed_service([endpoint.clone(), endpoint]).await;

        let result = service
            .list_directory("session:list", ListDirectoryRequest {
                path: "/workspace".into(),
            })
            .await
            .unwrap_or_else(|error| panic!("list directory call failed: {error}"));

        assert_eq!(result.result, "src/\nCargo.toml (1 line)");
        assert_eq!(backend.ensure_ready_calls.load(Ordering::SeqCst), 1);
        call.assert_async().await;
    }

    #[tokio::test]
    async fn terminal_inventory_preserves_exact_service_metadata() {
        let mut server = mockito::Server::new_async().await;
        let call = server
            .mock("GET", TOOLS_SERVICE_TERMINALS_PATH)
            .match_header("authorization", "Bearer terminal-token")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                r#"{"terminals":[{"kind":"execute","id":"terminal-42","sessionKey":"agent:42","sessionId":"$4","sessionName":"chelix-agent-42","windowId":"@8","windowName":"bash","paneId":"%11","running":true}]}"#,
            )
            .expect(1)
            .create_async()
            .await;
        let service = ManagedToolsService::for_test(ToolsServiceEndpoint {
            base_url: server.url(),
            token: "terminal-token".into(),
        })
        .unwrap_or_else(|error| panic!("test service failed: {error}"));

        let instances = service
            .terminal_instances()
            .await
            .unwrap_or_else(|error| panic!("terminal inventory failed: {error}"));

        assert_eq!(instances.len(), 1);
        assert_eq!(instances[0].id, "fixed");
        assert_eq!(instances[0].terminals[0].id, "terminal-42");
        assert_eq!(instances[0].terminals[0].session_id, "$4");
        assert_eq!(instances[0].terminals[0].window_id, "@8");
        assert_eq!(instances[0].terminals[0].pane_id, "%11");
        call.assert_async().await;
    }

    #[tokio::test]
    async fn terminal_creation_uses_selected_instance_and_explicit_session_key() {
        let mut server = mockito::Server::new_async().await;
        let call = server
            .mock("POST", TOOLS_SERVICE_TERMINALS_PATH)
            .match_header("authorization", "Bearer terminal-token")
            .match_body(mockito::Matcher::Json(serde_json::json!({
                "sessionKey": "agent:explicit"
            })))
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                r#"{"terminal":{"kind":"execute","id":"terminal-created","sessionKey":"agent:explicit","sessionId":"$9","sessionName":"chelix-agent-explicit","windowId":"@12","windowName":"shell","paneId":"%15","running":false}}"#,
            )
            .expect(1)
            .create_async()
            .await;
        let service = ManagedToolsService::for_test(ToolsServiceEndpoint {
            base_url: server.url(),
            token: "terminal-token".into(),
        })
        .unwrap_or_else(|error| panic!("test service failed: {error}"));

        let created = service
            .create_terminal("fixed", CreateToolsServiceTerminalRequest {
                session_key: "agent:explicit".into(),
            })
            .await
            .unwrap_or_else(|error| panic!("terminal creation failed: {error}"));

        assert_eq!(created.terminal.id, "terminal-created");
        assert_eq!(created.terminal.session_key, "agent:explicit");
        assert_eq!(created.terminal.session_id, "$9");
        assert_eq!(created.terminal.window_id, "@12");
        assert_eq!(created.terminal.pane_id, "%15");
        call.assert_async().await;
    }

    #[tokio::test]
    async fn terminal_creation_rejects_unknown_instance_without_network_fallback() {
        let service = ManagedToolsService::for_test(ToolsServiceEndpoint {
            base_url: "http://127.0.0.1:1".into(),
            token: "unused".into(),
        })
        .unwrap_or_else(|error| panic!("test service failed: {error}"));

        let error = service
            .create_terminal("missing", CreateToolsServiceTerminalRequest {
                session_key: "agent:explicit".into(),
            })
            .await
            .expect_err("unknown instance must fail");

        assert!(
            error
                .to_string()
                .contains("tools service instance not found: missing")
        );
    }
}
