//! Docker and Podman sandbox backends.

use {
    async_trait::async_trait,
    chelix_protocol::{
        TOOLS_SERVICE_CONTAINER_PORT, TOOLS_SERVICE_HEALTH_PATH, TOOLS_SERVICE_PROTOCOL_VERSION,
        TOOLS_SERVICE_TOKEN_ENV, ToolsServiceHealth,
    },
    std::{
        collections::{HashMap, HashSet},
        net::IpAddr,
        sync::{Arc, OnceLock},
    },
    tokio::sync::{Mutex, Semaphore},
    tracing::{debug, info, warn},
};

use {
    super::{
        containers::{
            current_sandbox_image_tag, install_tools_service_in_build_context,
            sandbox_image_dockerfile, sandbox_image_exists,
        },
        paths::resolved_sandbox_mount_plan,
        provision::provision_packages,
        types::{
            BuildImageResult, Sandbox, SandboxBackendId, SandboxConfig, SandboxId,
            SharedSandboxImage, ToolsServiceEndpoint, WorkspaceSysmount,
            canonical_sandbox_packages, shared_sandbox_image, tail_lines,
            truncate_output_for_display,
        },
    },
    crate::{
        command::{CommandOptions, CommandOutput, run_shell_command},
        error::{Error, Result},
        sandbox::file_system::{
            SandboxListFilesResult, SandboxReadResult, native_host_list_files,
            native_host_read_file, native_host_write_file, oci_container_list_files,
            oci_container_read_file, oci_container_write_file,
        },
    },
};

/// Distinguishes Docker from Podman for behaviour that differs between the two
/// OCI runtimes (hardening flags, platform defaults, etc.).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BackendKind {
    Docker,
    Podman,
}

const DEFAULT_OCI_CPU_QUOTA: f64 = 1.0;
const TOOLS_SERVICE_HEALTH_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(2);

/// Docker/Podman-based sandbox implementation.
///
/// The `cli` field selects the container CLI binary (`"docker"` or `"podman"`).
/// Podman's CLI is a drop-in replacement for Docker, so both backends share
/// this single implementation.  `kind` carries the typed backend identity for
/// behaviour branching without string comparisons.
pub struct DockerSandbox {
    pub config: SandboxConfig,
    effective_image: SharedSandboxImage,
    pub(crate) kind: BackendKind,
    cli: &'static str,
    /// Container names that have already been provisioned in this process.
    /// Prevents repeated `apt-get install` runs on the same container.
    pub(crate) provisioned: Mutex<HashSet<String>>,
    /// Per-container startup gates. Parallel command calls for the same session
    /// must not race through inspect-then-run with the same OCI container name.
    startup_gates: Mutex<HashMap<String, Arc<Semaphore>>>,
    tools_endpoints: Mutex<HashMap<String, ToolsServiceEndpoint>>,
    tools_http_client: reqwest::Client,
}

