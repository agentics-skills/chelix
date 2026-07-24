use std::{borrow::Cow, path::PathBuf, sync::Arc, time::Duration};

use {
    async_trait::async_trait,
    secrecy::{ExposeSecret, Secret},
    serde::{Deserialize, Serialize},
    tokio::process::Command,
    tracing::{debug, warn},
};

use crate::{Result, error::Error};

/// Event describing a completed command invocation.
#[derive(Debug, Clone)]
pub struct CommandCompletionEvent {
    pub command: String,
    pub exit_code: i32,
    pub stdout_preview: String,
    pub stderr_preview: String,
}

/// Callback fired after command completion.
pub type CommandCompletionFn = Arc<dyn Fn(CommandCompletionEvent) + Send + Sync>;

/// Provider of environment variables to inject at command execution boundaries.
/// Values are wrapped in `Secret` to prevent accidental logging.
#[derive(Debug, Clone)]
pub struct InjectedEnvVar {
    pub key: String,
    pub value: Secret<String>,
    pub secret: bool,
}

#[async_trait]
pub trait EnvVarProvider: Send + Sync {
    async fn get_env_vars(&self) -> anyhow::Result<Vec<InjectedEnvVar>>;
}

/// Result of a shell command execution.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommandOutput {
    pub stdout: String,
    pub stderr: String,
    pub exit_code: i32,
}

/// Controls whether command text may be emitted to diagnostic logs.
#[derive(Debug, Clone, Default)]
pub enum CommandLogPolicy {
    #[default]
    Visible,
    RedactSecrets(Vec<Secret<String>>),
    Replacement(String),
}

impl CommandLogPolicy {
    #[must_use]
    pub fn redact_secrets(values: impl IntoIterator<Item = Secret<String>>) -> Self {
        let values = values
            .into_iter()
            .filter(|value| !value.expose_secret().is_empty())
            .collect::<Vec<_>>();
        if values.is_empty() {
            Self::Visible
        } else {
            Self::RedactSecrets(values)
        }
    }

    #[must_use]
    pub fn replacement(command: impl Into<String>) -> Self {
        Self::Replacement(command.into())
    }

    #[must_use]
    pub fn for_log<'command>(&self, command: &'command str) -> Cow<'command, str> {
        match self {
            Self::Visible => Cow::Borrowed(command),
            Self::RedactSecrets(values) => {
                Cow::Owned(values.iter().fold(command.to_string(), |redacted, value| {
                    redaction_needles(value.expose_secret())
                        .into_iter()
                        .fold(redacted, |text, needle| text.replace(&needle, "[REDACTED]"))
                }))
            },
            Self::Replacement(command) => Cow::Owned(command.clone()),
        }
    }
}

/// Options controlling command execution behavior.
#[derive(Debug, Clone)]
pub struct CommandOptions {
    pub timeout: Duration,
    pub max_output_bytes: usize,
    pub working_dir: Option<PathBuf>,
    pub env: Vec<(String, String)>,
    pub log_policy: CommandLogPolicy,
}

impl Default for CommandOptions {
    fn default() -> Self {
        Self {
            timeout: Duration::from_secs(30),
            max_output_bytes: 200 * 1024,
            working_dir: None,
            env: Vec::new(),
            log_policy: CommandLogPolicy::default(),
        }
    }
}

pub fn truncate_output_for_display(output: &mut String, max_output_bytes: usize) {
    if output.len() <= max_output_bytes {
        return;
    }
    output.truncate(output.floor_char_boundary(max_output_bytes));
    output.push_str("\n... [output truncated]");
}

/// Redact injected environment variable values from command output.
pub fn redact_command_output(result: &mut CommandOutput, env: &[(String, String)]) {
    let values = env
        .iter()
        .map(|(_, value)| value.clone())
        .collect::<Vec<_>>();
    redact_secret_values(&mut result.stdout, &values);
    redact_secret_values(&mut result.stderr, &values);
}

/// Replace secret values and supported encoded derivatives in text.
pub fn redact_secret_values(text: &mut String, values: &[String]) {
    for value in values {
        if value.is_empty() {
            continue;
        }
        for needle in redaction_needles(value) {
            *text = text.replace(&needle, "[REDACTED]");
        }
    }
}

fn redaction_needles(value: &str) -> Vec<String> {
    use base64::Engine;

    let mut needles = vec![value.to_string()];

    let b64_std = base64::engine::general_purpose::STANDARD.encode(value.as_bytes());
    let b64_url = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(value.as_bytes());
    if b64_std != value {
        needles.push(b64_std);
    }
    if b64_url != value {
        needles.push(b64_url);
    }

    let hex = value
        .as_bytes()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    if hex != value {
        needles.push(hex);
    }

    needles
}

