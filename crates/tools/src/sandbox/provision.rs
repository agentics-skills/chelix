//! Package provisioning inside isolated containers.

use tracing::{debug, info, warn};

use {
    super::{containers::container_exec_shell_args, types::tail_lines},
    crate::error::Result,
};

/// Install configured packages inside a container via `apt-get`.
pub(crate) async fn provision_packages(
    cli: &str,
    container_name: &str,
    packages: &[String],
) -> Result<()> {
    if packages.is_empty() {
        return Ok(());
    }
    let package_list = packages.join(" ");
    info!(container = container_name, packages = %package_list, "provisioning sandbox packages");
    let output = tokio::process::Command::new(cli)
        .args(container_exec_shell_args(
            cli,
            container_name,
            format!("apt-get update -qq && apt-get install -y -qq {package_list}"),
        ))
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output()
        .await?;
    if output.status.success() {
        let stdout = String::from_utf8_lossy(&output.stdout);
        if !stdout.trim().is_empty() {
            let tail = tail_lines(&stdout, 20);
            debug!(container = container_name, output = %tail, "package provisioning output");
        }
    } else {
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        warn!(
            container = container_name,
            exit_code = output.status.code().unwrap_or(-1),
            stdout = %tail_lines(&stdout, 20),
            stderr = %stderr.trim(),
            "package provisioning failed (non-fatal)"
        );
    }
    Ok(())
}