impl DockerSandbox {
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
            kind: BackendKind::Docker,
            cli: "docker",
            provisioned: Mutex::new(HashSet::new()),
            startup_gates: Mutex::new(HashMap::new()),
            tools_endpoints: Mutex::new(HashMap::new()),
            tools_http_client: reqwest::Client::new(),
        }
    }

    pub fn podman(config: SandboxConfig) -> Self {
        let effective_image = shared_sandbox_image(&config);
        Self::podman_with_global_image(config, effective_image)
    }

    pub(crate) fn podman_with_global_image(
        config: SandboxConfig,
        effective_image: SharedSandboxImage,
    ) -> Self {
        Self {
            config,
            effective_image,
            kind: BackendKind::Podman,
            cli: "podman",
            provisioned: Mutex::new(HashSet::new()),
            startup_gates: Mutex::new(HashMap::new()),
            tools_endpoints: Mutex::new(HashMap::new()),
            tools_http_client: reqwest::Client::new(),
        }
    }

    #[cfg(test)]
    pub(crate) fn with_cli(config: SandboxConfig, cli: &'static str) -> Self {
        let effective_image = shared_sandbox_image(&config);
        Self {
            config,
            effective_image,
            kind: BackendKind::Docker,
            cli,
            provisioned: Mutex::new(HashSet::new()),
            startup_gates: Mutex::new(HashMap::new()),
            tools_endpoints: Mutex::new(HashMap::new()),
            tools_http_client: reqwest::Client::new(),
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

    pub(crate) fn image_repo(&self) -> &str {
        self.container_prefix()
    }

    #[cfg(test)]
    pub(crate) async fn startup_gate_for(&self, name: &str) -> Arc<Semaphore> {
        self.startup_gate_for_inner(name).await
    }

    async fn startup_gate_for_inner(&self, name: &str) -> Arc<Semaphore> {
        let mut gates = self.startup_gates.lock().await;
        Arc::clone(
            gates
                .entry(name.to_string())
                .or_insert_with(|| Arc::new(Semaphore::new(1))),
        )
    }

    async fn remove_startup_gate_if_unshared(&self, name: &str, gate: &Arc<Semaphore>) {
        let mut gates = self.startup_gates.lock().await;
        let Some(stored) = gates.get(name) else {
            return;
        };
        if Arc::ptr_eq(stored, gate) && Arc::strong_count(gate) == 2 {
            gates.remove(name);
        }
    }

    async fn is_container_running(&self, name: &str) -> bool {
        let check = tokio::process::Command::new(self.cli)
            .args(["inspect", "--format", "{{.State.Running}}", name])
            .output()
            .await;

        let Ok(output) = check else {
            return false;
        };
        String::from_utf8_lossy(&output.stdout).trim() == "true"
    }

    pub(crate) fn resource_args(&self) -> Vec<String> {
        let mut args = Vec::new();
        let limits = &self.config.resource_limits;
        if let Some(ref mem) = limits.memory_limit {
            args.extend(["--memory".to_string(), mem.clone()]);
        }
        let cpu = limits.cpu_quota.unwrap_or(DEFAULT_OCI_CPU_QUOTA);
        args.extend(["--cpus".to_string(), cpu.to_string()]);
        if let Some(pids) = limits.pids_max {
            args.extend(["--pids-limit".to_string(), pids.to_string()]);
        }
        if let Some(ref gpus) = self.config.gpus {
            args.extend(["--gpus".to_string(), gpus.clone()]);
        }
        args
    }

    pub(crate) fn network_run_args(&self) -> Vec<String> {
        vec![format!("--network={}", self.config.network)]
    }

    /// Security hardening flags for `docker run`.
    ///
    /// `is_prebuilt` and `workspace_sysmount` control whether read-only rootfs
    /// and privilege-hardening flags are applied. Prebuilt images already have
    /// packages baked in, so the root filesystem may be read-only; non-prebuilt
    /// images need a writable root for `apt-get` provisioning.
    pub(crate) fn hardening_args(
        is_prebuilt: bool,
        kind: BackendKind,
        workspace_sysmount: WorkspaceSysmount,
    ) -> Vec<String> {
        let mut args = vec![
            // --- Writable tmpfs mounts ---
            "--tmpfs".to_string(),
            "/tmp:rw,nosuid,size=256m".to_string(),
            "--tmpfs".to_string(),
            "/run:rw,nosuid,size=64m".to_string(),
            // --- Host metadata isolation ---
            // Give the container its own hostname so /proc/sys/kernel/hostname
            // and the `hostname` command do not reveal the host identity.
            "--hostname".to_string(),
            "sandbox".to_string(),
        ];
        if workspace_sysmount == WorkspaceSysmount::Ro {
            args.splice(0..0, [
                "--cap-drop".to_string(),
                "ALL".to_string(),
                "--security-opt".to_string(),
                "no-new-privileges".to_string(),
            ]);
        }
        // Mask /sys subtrees that expose host hardware identifiers
        // (serial numbers, BIOS/UEFI data, disk models, LUKS UUIDs).
        // Empty read-only tmpfs overlays hide the underlying sysfs entries.
        //
        // Skipped for Podman: its OCI runtime performs "tmpcopyup" on sysfs
        // tmpfs mounts, copying directory contents into the tmpfs first.
        // With --cap-drop ALL some sysfs files are permission-denied even for
        // root, causing the mount (and container startup) to fail.  Podman
        // already masks /sys/firmware via its built-in OCI MaskedPaths.
        if kind != BackendKind::Podman {
            args.push("--init".to_string());
            for path in sysfs_paths_to_mask() {
                args.extend(["--tmpfs".to_string(), format!("{path}:ro,nosuid")]);
            }
        }
        if is_prebuilt && workspace_sysmount == WorkspaceSysmount::Ro {
            args.push("--read-only".to_string());
        }
        args
    }

    pub(crate) fn mount_args(&self, id: &SandboxId) -> Result<Vec<String>> {
        let mounts = resolved_sandbox_mount_plan(&self.config, Some(self.cli), id)?;
        Ok(mounts
            .into_iter()
            .flat_map(|mount| {
                let mode = mount.mode.as_str();
                [
                    "-v".to_string(),
                    format!("{}:{}:{mode}", mount.host.display(), mount.guest.display()),
                ]
            })
            .collect())
    }

    async fn resolve_local_image(&self, requested_image: &str) -> Result<String> {
        if sandbox_image_exists(self.cli, requested_image).await {
            debug!(image = requested_image, "sandbox image found locally");
            return Ok(requested_image.to_string());
        }

        if requested_image.starts_with(&format!("{}:", self.image_repo())) {
            return Err(Error::message(format!(
                "current sandbox image {requested_image} is missing from the {} store; rebuild it before launching a sandbox",
                self.cli
            )));
        }

        Ok(requested_image.to_string())
    }

    /// Export an image from BuildKit's cache into the Podman store.
    ///
    /// When Podman delegates `podman build` to a BuildKit daemon the image may
    /// land only in BuildKit's internal cache.  This method re-runs the build
    /// with `--output type=docker,dest=<file>` (a BuildKit cache-hit, so
    /// essentially free) and pipes the tarball into `podman load`.
    async fn export_buildkit_image_to_store(
        &self,
        tag: &str,
        dockerfile_path: &std::path::Path,
        context_dir: &std::path::Path,
    ) -> Result<()> {
        let tar_path = std::env::temp_dir().join(format!(
            "chelix-sandbox-export-{}.tar",
            uuid::Uuid::new_v4()
        ));

        // Re-build with docker-archive output.  The `-t` flag embeds the
        // correct tag in the archive so `podman load` names it correctly.
        // BuildKit's layer cache makes this a near-instant cache hit for the
        // same Dockerfile.
        let export_output = tokio::process::Command::new(self.cli)
            .args([
                "build",
                "--output",
                &format!("type=docker,dest={}", tar_path.display()),
                "-t",
                tag,
                "-f",
            ])
            .arg(dockerfile_path)
            .arg(context_dir)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .output()
            .await?;

        if !export_output.status.success() {
            let _ = std::fs::remove_file(&tar_path);
            let stderr = String::from_utf8_lossy(&export_output.stderr);
            return Err(Error::message(format!(
                "podman build --output failed for {tag}: {}",
                stderr.trim()
            )));
        }

        // Load the tarball into the Podman store.
        let load_output = tokio::process::Command::new(self.cli)
            .args(["load", "-i"])
            .arg(&tar_path)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .output()
            .await?;

        let _ = std::fs::remove_file(&tar_path);

        if !load_output.status.success() {
            let stderr = String::from_utf8_lossy(&load_output.stderr);
            return Err(Error::message(format!(
                "podman load failed for {tag}: {}",
                stderr.trim()
            )));
        }

        // Final verification.
        if !sandbox_image_exists(self.cli, tag).await {
            return Err(Error::message(format!(
                "image {tag} still missing from podman store after BuildKit export"
            )));
        }

        info!(tag, "successfully exported BuildKit image to podman store");
        Ok(())
    }

    async fn ensure_ready_locked(&self, id: &SandboxId) -> Result<()> {
        let name = self.container_name(id);

        if self.is_container_running(&name).await {
            let cached_endpoint = self.tools_endpoints.lock().await.get(&name).cloned();
            if let Some(endpoint) = cached_endpoint {
                if probe_tools_health(&self.tools_http_client, &endpoint)
                    .await
                    .is_ok()
                {
                    debug!(container = %name, "sandbox container already running");
                    return Ok(());
                }

                warn!(
                    container = %name,
                    "sandbox container has a stale tools endpoint, rediscovering"
                );
                match self
                    .discover_tools_service_endpoint(&name, endpoint.token)
                    .await
                {
                    Ok(endpoint) => {
                        self.tools_endpoints
                            .lock()
                            .await
                            .insert(name.clone(), endpoint);
                        return Ok(());
                    },
                    Err(error) => {
                        warn!(
                            container = %name,
                            %error,
                            "sandbox container tools endpoint recovery failed, recreating"
                        );
                    },
                }
            } else {
                warn!(container = %name, "sandbox container has no runtime tools endpoint, recreating");
            }

            self.provisioned.lock().await.remove(&name);
            self.tools_endpoints.lock().await.remove(&name);
            force_remove_container(self.cli, &name).await?;
        }

        self.provisioned.lock().await.remove(&name);
        self.tools_endpoints.lock().await.remove(&name);

        // Resolve image first so we know whether it's prebuilt (affects hardening).
        let effective_image = self.effective_image.read().await.clone();
        let image = self.resolve_local_image(&effective_image).await?;
        let is_prebuilt = image.starts_with(&format!("{}:", self.image_repo()));
        debug!(container = %name, %image, is_prebuilt, "resolved sandbox image");

        // Start a new container.
        info!(container = %name, %image, "starting new sandbox container");
        let mut args = vec![
            "run".to_string(),
            "-d".to_string(),
            "--name".to_string(),
            name.clone(),
        ];

        args.extend(self.network_run_args());
        let tools_token = format!(
            "{}{}",
            uuid::Uuid::new_v4().simple(),
            uuid::Uuid::new_v4().simple()
        );
        args.extend([
            "-e".to_string(),
            format!("{TOOLS_SERVICE_TOKEN_ENV}={tools_token}"),
            "-p".to_string(),
            format!("127.0.0.1::{TOOLS_SERVICE_CONTAINER_PORT}"),
        ]);

        if let Some(ref tz) = self.config.timezone {
            args.extend(["-e".to_string(), format!("TZ={tz}")]);
        }

        args.extend(self.resource_args());
        args.extend(Self::hardening_args(
            is_prebuilt,
            self.kind,
            self.config.workspace_sysmount,
        ));
        args.extend(self.mount_args(id)?);

        args.push(image);
        args.extend([
            "chelix-tools-service".to_string(),
            "--listen".to_string(),
            format!("0.0.0.0:{TOOLS_SERVICE_CONTAINER_PORT}"),
        ]);

        let output = tokio::process::Command::new(self.cli)
            .args(&args)
            .output()
            .await?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            if is_container_name_conflict(&stderr) {
                warn!(
                    container = %name,
                    "{} run reported a name conflict, recreating container",
                    self.cli
                );
                self.provisioned.lock().await.remove(&name);
                self.tools_endpoints.lock().await.remove(&name);
                let _ = tokio::process::Command::new(self.cli)
                    .args(["rm", "-f", &name])
                    .output()
                    .await;

                let retry_output = tokio::process::Command::new(self.cli)
                    .args(&args)
                    .output()
                    .await?;
                if !retry_output.status.success() {
                    let retry_stderr = String::from_utf8_lossy(&retry_output.stderr);
                    return Err(Error::message(format!(
                        "{} run failed after removing stale container '{}': {}",
                        self.cli,
                        name,
                        retry_stderr.trim()
                    )));
                }
            } else {
                return Err(Error::message(format!(
                    "{} run failed: {}",
                    self.cli,
                    stderr.trim()
                )));
            }
        }

        let endpoint = match self
            .discover_tools_service_endpoint(&name, tools_token)
            .await
        {
            Ok(endpoint) => endpoint,
            Err(error) => {
                self.provisioned.lock().await.remove(&name);
                self.tools_endpoints.lock().await.remove(&name);
                if let Err(cleanup_error) = force_remove_container(self.cli, &name).await {
                    warn!(
                        container = %name,
                        error = %cleanup_error,
                        "failed to remove sandbox container after tools service readiness failure"
                    );
                }
                return Err(error);
            },
        };
        self.tools_endpoints
            .lock()
            .await
            .insert(name.clone(), endpoint);

        // Skip provisioning if the image is a pre-built instance sandbox image
        // (packages are already baked in — including /home/sandbox from the Dockerfile).
        if !is_prebuilt {
            let needs_provisioning = {
                let mut provisioned = self.provisioned.lock().await;
                if provisioned.contains(&name) {
                    false
                } else {
                    provisioned.insert(name.clone());
                    true
                }
            };
            if needs_provisioning {
                if let Err(e) = provision_packages(self.cli, &name, &self.config.packages).await {
                    self.provisioned.lock().await.remove(&name);
                    return Err(e);
                }
            } else {
                debug!(
                    container = %name,
                    "skipping provisioning, already completed for container"
                );
            }
        }

        Ok(())
    }

    pub(super) async fn discover_tools_service_endpoint(
        &self,
        name: &str,
        token: String,
    ) -> Result<ToolsServiceEndpoint> {
        const MAX_ATTEMPTS: usize = 50;
        const RETRY_DELAY: std::time::Duration = std::time::Duration::from_millis(100);
        let candidates = self.tools_service_endpoint_candidates(name, &token).await?;
        let mut last_error = String::new();

        for attempt in 0..MAX_ATTEMPTS {
            match select_reachable_tools_service_endpoint(&self.tools_http_client, &candidates)
                .await
            {
                Ok(endpoint) => {
                    debug!(
                        container = %name,
                        base_url = %endpoint.base_url,
                        "sandbox tools service endpoint ready"
                    );
                    return Ok(endpoint);
                },
                Err(error) => last_error = error,
            }
            if attempt + 1 < MAX_ATTEMPTS {
                tokio::time::sleep(RETRY_DELAY).await;
            }
        }

        Err(Error::message(format!(
            "{} tools service in container {name} did not become ready: {last_error}",
            self.cli
        )))
    }

    async fn tools_service_endpoint_candidates(
        &self,
        name: &str,
        token: &str,
    ) -> Result<Vec<ToolsServiceEndpoint>> {
        let published_output = tokio::process::Command::new(self.cli)
            .args(["port", name, &format!("{TOOLS_SERVICE_CONTAINER_PORT}/tcp")])
            .output()
            .await?;
        let inspect_output = tokio::process::Command::new(self.cli)
            .args([
                "inspect",
                "--format",
                tools_service_inspect_template(self.kind),
                name,
            ])
            .output()
            .await?;

        let published = if published_output.status.success() {
            String::from_utf8_lossy(&published_output.stdout).into_owned()
        } else {
            String::new()
        };
        let addresses = if inspect_output.status.success() {
            String::from_utf8_lossy(&inspect_output.stdout).into_owned()
        } else {
            String::new()
        };
        let candidates = tools_service_endpoint_candidates(&published, &addresses, token);
        if candidates.is_empty() {
            let published_error = String::from_utf8_lossy(&published_output.stderr);
            let inspect_error = String::from_utf8_lossy(&inspect_output.stderr);
            return Err(Error::message(format!(
                "{} returned no tools service endpoint candidates for container {name}; port error: {}; inspect error: {}",
                self.cli,
                published_error.trim(),
                inspect_error.trim()
            )));
        }
        Ok(candidates)
    }
}

