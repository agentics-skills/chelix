use std::{path::PathBuf, process::Stdio, sync::Arc, time::Duration};

use {
    async_trait::async_trait,
    chelix_protocol::{
        ListDirectoryRequest, ListDirectoryResponse, RipgrepRequest, RipgrepResponse,
        TOOLS_SERVICE_BINARY_ENV, TOOLS_SERVICE_HEALTH_PATH, TOOLS_SERVICE_LIST_DIRECTORY_PATH,
        TOOLS_SERVICE_PROTOCOL_VERSION, TOOLS_SERVICE_RIPGREP_PATH, ToolsServiceError,
        ToolsServiceHealth, ToolsServiceReady,
    },
    serde::{Serialize, de::DeserializeOwned},
    serde_json::Value,
    tokio::{
        io::{AsyncBufReadExt, BufReader},
        process::{Child, Command},
        sync::Mutex,
    },
    tracing::{info, warn},
};

use crate::{
    error::{Error, Result},
    sandbox::{ExecEnv, SandboxRouter, ToolsServiceEndpoint},
};

const STARTUP_TIMEOUT: Duration = Duration::from_secs(10);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(120);

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

#[async_trait]
pub trait ToolsService: Send + Sync {
    async fn list_directory(&self, session_key: &str, path: String) -> Result<String>;
    async fn ripgrep(&self, session_key: &str, params: Value) -> Result<Value>;
}

pub struct ManagedToolsService {
    router: Arc<SandboxRouter>,
    runtime: ManagedToolsRuntime,
    client: reqwest::Client,
}

enum ManagedToolsRuntime {
    Host(Box<HostToolsService>),
    Sandbox,
}

impl ManagedToolsService {
    pub async fn start(router: Arc<SandboxRouter>) -> Result<Arc<Self>> {
        let client = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(2))
            .timeout(REQUEST_TIMEOUT)
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
        }
    }

    async fn post_tool<Request, Response>(
        &self,
        endpoint: &ToolsServiceEndpoint,
        path: &str,
        request: &Request,
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

    async fn call_tool<Request, Response>(
        &self,
        session_key: &str,
        path: &str,
        request: &Request,
    ) -> Result<Response>
    where
        Request: Serialize + Sync + ?Sized,
        Response: DeserializeOwned,
    {
        let (endpoint, sandboxed) = self.endpoint_for(session_key).await?;
        match self.post_tool(&endpoint, path, request).await {
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
                self.post_tool(&recovered_endpoint, path, request)
                    .await
                    .map_err(ToolsServiceCallError::into_error)
            },
            Err(error) => Err(error.into_error()),
        }
    }
}

#[async_trait]
impl ToolsService for ManagedToolsService {
    async fn list_directory(&self, session_key: &str, path: String) -> Result<String> {
        let response: ListDirectoryResponse = self
            .call_tool(
                session_key,
                TOOLS_SERVICE_LIST_DIRECTORY_PATH,
                &ListDirectoryRequest { path },
            )
            .await?;
        Ok(response.result)
    }

    async fn ripgrep(&self, session_key: &str, params: Value) -> Result<Value> {
        let response: RipgrepResponse = self
            .call_tool(session_key, TOOLS_SERVICE_RIPGREP_PATH, &RipgrepRequest {
                params,
            })
            .await?;
        Ok(response.result)
    }
}

struct HostToolsService {
    endpoint: ToolsServiceEndpoint,
    _child: Mutex<Child>,
}

impl HostToolsService {
    async fn start(client: &reqwest::Client) -> Result<Self> {
        let binary = resolve_host_binary()?;
        let mut child = Command::new(&binary)
            .arg("--shutdown-on-stdin-eof")
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
            .ripgrep(
                "session:recovery",
                serde_json::json!({ "pattern": "needle" }),
            )
            .await
            .unwrap_or_else(|error| panic!("sandbox recovery failed: {error}"));

        assert_eq!(result["found"], true);
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
            .ripgrep("session:tool-error", serde_json::json!({ "pattern": "[" }))
            .await
        {
            Ok(result) => panic!("expected tool error, got {result}"),
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
            .list_directory("session:list", "/workspace".into())
            .await
            .unwrap_or_else(|error| panic!("list directory call failed: {error}"));

        assert_eq!(result, "src/\nCargo.toml (1 line)");
        assert_eq!(backend.ensure_ready_calls.load(Ordering::SeqCst), 1);
        call.assert_async().await;
    }
}
