//! Apple Container sandbox backend (macOS 26+, Apple Silicon).

#[cfg(target_os = "macos")]
use std::collections::HashMap;

#[cfg(target_os = "macos")]
use async_trait::async_trait;
#[cfg(target_os = "macos")]
use chelix_protocol::{TOOLS_SERVICE_CONTAINER_PORT, TOOLS_SERVICE_TOKEN_ENV};
use tracing::{debug, info, warn};

#[cfg(target_os = "macos")]
use tokio::sync::RwLock;

#[cfg(target_os = "macos")]
use super::paths::resolved_sandbox_mount_plan;
#[cfg(target_os = "macos")]
use super::types::{
    BuildImageResult, Sandbox, SandboxBackendId, SandboxConfig, SandboxId, SharedSandboxImage,
    ToolsServiceEndpoint, ToolsServiceInstance, canonical_sandbox_packages,
    sanitize_path_component, shared_sandbox_image, tail_lines, truncate_output_for_display,
};
#[cfg(target_os = "macos")]
use super::{
    containers::{
        AppleContainerState, AppleManagedContainer, ApplePublishProtocol,
        TOOLS_SERVICE_INSTALL_PATH, apple_container_exec_args, apple_container_run_args,
        current_sandbox_image_tag, install_tools_service_in_build_context,
        sandbox_image_dockerfile, sandbox_image_exists, sandbox_tools_service_artifact,
        unmark_zombie,
    },
    docker::{ToolsHealthStatus, probe_tools_health},
};
#[cfg(target_os = "macos")]
use crate::command::{CommandOptions, CommandOutput};
#[cfg(target_os = "macos")]
use crate::error::{Error, Result};

/// Apple Container sandbox using the `container` CLI (macOS 26+, Apple Silicon).
#[cfg(target_os = "macos")]
pub struct AppleContainerSandbox {
    pub config: SandboxConfig,
    effective_image: SharedSandboxImage,
    tools_endpoints: RwLock<HashMap<String, ToolsServiceEndpoint>>,
}

#[cfg(target_os = "macos")]
impl AppleContainerSandbox {
    pub fn new(config: SandboxConfig) -> Self {
        let effective_image = shared_sandbox_image(&config);
        Self::new_with_global_image(config, effective_image)
    }

    pub(crate) fn new_with_global_image(
        config: SandboxConfig,
        effective_image: SharedSandboxImage,
    ) -> Self {
        Self {
            config,
            effective_image,
            tools_endpoints: RwLock::new(HashMap::new()),
        }
    }

    fn container_prefix(&self) -> &str {
        self.config
            .container_prefix
            .as_deref()
            .unwrap_or("chelix-sandbox")
    }

    pub(crate) fn container_name(&self, id: &SandboxId) -> String {
        format!("{}-{}", self.container_prefix(), id.key)
    }

    fn image_repo(&self) -> &str {
        self.container_prefix()
    }

    pub(crate) fn mount_specs(&self, id: &SandboxId) -> Result<Vec<String>> {
        Ok(
            resolved_sandbox_mount_plan(&self.config, Some("container"), id)?
                .into_iter()
                .filter_map(|mount| {
                    // Apple Container documents host-directory sharing, and its 0.12 release
                    // notes call out a fix for unreliable single-file mounts. Chelix does not
                    // enforce that minimum version, so keep launch arguments compatible with
                    // older installations by omitting non-directory sources.
                    if !mount.host.is_dir() {
                        debug!(
                            host = %mount.host.display(),
                            guest = %mount.guest.display(),
                            "skipping non-directory Apple Container bind mount"
                        );
                        return None;
                    }
                    let readonly = match mount.mode {
                        chelix_config::container_mounts::MountMode::Ro => ",readonly",
                        chelix_config::container_mounts::MountMode::Rw => "",
                    };
                    Some(format!(
                        "source={},target={}{}",
                        mount.host.display(),
                        mount.guest.display(),
                        readonly
                    ))
                })
                .collect(),
        )
    }