/// Execute a shell command with timeout and output limits.
#[tracing::instrument(
    skip(command, opts),
    fields(
        command = %opts.log_policy.for_log(command),
        timeout_secs = opts.timeout.as_secs()
    )
)]
pub async fn run_shell_command(command: &str, opts: &CommandOptions) -> Result<CommandOutput> {
    debug!(
        command = %opts.log_policy.for_log(command),
        timeout_secs = opts.timeout.as_secs(),
        "run_shell_command"
    );

    let mut cmd = Command::new("bash");
    cmd.arg("-c").arg(command);

    if let Some(ref dir) = opts.working_dir {
        cmd.current_dir(dir);
    }
    for (key, value) in &opts.env {
        cmd.env(key, value);
    }

    cmd.stdout(std::process::Stdio::piped());
    cmd.stderr(std::process::Stdio::piped());
    cmd.stdin(std::process::Stdio::null());

    let child = cmd.spawn().map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            if let Some(ref dir) = opts.working_dir {
                Error::message(format!(
                    "failed to start command: working directory '{}' does not exist",
                    dir.display()
                ))
            } else {
                Error::message("failed to start command: shell 'bash' not found")
            }
        } else {
            Error::message(format!("failed to start command: {error}"))
        }
    })?;

    match tokio::time::timeout(opts.timeout, child.wait_with_output()).await {
        Ok(Ok(output)) => {
            let mut stdout = String::from_utf8_lossy(&output.stdout).into_owned();
            let mut stderr = String::from_utf8_lossy(&output.stderr).into_owned();

            truncate_output_for_display(&mut stdout, opts.max_output_bytes);
            truncate_output_for_display(&mut stderr, opts.max_output_bytes);

            let exit_code = output.status.code().unwrap_or(-1);
            debug!(
                exit_code,
                stdout_len = stdout.len(),
                stderr_len = stderr.len(),
                "command complete"
            );

            Ok(CommandOutput {
                stdout,
                stderr,
                exit_code,
            })
        },
        Ok(Err(error)) => Err(Error::message(format!("failed to run command: {error}"))),
        Err(_) => {
            warn!(
                command = %opts.log_policy.for_log(command),
                "command timeout"
            );
            Err(Error::message(format!(
                "command timed out after {}s",
                opts.timeout.as_secs()
            )))
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn run_shell_command_captures_stdout() {
        let result = run_shell_command("echo hello", &CommandOptions::default())
            .await
            .unwrap();
        assert_eq!(result.stdout.trim(), "hello");
        assert_eq!(result.exit_code, 0);
    }

    #[tokio::test]
    async fn run_shell_command_captures_stderr() {
        let result = run_shell_command("echo err >&2", &CommandOptions::default())
            .await
            .unwrap();
        assert_eq!(result.stderr.trim(), "err");
    }

    #[tokio::test]
    async fn run_shell_command_returns_exit_code() {
        let result = run_shell_command("exit 42", &CommandOptions::default())
            .await
            .unwrap();
        assert_eq!(result.exit_code, 42);
    }

    #[tokio::test]
    async fn run_shell_command_times_out() {
        let opts = CommandOptions {
            timeout: Duration::from_millis(100),
            ..Default::default()
        };
        let result = run_shell_command("sleep 10", &opts).await;
        assert!(result.is_err());
    }

    #[test]
    fn truncate_output_handles_multibyte_boundary() {
        let mut output = format!("{}л{}", "a".repeat(1999), "z".repeat(10));
        truncate_output_for_display(&mut output, 2000);
        assert!(output.contains("[output truncated]"));
        assert!(!output.contains('л'));
    }

    #[tokio::test]
    async fn run_shell_command_reports_bad_working_dir() {
        let opts = CommandOptions {
            working_dir: Some(PathBuf::from("/definitely/not/a/real/path")),
            ..Default::default()
        };
        let err = run_shell_command("echo hello", &opts).await.unwrap_err();
        assert!(err.to_string().contains("working directory"));
    }

    #[test]
    fn command_log_policy_redacts_only_secret_values() {
        let command = "printf command-remains-visible super-secret-value";
        let policy =
            CommandLogPolicy::redact_secrets([Secret::new("super-secret-value".to_string())]);

        assert_eq!(
            policy.for_log(command),
            "printf command-remains-visible [REDACTED]"
        );
        assert_eq!(CommandLogPolicy::Visible.for_log(command), command);
    }

    #[test]
    fn redact_command_output_removes_secret_encodings() {
        use base64::Engine;

        let secret = "sensitive-token";
        let mut result = CommandOutput {
            stdout: format!(
                "{secret} {} 73656e7369746976652d746f6b656e",
                base64::engine::general_purpose::STANDARD.encode(secret.as_bytes())
            ),
            stderr: base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(secret.as_bytes()),
            exit_code: 0,
        };

        redact_command_output(&mut result, &[("TOKEN".to_string(), secret.to_string())]);

        assert!(!result.stdout.contains(secret));
        assert!(!result.stderr.contains(secret));
        assert!(result.stdout.contains("[REDACTED]"));
        assert!(result.stderr.contains("[REDACTED]"));
    }
}