pub(super) fn tools_service_inspect_template(kind: BackendKind) -> &'static str {
    match kind {
        BackendKind::Docker => "{{range .NetworkSettings.Networks}}{{println .IPAddress}}{{end}}",
        BackendKind::Podman => "{{println .NetworkSettings.IPAddress}}",
    }
}

pub(super) async fn force_remove_container(cli: &str, name: &str) -> Result<()> {
    let cleanup = tokio::process::Command::new(cli)
        .args(["rm", "-f", name])
        .output()
        .await?;
    if !cleanup.status.success() {
        let stderr = String::from_utf8_lossy(&cleanup.stderr);
        return Err(Error::message(format!(
            "{cli} rm -f failed for container {name}: {}",
            stderr.trim()
        )));
    }
    Ok(())
}

pub(super) fn tools_service_endpoint_candidates(
    published_output: &str,
    inspect_output: &str,
    token: &str,
) -> Vec<ToolsServiceEndpoint> {
    let mut base_urls = Vec::new();
    if let Some(port) = parse_published_port(published_output) {
        base_urls.push(format!("http://127.0.0.1:{port}"));
    }
    for address in parse_container_addresses(inspect_output) {
        let host = match address {
            IpAddr::V4(address) => address.to_string(),
            IpAddr::V6(address) => format!("[{address}]"),
        };
        base_urls.push(format!("http://{host}:{TOOLS_SERVICE_CONTAINER_PORT}"));
    }
    base_urls.dedup();
    base_urls
        .into_iter()
        .map(|base_url| ToolsServiceEndpoint {
            base_url,
            token: token.to_string(),
        })
        .collect()
}