    async fn resolve_local_image(&self, requested_image: &str) -> Result<String> {
        if sandbox_image_exists("container", requested_image).await {
            return Ok(requested_image.to_string());
        }

        if requested_image.starts_with(&format!("{}:", self.image_repo())) {
            return Err(Error::message(format!(
                "current sandbox image {requested_image} is missing from the Apple Container store; rebuild it before launching a sandbox"
            )));
        }

        Ok(requested_image.to_string())
    }

    /// Check whether the `container` CLI is available.
    pub async fn is_available() -> bool {
        tokio::process::Command::new("container")
            .arg("--version")
            .output()
            .await
            .is_ok_and(|o| o.status.success())
    }

    async fn list_containers() -> Result<Vec<AppleManagedContainer>> {
        let output = tokio::process::Command::new("container")
            .args(["list", "--all", "--format", "json"])
            .output()
            .await?;
        if !output.status.success() {
            return Err(Error::message(format!(
                "container list failed: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            )));
        }
        serde_json::from_slice(&output.stdout)
            .map_err(|error| Error::message(format!("invalid container list JSON: {error}")))
    }

    async fn inspect_container(name: &str) -> Result<AppleManagedContainer> {
        let output = tokio::process::Command::new("container")
            .args(["inspect", name])
            .output()
            .await?;
        if !output.status.success() {
            return Err(Error::message(format!(
                "container inspect failed for {name}: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            )));
        }
        let entries: Vec<AppleManagedContainer> = serde_json::from_slice(&output.stdout)
            .map_err(|error| Error::message(format!("invalid container inspect JSON: {error}")))?;
        entries
            .into_iter()
            .find(|container| container.id == name)
            .ok_or_else(|| {
                Error::message(format!("container inspect returned no record for {name}"))
            })
    }

    async fn container_command(command: &str, name: &str) -> Result<()> {
        let output = tokio::process::Command::new("container")
            .args([command, name])
            .output()
            .await?;
        if !output.status.success() {
            return Err(Error::message(format!(
                "container {command} failed for {name}: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            )));
        }
        Ok(())
    }

    fn endpoint_from_inspect(container: &AppleManagedContainer) -> Result<ToolsServiceEndpoint> {
        if container.status.state != AppleContainerState::Running {
            return Err(Error::message(format!(
                "container {} is not running: {:?}",
                container.id, container.status.state
            )));
        }
        let token_prefix = format!("{TOOLS_SERVICE_TOKEN_ENV}=");
        let token = container
            .configuration
            .init_process
            .environment
            .iter()
            .find_map(|value| {
                value
                    .strip_prefix(&token_prefix)
                    .filter(|token| !token.is_empty())
            })
            .ok_or_else(|| {
                Error::message(format!(
                    "container {} has no tools service token",
                    container.id
                ))
            })?;
        let port = container
            .configuration
            .published_ports
            .iter()
            .find(|port| {
                port.container_port == TOOLS_SERVICE_CONTAINER_PORT
                    && port.proto == ApplePublishProtocol::Tcp
                    && port.count == 1
                    && port.host_port != 0
            })
            .ok_or_else(|| {
                Error::message(format!(
                    "container {} has no published tools service port",
                    container.id
                ))
            })?;
        let host = match port.host_address {
            std::net::IpAddr::V4(address) => address.to_string(),
            std::net::IpAddr::V6(address) => format!("[{address}]"),
        };
        Ok(ToolsServiceEndpoint {
            base_url: format!("http://{host}:{}", port.host_port),
            token: token.to_string(),
        })
    }

    async fn run_container(
        name: &str,
        image: &str,
        tz: Option<&str>,
        mounts: &[String],
        endpoint: &ToolsServiceEndpoint,
        terminal_cols: u16,
        terminal_rows: u16,
    ) -> Result<()> {
        let port = endpoint
            .base_url
            .rsplit_once(':')
            .and_then(|(_, port)| port.parse::<u16>().ok())
            .ok_or_else(|| Error::message("invalid tools service endpoint port"))?;
        let args = apple_container_run_args(
            name,
            image,
            tz,
            mounts,
            &endpoint.token,
            port,
            terminal_cols,
            terminal_rows,
        );
        let output = tokio::process::Command::new("container")
            .args(&args)
            .output()
            .await?;
        if !output.status.success() {
            return Err(Error::message(format!(
                "container run failed for {name}: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            )));
        }
        Ok(())
    }

    async fn update_tools_service(name: &str) -> Result<()> {
        let artifact = sandbox_tools_service_artifact()?;
        let temporary = format!("{TOOLS_SERVICE_INSTALL_PATH}.{}", uuid::Uuid::new_v4());
        let output = tokio::process::Command::new("container")
            .arg("cp")
            .arg(&artifact)
            .arg(format!("{name}:{temporary}"))
            .output()
            .await?;
        if !output.status.success() {
            return Err(Error::message(format!(
                "container cp failed for {name}: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            )));
        }
        let output = tokio::process::Command::new("container")
            .args([
                "exec",
                name,
                "mv",
                "-f",
                &temporary,
                TOOLS_SERVICE_INSTALL_PATH,
            ])
            .output()
            .await?;
        if !output.status.success() {
            return Err(Error::message(format!(
                "container exec mv failed for {name}: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            )));
        }
        Self::container_command("stop", name).await?;
        Self::container_command("start", name).await
    }

    fn allocate_tools_endpoint() -> Result<ToolsServiceEndpoint> {
        let listener = std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))?;
        let port = listener.local_addr()?.port();
        drop(listener);
        Ok(ToolsServiceEndpoint {
            base_url: format!("http://127.0.0.1:{port}"),
            token: format!(
                "{}{}",
                uuid::Uuid::new_v4().simple(),
                uuid::Uuid::new_v4().simple()
            ),
        })
    }

    async fn wait_for_tools_health(endpoint: &ToolsServiceEndpoint) -> Result<ToolsHealthStatus> {
        const MAX_ATTEMPTS: usize = 50;
        const RETRY_DELAY: std::time::Duration = std::time::Duration::from_millis(100);
        let client = reqwest::Client::new();
        let mut last_error = String::new();
        for attempt in 0..MAX_ATTEMPTS {
            match probe_tools_health(&client, endpoint).await {
                Ok(health) => return Ok(health),
                Err(error) => {
                    warn!(attempt, %error, "apple container tools health retry");
                    last_error = error.to_string();
                },
            }
            if attempt + 1 < MAX_ATTEMPTS {
                tokio::time::sleep(RETRY_DELAY).await;
            }
        }
        Err(Error::message(format!(
            "apple container tools service did not become ready: {last_error}"
        )))
    }

    async fn remember_tools_endpoint(&self, name: &str, endpoint: ToolsServiceEndpoint) {
        self.tools_endpoints
            .write()
            .await
            .insert(name.to_string(), endpoint);
    }
}

/// Check whether the Apple Container system service is running.
#[cfg(target_os = "macos")]
fn is_apple_container_service_running() -> bool {
    std::process::Command::new("container")
        .args(["system", "status"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

/// Try to start the Apple Container system service.
/// Returns `true` if the service was successfully started.
#[cfg(target_os = "macos")]
fn try_start_apple_container_service() -> bool {
    tracing::info!("apple container service is not running, starting it automatically");
    let result = std::process::Command::new("container")
        .args(["system", "start"])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .status();
    match result {
        Ok(status) if status.success() => {
            tracing::info!("apple container service started successfully");
            true
        },
        Ok(status) => {
            tracing::warn!(
                exit_code = status.code(),
                "failed to start apple container service; run `container system start` manually"
            );
            false
        },
        Err(e) => {
            tracing::warn!(
                error = %e,
                "failed to start apple container service; run `container system start` manually"
            );
            false
        },
    }
}

/// Ensure the Apple Container system service is running, starting it if needed.
/// Returns `true` if the service is running (either already or after starting).
#[cfg(target_os = "macos")]
pub fn ensure_apple_container_service() -> bool {
    if is_apple_container_service_running() {
        return true;
    }
    try_start_apple_container_service()
}

#[cfg(target_os = "macos")]
#[async_trait]
impl Sandbox for AppleContainerSandbox {
    fn backend_id(&self) -> SandboxBackendId {
        SandboxBackendId::AppleContainer
    }

    fn provides_fs_isolation(&self) -> bool {
        true
    }

    async fn ensure_ready(&self, id: &SandboxId) -> Result<()> {
        let name = self.container_name(id);
        let exists = Self::list_containers()
            .await?
            .iter()
            .any(|container| container.id == name);
        let mut endpoint = if exists {
            let container = Self::inspect_container(&name).await?;
            match container.status.state {
                AppleContainerState::Running => Self::endpoint_from_inspect(&container)?,
                AppleContainerState::Stopped => {
                    Self::container_command("start", &name).await?;
                    Self::endpoint_from_inspect(&Self::inspect_container(&name).await?)?
                },
                AppleContainerState::Unknown | AppleContainerState::Stopping => {
                    return Err(Error::message(format!(
                        "container {name} cannot be prepared in state {:?}",
                        container.status.state
                    )));
                },
            }
        } else {
            let effective_image = self.effective_image.read().await.clone();
            let image = self.resolve_local_image(&effective_image).await?;
            let mounts = self.mount_specs(id)?;
            let terminal_size = self
                .config
                .terminal_size
                .ok_or_else(|| Error::message("tools.execute_command.terminal_size is required"))?;
            let endpoint = Self::allocate_tools_endpoint()?;
            Self::run_container(
                &name,
                &image,
                self.config.timezone.as_deref(),
                &mounts,
                &endpoint,
                terminal_size.cols,
                terminal_size.rows,
            )
            .await?;
            Self::endpoint_from_inspect(&Self::inspect_container(&name).await?)?
        };
        if Self::wait_for_tools_health(&endpoint).await? == ToolsHealthStatus::ProtocolMismatch {
            Self::update_tools_service(&name).await?;
            endpoint = Self::endpoint_from_inspect(&Self::inspect_container(&name).await?)?;
            if Self::wait_for_tools_health(&endpoint).await? == ToolsHealthStatus::ProtocolMismatch
            {
                return Err(Error::message(format!(
                    "tools service protocol mismatch in container {name} after update"
                )));
            }
        }
        self.remember_tools_endpoint(&name, endpoint).await;
        unmark_zombie(&name);
        Ok(())
    }

    async fn tools_service_endpoint(&self, id: &SandboxId) -> Result<ToolsServiceEndpoint> {
        let name = self.container_name(id);
        self.tools_endpoints
            .read()
            .await
            .get(&name)
            .cloned()
            .ok_or_else(|| {
                Error::message(format!(
                    "apple tools service endpoint is unavailable for container {name}"
                ))
            })
    }

    async fn tools_service_instances(&self) -> Result<Vec<ToolsServiceInstance>> {
        let mut instances = self
            .tools_endpoints
            .read()
            .await
            .iter()
            .map(|(name, endpoint)| ToolsServiceInstance {
                id: name.clone(),
                label: format!("{name} (apple-container)"),
                endpoint: endpoint.clone(),
            })
            .collect::<Vec<_>>();
        instances.sort_by(|left, right| left.id.cmp(&right.id));
        Ok(instances)
    }

    async fn existing_container_ids(&self) -> Result<Vec<String>> {
        let prefix = format!("{}-", self.container_prefix());
        Self::list_containers()
            .await?
            .into_iter()
            .filter_map(|container| container.id.strip_prefix(&prefix).map(str::to_string))
            .map(|id| {
                if id.is_empty() || sanitize_path_component(&id) != id {
                    return Err(Error::message(format!(
                        "invalid managed container ID: {id}"
                    )));
                }
                Ok(id)
            })
            .collect()
    }

    async fn run_command(
        &self,
        id: &SandboxId,
        command: &str,
        opts: &CommandOptions,
    ) -> Result<CommandOutput> {
        let name = self.container_name(id);
        info!(
            name,
            command = %opts.log_policy.for_log(command),
            "apple container exec"
        );

        // Apple Container CLI doesn't support -e flags, so prepend export
        // statements to inject env vars into the shell.
        let mut prefix = String::new();

        for (k, v) in &opts.env {
            // Shell-escape the value with single quotes.
            let escaped = v.replace('\'', "'\\''");
            prefix.push_str(&format!("export {k}='{escaped}'; "));
        }

        let full_command = if let Some(ref dir) = opts.working_dir {
            format!("{prefix}cd {} && {command}", dir.display())
        } else {
            format!("{prefix}{command}")
        };

        let args = apple_container_exec_args(&name, full_command);

        let child = tokio::process::Command::new("container")
            .args(&args)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .stdin(std::process::Stdio::null())
            .spawn()?;

        let result = tokio::time::timeout(opts.timeout, child.wait_with_output()).await;

        match result {
            Ok(Ok(output)) => {
                let exit_code = output.status.code().unwrap_or(-1);
                let mut stdout = String::from_utf8_lossy(&output.stdout).into_owned();
                let mut stderr = String::from_utf8_lossy(&output.stderr).into_owned();

                truncate_output_for_display(&mut stdout, opts.max_output_bytes);
                truncate_output_for_display(&mut stderr, opts.max_output_bytes);

                debug!(
                    name,
                    exit_code,
                    stdout_len = stdout.len(),
                    stderr_len = stderr.len(),
                    "apple container exec complete"
                );
                Ok(CommandOutput {
                    stdout,
                    stderr,
                    exit_code,
                })
            },
            Ok(Err(e)) => {
                warn!(name, %e, "apple container exec spawn failed");
                return Err(Error::message(format!(
                    "container exec failed for {name}: {e}"
                )));
            },
            Err(_) => {
                warn!(
                    name,
                    timeout_secs = opts.timeout.as_secs(),
                    "apple container exec timed out"
                );
                return Err(Error::message(format!(
                    "container exec timed out for {name} after {}s",
                    opts.timeout.as_secs()
                )));
            },
        }
    }

    async fn build_image(
        &self,
        base: &str,
        packages: &[String],
    ) -> Result<Option<BuildImageResult>> {
        let tag = current_sandbox_image_tag(self.image_repo(), base, packages)?;

        if sandbox_image_exists("container", &tag).await {
            debug!(
                tag,
                "pre-built sandbox image already exists, skipping build"
            );
            return Ok(Some(BuildImageResult { tag, built: false }));
        }

        let tmp_dir =
            std::env::temp_dir().join(format!("chelix-sandbox-build-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&tmp_dir)?;

        let pkg_list = canonical_sandbox_packages(packages).join(" ");
        let dockerfile = sandbox_image_dockerfile(base, packages);
        let dockerfile_path = tmp_dir.join("Dockerfile");
        std::fs::write(&dockerfile_path, &dockerfile)?;
        install_tools_service_in_build_context(&tmp_dir)?;

        info!(tag, packages = %pkg_list, "building pre-built sandbox image (apple container)");

        let output = tokio::process::Command::new("container")
            .args(["build", "-t", &tag, "-f"])
            .arg(&dockerfile_path)
            .arg(&tmp_dir)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .output()
            .await;

        let _ = std::fs::remove_dir_all(&tmp_dir);

        let output = output?;
        if !output.status.success() {
            let stdout = String::from_utf8_lossy(&output.stdout);
            let stderr = String::from_utf8_lossy(&output.stderr);
            if stderr.contains("XPC connection error") || stderr.contains("Connection invalid") {
                return Err(Error::message(
                    "apple container service is not running. \
                     Start it with `container system start` and restart chelix",
                ));
            }
            debug!(
                tag,
                stdout = %tail_lines(&stdout, 20),
                stderr = %tail_lines(&stderr, 20),
                "container build failed"
            );
            let status = output.status.code().map_or_else(
                || output.status.to_string(),
                |code| format!("exit code {code}"),
            );
            return Err(Error::message(format!(
                "container build failed for {tag}: {}",
                status
            )));
        }

        info!(tag, "pre-built sandbox image ready (apple container)");
        Ok(Some(BuildImageResult { tag, built: true }))
    }

    async fn cleanup(&self, id: &SandboxId) -> Result<()> {
        let name = self.container_name(id);
        if Self::list_containers()
            .await?
            .iter()
            .any(|container| container.id == name)
        {
            let output = tokio::process::Command::new("container")
                .args(["rm", "--force", &name])
                .output()
                .await?;
            if !output.status.success() {
                return Err(Error::message(format!(
                    "container rm --force failed for {name}: {}",
                    String::from_utf8_lossy(&output.stderr).trim()
                )));
            }
        }
        self.tools_endpoints.write().await.remove(&name);
        unmark_zombie(&name);
        Ok(())
    }
}
