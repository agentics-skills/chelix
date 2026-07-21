use std::{path::Path, time::Duration};

use {
    anyhow::{Context, Result, anyhow, bail},
    tokio::process::Command,
};

const COMMAND_TIMEOUT: Duration = Duration::from_secs(10);
const SOCKET_PREFIX: &str = "chelix-tools";

#[derive(Debug)]
pub(crate) struct CommandOutput {
    pub(crate) stdout: String,
    pub(crate) stderr: String,
    pub(crate) exit_code: i32,
}

/// Dedicated tmux server owned by one `chelix-tools-service` process.
///
/// Every operation uses the same unique socket name. This prevents managed
/// terminals from observing or mutating the user's default tmux server.
pub(crate) struct TmuxRuntime {
    socket_name: String,
}

impl TmuxRuntime {
    pub(crate) fn new() -> Self {
        Self {
            socket_name: format!("{SOCKET_PREFIX}-{}", uuid::Uuid::new_v4().simple()),
        }
    }

    pub(crate) async fn run(&self, args: &[String]) -> Result<CommandOutput> {
        let mut command = Command::new("tmux");
        command
            .arg("-L")
            .arg(&self.socket_name)
            .args(args)
            .kill_on_drop(true);
        let output = tokio::time::timeout(COMMAND_TIMEOUT, command.output())
            .await
            .map_err(|_| {
                anyhow!(
                    "tmux command timed out after {}s",
                    COMMAND_TIMEOUT.as_secs()
                )
            })?
            .context("failed to start tmux")?;
        Ok(CommandOutput {
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
            exit_code: output
                .status
                .code()
                .ok_or_else(|| anyhow!("tmux process terminated without an exit code"))?,
        })
    }

    pub(crate) async fn shutdown(&self) -> Result<()> {
        let output = self.run(&["kill-server".into()]).await?;
        if output.exit_code == 0 || is_no_server(&command_error(&output)) {
            return Ok(());
        }
        bail!(
            "failed to stop managed tmux server: {}",
            command_error(&output)
        );
    }

    pub(crate) fn attach_command(
        &self,
        session_name: &str,
        window_id: &str,
        pane_id: &str,
    ) -> portable_pty::CommandBuilder {
        let mut command = portable_pty::CommandBuilder::new("tmux");
        command.args([
            "-L",
            &self.socket_name,
            "attach-session",
            "-t",
            &attach_target(session_name, window_id, pane_id),
        ]);
        command.env("TERM", "xterm-256color");
        command.env("COLORTERM", "truecolor");
        command
    }
}

fn attach_target(session_name: &str, window_id: &str, pane_id: &str) -> String {
    format!("{session_name}:{window_id}.{pane_id}")
}

pub(crate) async fn verify_runtime(default_working_dir: &Path) -> Result<()> {
    let metadata = tokio::fs::metadata(default_working_dir)
        .await
        .with_context(|| {
            format!(
                "tools service working directory is unavailable: {}",
                default_working_dir.display()
            )
        })?;
    if !metadata.is_dir() {
        bail!(
            "tools service working directory is not a directory: {}",
            default_working_dir.display()
        );
    }
    let output = Command::new("tmux")
        .arg("-V")
        .output()
        .await
        .context("tmux is required by chelix-tools-service")?;
    if !output.status.success() {
        bail!(
            "tmux availability check failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(())
}

pub(crate) fn command_error(output: &CommandOutput) -> String {
    if !output.stderr.trim().is_empty() {
        output.stderr.trim().into()
    } else if !output.stdout.trim().is_empty() {
        output.stdout.trim().into()
    } else {
        format!("exit {}", output.exit_code)
    }
}

pub(crate) fn is_no_server(message: &str) -> bool {
    let lower = message.to_ascii_lowercase();
    lower.contains("no server running")
        || lower.contains("no sessions")
        || lower.contains("error connecting to") && lower.contains("no such file")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runtime_socket_names_are_unique_and_service_scoped() {
        let first = TmuxRuntime::new();
        let second = TmuxRuntime::new();

        assert!(first.socket_name.starts_with(SOCKET_PREFIX));
        assert!(second.socket_name.starts_with(SOCKET_PREFIX));
        assert_ne!(first.socket_name, second.socket_name);
    }

    #[test]
    fn no_server_errors_are_recognized() {
        assert!(is_no_server("no server running on /tmp/tmux-501/default"));
        assert!(is_no_server(
            "error connecting to /tmp/tmux.sock (No such file or directory)"
        ));
        assert!(!is_no_server("permission denied"));
    }

    #[test]
    fn attach_target_selects_exact_managed_pane() {
        assert_eq!(
            attach_target("chelix-agent", "@2", "%3"),
            "chelix-agent:@2.%3"
        );
    }
}
