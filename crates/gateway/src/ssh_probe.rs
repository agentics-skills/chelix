use std::{io::Write, path::Path, time::Duration};

use {
    anyhow::Result,
    secrecy::ExposeSecret,
    tokio::{io::AsyncReadExt, process::Command},
};

use crate::auth::{CredentialStore, SshAuthMode, SshResolvedTarget};

pub const PROBE_MARKER: &str = "__chelix_ssh_probe__";
const PROBE_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_OUTPUT_BYTES: usize = 8 * 1024;

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct SshProbeResult {
    pub stdout: String,
    pub stderr: String,
    pub exit_code: i32,
}

pub async fn probe_target(
    credential_store: &CredentialStore,
    target: &SshResolvedTarget,
) -> Result<SshProbeResult> {
    match target.auth_mode {
        SshAuthMode::System => run_probe(target, None).await,
        SshAuthMode::Managed => {
            let key_id = target
                .key_id
                .ok_or_else(|| anyhow::anyhow!("managed ssh target has no key configured"))?;
            let private_key = credential_store
                .get_ssh_private_key(key_id)
                .await?
                .ok_or_else(|| anyhow::anyhow!("ssh key {key_id} not found"))?;
            let mut key_file = tempfile::NamedTempFile::new()?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(key_file.path(), std::fs::Permissions::from_mode(0o600))?;
            }
            key_file.write_all(private_key.expose_secret().as_bytes())?;
            key_file.flush()?;
            run_probe(target, Some(key_file.path())).await
        },
    }
}

async fn run_probe(
    target: &SshResolvedTarget,
    identity_file: Option<&Path>,
) -> Result<SshProbeResult> {
    let known_hosts_file = if let Some(known_host) = target.known_host.as_deref() {
        let mut file = tempfile::NamedTempFile::new()?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(file.path(), std::fs::Permissions::from_mode(0o600))?;
        }
        file.write_all(known_host.as_bytes())?;
        file.write_all(b"\n")?;
        file.flush()?;
        Some(file)
    } else {
        None
    };

    let mut ssh = Command::new("ssh");
    ssh.arg("-T")
        .arg("-o")
        .arg("BatchMode=yes")
        .arg("-o")
        .arg("ConnectTimeout=10");
    if let Some(known_hosts_file) = known_hosts_file.as_ref() {
        ssh.arg("-o")
            .arg("StrictHostKeyChecking=yes")
            .arg("-o")
            .arg(format!(
                "UserKnownHostsFile={}",
                ssh_config_quote_path(known_hosts_file.path())
            ))
            .arg("-o")
            .arg("GlobalKnownHostsFile=/dev/null");
    }
    if let Some(identity_file) = identity_file {
        ssh.arg("-o")
            .arg("IdentitiesOnly=yes")
            .arg("-i")
            .arg(identity_file);
    }
    if let Some(port) = target.port {
        ssh.arg("-p").arg(port.to_string());
    }
    ssh.arg("--")
        .arg(&target.target)
        .arg(format!("printf '%s' {PROBE_MARKER}"))
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());

    let mut child = ssh.spawn()?;
    let stdout_task = child.stdout.take().map(read_pipe);
    let stderr_task = child.stderr.take().map(read_pipe);
    let status = match tokio::time::timeout(PROBE_TIMEOUT, child.wait()).await {
        Ok(status) => status?,
        Err(_) => {
            let _ = child.start_kill();
            let _ = child.wait().await;
            let _ = join_pipe(stdout_task).await;
            let _ = join_pipe(stderr_task).await;
            anyhow::bail!("ssh probe timed out after 10s");
        },
    };

    let mut stdout = String::from_utf8_lossy(&join_pipe(stdout_task).await?).into_owned();
    let mut stderr = String::from_utf8_lossy(&join_pipe(stderr_task).await?).into_owned();
    truncate_output(&mut stdout);
    truncate_output(&mut stderr);

    Ok(SshProbeResult {
        stdout,
        stderr,
        exit_code: status.code().unwrap_or(-1),
    })
}

fn read_pipe<R>(mut reader: R) -> tokio::task::JoinHandle<std::io::Result<Vec<u8>>>
where
    R: tokio::io::AsyncRead + Unpin + Send + 'static,
{
    tokio::spawn(async move {
        let mut bytes = Vec::new();
        reader.read_to_end(&mut bytes).await?;
        Ok(bytes)
    })
}

async fn join_pipe(
    task: Option<tokio::task::JoinHandle<std::io::Result<Vec<u8>>>>,
) -> Result<Vec<u8>> {
    match task {
        Some(task) => Ok(task.await??),
        None => Ok(Vec::new()),
    }
}

fn truncate_output(output: &mut String) {
    if output.len() <= MAX_OUTPUT_BYTES {
        return;
    }
    output.truncate(output.floor_char_boundary(MAX_OUTPUT_BYTES));
    output.push_str("\n... [output truncated]");
}

fn ssh_config_quote_path(path: &Path) -> String {
    let value = path.to_string_lossy();
    format!("\"{}\"", value.replace('\\', "\\\\").replace('"', "\\\""))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn output_is_truncated_on_a_character_boundary() {
        let mut output = "é".repeat(MAX_OUTPUT_BYTES);
        truncate_output(&mut output);
        assert!(output.ends_with("... [output truncated]"));
        assert!(output.is_char_boundary(output.len()));
    }

    #[test]
    fn paths_are_quoted_for_ssh_config() {
        let quoted = ssh_config_quote_path(Path::new("/tmp/key \"one\""));
        assert_eq!(quoted, "\"/tmp/key \\\"one\\\"\"");
    }
}
