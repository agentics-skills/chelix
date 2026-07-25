//! Tool implementations and policy enforcement — sandbox subsystem.
//!
//! Split into submodules by domain for maintainability.

#[cfg(target_os = "macos")]
pub(crate) mod apple;
pub(crate) mod containers;
pub(crate) mod docker;
pub mod env;
pub(crate) mod file_system;
pub(crate) mod paths;
pub(crate) mod provision;
pub mod router;
pub(crate) mod sync;
pub(crate) mod types;

#[cfg(test)]
mod tests;

// ── Re-exports (preserves the existing public API) ───────────────────────────

#[cfg(target_os = "macos")]
pub use apple::{AppleContainerSandbox, ensure_apple_container_service};
pub use {
    containers::{
        ContainerBackend, ContainerDiskUsage, ContainerRunState, RunningContainer, SandboxImage,
        clean_all_containers, clean_sandbox_images, container_cli, container_disk_usage,
        current_sandbox_image_tag, is_cli_available, list_running_containers, list_sandbox_images,
        remove_container, remove_sandbox_image, restart_container_daemon, sandbox_image_tag,
        stop_container,
    },
    docker::{DockerSandbox, NoSandbox},
    env::ExecEnv,
    paths::shared_home_dir_path,
    router::{SandboxEvent, SandboxRouter, create_sandbox},
    types::{
        BuildImageResult, DEFAULT_SANDBOX_IMAGE, HomePersistence, ResourceLimits, Sandbox,
        SandboxBackend, SandboxBackendId, SandboxConfig, SandboxId, SandboxMode, SandboxScope,
        ToolsServiceEndpoint, ToolsServiceInstance,
    },
};