pub(super) fn parse_container_addresses(output: &str) -> Vec<IpAddr> {
    output
        .lines()
        .filter_map(|line| line.trim().parse::<IpAddr>().ok())
        .filter(|address| !address.is_unspecified())
        .collect()
}

fn parse_published_port(output: &str) -> Option<u16> {
    output.lines().find_map(|line| {
        line.trim()
            .rsplit_once(':')
            .and_then(|(_, port)| port.parse().ok())
    })
}

pub(super) async fn select_reachable_tools_service_endpoint(
    client: &reqwest::Client,
    candidates: &[ToolsServiceEndpoint],
) -> std::result::Result<ToolsServiceEndpoint, String> {
    let mut errors = Vec::new();
    for endpoint in candidates {
        match probe_tools_health(client, endpoint).await {
            Ok(()) => return Ok(endpoint.clone()),
            Err(error) => errors.push(format!("{}: {error}", endpoint.base_url)),
        }
    }
    Err(errors.join("; "))
}

async fn probe_tools_health(
    client: &reqwest::Client,
    endpoint: &ToolsServiceEndpoint,
) -> Result<()> {
    let response = client
        .get(format!(
            "{}{}",
            endpoint.base_url, TOOLS_SERVICE_HEALTH_PATH
        ))
        .bearer_auth(&endpoint.token)
        .timeout(TOOLS_SERVICE_HEALTH_TIMEOUT)
        .send()
        .await?;
    if !response.status().is_success() {
        return Err(Error::message(format!(
            "tools service health returned {}",
            response.status()
        )));
    }
    let health = response.json::<ToolsServiceHealth>().await?;
    if health.protocol_version != TOOLS_SERVICE_PROTOCOL_VERSION {
        return Err(Error::message(format!(
            "tools service protocol mismatch: expected {}, got {}",
            TOOLS_SERVICE_PROTOCOL_VERSION, health.protocol_version
        )));
    }
    Ok(())
}

#[async_trait]
impl Sandbox for DockerSandbox {
    fn backend_id(&self) -> SandboxBackendId {
        match self.kind {
            BackendKind::Docker => SandboxBackendId::Docker,
            BackendKind::Podman => SandboxBackendId::Podman,
        }
    }

    fn provides_fs_isolation(&self) -> bool {
        true
    }

    async fn ensure_ready(&self, id: &SandboxId) -> Result<()> {
        let name = self.container_name(id);
        let gate = self.startup_gate_for_inner(&name).await;
        let _permit = gate
            .acquire()
            .await
            .map_err(|_| Error::message("sandbox startup gate closed"))?;
        let result = self.ensure_ready_locked(id).await;
        if result.is_err() {
            self.remove_startup_gate_if_unshared(&name, &gate).await;
        }
        result
    }

    async fn tools_service_endpoint(&self, id: &SandboxId) -> Result<ToolsServiceEndpoint> {
        let name = self.container_name(id);
        self.tools_endpoints
            .lock()
            .await
            .get(&name)
            .cloned()
            .ok_or_else(|| {
                Error::message(format!(
                    "{} tools service endpoint is unavailable for container {name}",
                    self.cli
                ))
            })
    }

    async fn build_image(
        &self,
        base: &str,
        packages: &[String],
    ) -> Result<Option<BuildImageResult>> {
        let tag = current_sandbox_image_tag(self.image_repo(), base, packages)?;

        // Check if image already exists.
        if sandbox_image_exists(self.cli, &tag).await {
            debug!(
                tag,
                "pre-built sandbox image already exists, skipping build"
            );
            return Ok(Some(BuildImageResult { tag, built: false }));
        }

        // Generate Dockerfile in a temp dir.
        let tmp_dir =
            std::env::temp_dir().join(format!("chelix-sandbox-build-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&tmp_dir)?;

        let pkg_list = canonical_sandbox_packages(packages).join(" ");
        let dockerfile = sandbox_image_dockerfile(base, packages);
        let dockerfile_path = tmp_dir.join("Dockerfile");
        std::fs::write(&dockerfile_path, &dockerfile)?;
        install_tools_service_in_build_context(&tmp_dir)?;

        info!(tag, packages = %pkg_list, "building pre-built sandbox image");

        let output = tokio::process::Command::new(self.cli)
            .args(["build", "-t", &tag, "-f"])
            .arg(&dockerfile_path)
            .arg(&tmp_dir)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .output()
            .await;

        let output = output?;
        if !output.status.success() {
            let _ = std::fs::remove_dir_all(&tmp_dir);
            let stdout = String::from_utf8_lossy(&output.stdout);
            let stderr = String::from_utf8_lossy(&output.stderr);
            debug!(
                tag,
                stdout = %tail_lines(&stdout, 20),
                stderr = %tail_lines(&stderr, 20),
                "{} build failed",
                self.cli,
            );
            let status = output.status.code().map_or_else(
                || output.status.to_string(),
                |code| format!("exit code {code}"),
            );
            return Err(Error::message(format!(
                "{} build failed for {tag}: {}",
                self.cli, status
            )));
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        debug!(tag, output = %tail_lines(&stdout, 20), "docker build output");

        // Podman with BuildKit: the build may succeed (exit 0) but leave the
        // image in BuildKit's internal cache instead of the Podman store.
        // Verify the image is actually present and recover if not.
        if self.kind == BackendKind::Podman && !sandbox_image_exists(self.cli, &tag).await {
            warn!(
                tag,
                "podman build succeeded but image missing from store \
                 (likely BuildKit delegation), exporting via tarball"
            );
            let export_result = self
                .export_buildkit_image_to_store(&tag, &dockerfile_path, &tmp_dir)
                .await;
            // Clean up temp dir regardless of export result.
            let _ = std::fs::remove_dir_all(&tmp_dir);
            export_result?;
        } else {
            let _ = std::fs::remove_dir_all(&tmp_dir);
        }

        info!(tag, "pre-built sandbox image ready");
        Ok(Some(BuildImageResult { tag, built: true }))
    }

    async fn run_command(
        &self,
        id: &SandboxId,
        command: &str,
        opts: &CommandOptions,
    ) -> Result<CommandOutput> {
        let name = self.container_name(id);

        let mut args = vec!["exec".to_string()];

        if let Some(ref dir) = opts.working_dir {
            args.extend(["-w".to_string(), dir.display().to_string()]);
        }

        for (k, v) in &opts.env {
            args.extend(["-e".to_string(), format!("{}={}", k, v)]);
        }

        args.push(name);
        args.extend(["bash".to_string(), "-c".to_string(), command.to_string()]);

        let child = tokio::process::Command::new(self.cli)
            .args(&args)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .stdin(std::process::Stdio::null())
            .spawn()?;

        let result = tokio::time::timeout(opts.timeout, child.wait_with_output()).await;

        match result {
            Ok(Ok(output)) => {
                let mut stdout = String::from_utf8_lossy(&output.stdout).into_owned();
                let mut stderr = String::from_utf8_lossy(&output.stderr).into_owned();

                truncate_output_for_display(&mut stdout, opts.max_output_bytes);
                truncate_output_for_display(&mut stderr, opts.max_output_bytes);

                Ok(CommandOutput {
                    stdout,
                    stderr,
                    exit_code: output.status.code().unwrap_or(-1),
                })
            },
            Ok(Err(e)) => {
                return Err(Error::message(format!("{} command failed: {e}", self.cli)));
            },
            Err(_) => {
                return Err(Error::message(format!(
                    "{} command timed out after {}s",
                    self.cli,
                    opts.timeout.as_secs()
                )));
            },
        }
    }

    async fn read_file(
        &self,
        id: &SandboxId,
        file_path: &str,
        max_bytes: u64,
    ) -> Result<SandboxReadResult> {
        let container_name = self.container_name(id);
        oci_container_read_file(self.cli, &container_name, file_path, max_bytes).await
    }

    async fn write_file(
        &self,
        id: &SandboxId,
        file_path: &str,
        content: &[u8],
    ) -> Result<Option<serde_json::Value>> {
        let container_name = self.container_name(id);
        oci_container_write_file(self.cli, &container_name, file_path, content).await
    }

    async fn list_files(&self, id: &SandboxId, root: &str) -> Result<SandboxListFilesResult> {
        let container_name = self.container_name(id);
        oci_container_list_files(self.cli, &container_name, root).await
    }

    async fn cleanup(&self, id: &SandboxId) -> Result<()> {
        let name = self.container_name(id);
        self.provisioned.lock().await.remove(&name);
        self.startup_gates.lock().await.remove(&name);
        self.tools_endpoints.lock().await.remove(&name);
        let _ = tokio::process::Command::new(self.cli)
            .args(["rm", "-f", &name])
            .output()
            .await;
        Ok(())
    }
}

pub(crate) fn is_container_name_conflict(stderr: &str) -> bool {
    let lower = stderr.to_ascii_lowercase();
    lower.contains("already in use")
        && (lower.contains("container name")
            || lower.contains("the name \"")
            || lower.contains("the name '"))
}

/// No-op sandbox that passes through to direct execution.
pub struct NoSandbox;

#[async_trait]
impl Sandbox for NoSandbox {
    fn backend_id(&self) -> SandboxBackendId {
        SandboxBackendId::None
    }

    fn is_real(&self) -> bool {
        false
    }

    fn provides_fs_isolation(&self) -> bool {
        false
    }

    async fn ensure_ready(&self, _id: &SandboxId) -> Result<()> {
        Ok(())
    }

    async fn run_command(
        &self,
        _id: &SandboxId,
        command: &str,
        opts: &CommandOptions,
    ) -> Result<CommandOutput> {
        run_shell_command(command, opts).await
    }

    async fn read_file(
        &self,
        _id: &SandboxId,
        file_path: &str,
        max_bytes: u64,
    ) -> Result<SandboxReadResult> {
        native_host_read_file(file_path, max_bytes).await
    }

    async fn write_file(
        &self,
        _id: &SandboxId,
        file_path: &str,
        content: &[u8],
    ) -> Result<Option<serde_json::Value>> {
        native_host_write_file(file_path, content).await
    }

    async fn list_files(&self, _id: &SandboxId, root: &str) -> Result<SandboxListFilesResult> {
        native_host_list_files(root).await
    }

    async fn cleanup(&self, _id: &SandboxId) -> Result<()> {
        Ok(())
    }
}

/// Sysfs paths to mask with empty read-only tmpfs overlays.
///
/// On Linux, Docker shares the host kernel's sysfs.  Paths that don't exist
/// on the host (ARM devices without DMI, WSL2, etc.) would cause Docker to
/// fail with "mkdirat: read-only file system" when it tries to create
/// mountpoints on the read-only sysfs.  We probe each path and only mount
/// the ones that actually exist.
///
/// On non-Linux hosts (macOS), Docker Desktop runs in a Linux VM with full
/// sysfs, so all paths are included unconditionally — the host `/sys` layout
/// is irrelevant.
pub(crate) const SYSFS_MASK_PATHS: &[&str] = &[
    "/sys/firmware",
    "/sys/class/dmi",
    "/sys/devices/virtual/dmi",
    "/sys/class/block",
];

pub(crate) fn sysfs_paths_to_mask() -> Vec<&'static str> {
    static PATHS: OnceLock<Vec<&'static str>> = OnceLock::new();
    PATHS
        .get_or_init(|| {
            let paths = sysfs_paths_to_mask_from("/sys");
            let skipped = SYSFS_MASK_PATHS.len() - paths.len();
            if skipped > 0 {
                warn!(
                    skipped,
                    "some sysfs mask paths do not exist on this host and will be skipped"
                );
            }
            paths
        })
        .clone()
}

/// Testable inner helper: probes each `SYSFS_MASK_PATHS` entry and returns
/// only those that exist under `sysfs_root`.  If `sysfs_root` itself doesn't
/// exist (macOS), all paths are returned — Docker Desktop's VM will have them.
pub(crate) fn sysfs_paths_to_mask_from(sysfs_root: &str) -> Vec<&'static str> {
    let root = std::path::Path::new(sysfs_root);
    if !root.exists() {
        // Non-Linux host (macOS): Docker runs in a VM with full sysfs.
        return SYSFS_MASK_PATHS.to_vec();
    }
    SYSFS_MASK_PATHS
        .iter()
        .copied()
        .filter(|p| {
            // Strip the canonical "/sys/" prefix so the path is relative,
            // then probe under the supplied root (real or test tempdir).
            let rel = p.strip_prefix("/sys/").unwrap_or(p);
            root.join(rel).exists()
        })
        .collect()
}
