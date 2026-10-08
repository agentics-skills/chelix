use std::{collections::HashMap, path::PathBuf, pin::Pin, process::Stdio, time::Duration};

use {
    anyhow::anyhow,
    futures::{Stream, stream},
    tokio::{io::AsyncWriteExt, process::Command},
};

use crate::{
    transport::ExternalAgentSession,
    types::{ContextSnapshot, ExternalAgentEvent, ExternalAgentStatus},
};

#[allow(dead_code)]
pub struct OneShotProcessSession {
    binary: String,
    args: Vec<String>,
    env: HashMap<String, String>,
    working_dir: Option<PathBuf>,
    timeout: Duration,
    status: ExternalAgentStatus,
    cancel: tokio_util::sync::CancellationToken,
}

#[allow(dead_code)]
impl OneShotProcessSession {
    pub fn new(
        binary: String,
        args: Vec<String>,
        env: HashMap<String, String>,
        working_dir: Option<PathBuf>,
        timeout_secs: Option<u64>,
    ) -> Self {
        Self {
            binary,
            args,
            env,
            working_dir,
            timeout: Duration::from_secs(timeout_secs.unwrap_or(300)),
            status: ExternalAgentStatus::Idle,
            cancel: tokio_util::sync::CancellationToken::new(),
        }
    }
}

#[async_trait::async_trait]
impl ExternalAgentSession for OneShotProcessSession {
    fn external_session_id(&self) -> Option<&str> {
        None
    }

    async fn send_prompt(
        &mut self,
        prompt: &str,
        context: Option<&ContextSnapshot>,
    ) -> anyhow::Result<Pin<Box<dyn Stream<Item = ExternalAgentEvent> + Send>>> {
        self.status = ExternalAgentStatus::Running;
        let input = build_process_input(prompt, context);
        let mut command = Command::new(&self.binary);
        command.args(&self.args);
        if let Some(working_dir) = &self.working_dir {
            command.current_dir(working_dir);
        }
        command.envs(&self.env);
        command.stdin(Stdio::piped());
        command.stdout(Stdio::piped());
        command.stderr(Stdio::piped());
        command.kill_on_drop(true);

        if self.cancel.is_cancelled() {
            self.status = ExternalAgentStatus::Idle;
            return Ok(Box::pin(stream::iter(vec![
                ExternalAgentEvent::TurnInterrupted,
            ])));
        }
        let mut child = command.spawn()?;
        let cancel = self.cancel.clone();
        let write_cancelled = if let Some(mut stdin) = child.stdin.take() {
            tokio::select! {
                biased;
                () = cancel.cancelled() => true,
                result = async {
                    stdin.write_all(input.as_bytes()).await?;
                    stdin.shutdown().await?;
                    Ok::<(), std::io::Error>(())
                } => {
                    result?;
                    false
                }
            }
        } else {
            false
        };
        if write_cancelled {
            stop_child(&mut child, self.timeout).await?;
            self.status = ExternalAgentStatus::Idle;
            return Ok(Box::pin(stream::iter(vec![
                ExternalAgentEvent::TurnInterrupted,
            ])));
        }

        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| anyhow::anyhow!("external agent stdout missing"))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| anyhow::anyhow!("external agent stderr missing"))?;
        let mut stdout_reader = OutputReader::spawn(stdout);
        let mut stderr_reader = OutputReader::spawn(stderr);
        let cancel = self.cancel.clone();
        let status = tokio::select! {
            biased;
            () = cancel.cancelled() => {
                if let Err(error) = stop_child(&mut child, self.timeout).await {
                    stdout_reader.abort().await;
                    stderr_reader.abort().await;
                    return Err(error);
                }
                if let Err(error) = stdout_reader.read(self.timeout, CANCEL_READ_TIMEOUT).await {
                    stdout_reader.abort().await;
                    stderr_reader.abort().await;
                    return Err(error);
                }
                if let Err(error) = stderr_reader.read(self.timeout, CANCEL_READ_TIMEOUT).await {
                    stdout_reader.abort().await;
                    stderr_reader.abort().await;
                    return Err(error);
                }
                self.status = ExternalAgentStatus::Idle;
                return Ok(Box::pin(stream::iter(vec![ExternalAgentEvent::TurnInterrupted])));
            }
            status = tokio::time::timeout(self.timeout, child.wait()) => status,
        };
        let status = match status {
            Ok(Ok(status)) => status,
            Ok(Err(error)) => {
                stdout_reader.abort().await;
                stderr_reader.abort().await;
                return Err(anyhow!(error));
            },
            Err(_) => {
                stdout_reader.abort().await;
                stderr_reader.abort().await;
                return Err(anyhow!("external agent did not exit"));
            },
        };
        let stdout = match stdout_reader
            .read(self.timeout, "external agent output read timed out")
            .await
        {
            Ok(()) => stdout_reader.take_output(),
            Err(error) => {
                stdout_reader.abort().await;
                stderr_reader.abort().await;
                return Err(error);
            },
        };
        let stderr = match stderr_reader
            .read(self.timeout, "external agent output read timed out")
            .await
        {
            Ok(()) => stderr_reader.take_output(),
            Err(error) => {
                stdout_reader.abort().await;
                stderr_reader.abort().await;
                return Err(error);
            },
        };
        let output = std::process::Output {
            status,
            stdout,
            stderr,
        };
        self.status = ExternalAgentStatus::Idle;

        if output.status.success() {
            let text = String::from_utf8_lossy(&output.stdout).trim().to_string();
            let events = vec![
                ExternalAgentEvent::TextDelta(text),
                ExternalAgentEvent::Done { usage: None },
            ];
            Ok(Box::pin(stream::iter(events)))
        } else {
            let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
            let message = if stderr.is_empty() {
                format!("external agent exited with status {}", output.status)
            } else {
                stderr
            };
            Ok(Box::pin(stream::iter(vec![ExternalAgentEvent::Error(
                message,
            )])))
        }
    }

    async fn is_alive(&self) -> bool {
        true
    }

    async fn shutdown(&mut self) -> anyhow::Result<()> {
        self.status = ExternalAgentStatus::Stopped;
        Ok(())
    }

    fn status(&self) -> ExternalAgentStatus {
        self.status
    }

    fn arm_turn_cancel(&mut self, token: &tokio_util::sync::CancellationToken) {
        self.cancel = token.clone();
    }
}

pub(crate) fn build_process_input(prompt: &str, context: Option<&ContextSnapshot>) -> String {
    let Some(context) = context else {
        return prompt.to_string();
    };

    let mut parts = Vec::new();
    if let Some(working_dir) = &context.working_dir {
        parts.push(format!("Working directory: {}", working_dir.display()));
    }
    if let Some(instructions) = &context.system_instructions {
        parts.push(format!("System instructions:\n{instructions}"));
    }
    if let Some(summary) = &context.summary {
        parts.push(format!("Conversation summary:\n{summary}"));
    }
    if !context.recent_turns.is_empty() {
        let turns = context
            .recent_turns
            .iter()
            .map(|turn| format!("{}: {}", turn.role, turn.content))
            .collect::<Vec<_>>()
            .join("\n");
        parts.push(format!("Recent conversation:\n{turns}"));
    }
    if let Some(project_context) = &context.project_context {
        parts.push(format!("Project context:\n{project_context}"));
    }
    parts.push(format!("User prompt:\n{prompt}"));
    parts.join("\n\n")
}

pub(crate) const CANCEL_READ_TIMEOUT: &str = "external agent output read timed out after cancel";

pub(crate) async fn stop_child(
    child: &mut tokio::process::Child,
    timeout: Duration,
) -> anyhow::Result<()> {
    child
        .start_kill()
        .map_err(|error| anyhow!("external agent kill failed: {error}"))?;
    tokio::time::timeout(timeout, child.wait())
        .await
        .map_err(|_| anyhow!("external agent did not exit after cancel"))?
        .map_err(|error| anyhow!(error))?;
    Ok(())
}

pub(crate) struct OutputReader {
    handle: tokio::task::JoinHandle<std::io::Result<Vec<u8>>>,
    received: bool,
    output: Vec<u8>,
}

impl OutputReader {
    pub(crate) fn spawn(mut pipe: impl tokio::io::AsyncRead + Send + Unpin + 'static) -> Self {
        Self {
            handle: tokio::spawn(async move {
                let mut buf = Vec::new();
                tokio::io::AsyncReadExt::read_to_end(&mut pipe, &mut buf).await?;
                Ok(buf)
            }),
            received: false,
            output: Vec::new(),
        }
    }

    pub(crate) async fn read(
        &mut self,
        timeout: Duration,
        timeout_message: &str,
    ) -> anyhow::Result<()> {
        if self.received {
            return Ok(());
        }
        let joined = tokio::time::timeout(timeout, &mut self.handle).await;
        match joined {
            Ok(result) => {
                self.received = true;
                match result {
                    Ok(Ok(buf)) => {
                        self.output = buf;
                        Ok(())
                    },
                    Ok(Err(error)) => Err(anyhow!(error)),
                    Err(error) => Err(anyhow!(error)),
                }
            },
            Err(_) => Err(anyhow!("{timeout_message}")),
        }
    }

    pub(crate) fn take_output(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.output)
    }

    pub(crate) async fn abort(&mut self) {
        if self.received {
            return;
        }
        self.handle.abort();
        let joined = (&mut self.handle).await;
        self.received = true;
        let _ = joined;
    }
}

impl Drop for OutputReader {
    fn drop(&mut self) {
        if !self.received {
            self.handle.abort();
        }
    }
}

#[cfg(test)]
mod tests {
    use {super::*, crate::types::ContextTurn};

    #[test]
    fn process_input_includes_context_and_prompt() {
        let context = ContextSnapshot {
            system_instructions: Some("Be concise".to_string()),
            recent_turns: vec![ContextTurn {
                role: "user".to_string(),
                content: "previous".to_string(),
            }],
            project_context: Some("project details".to_string()),
            ..ContextSnapshot::default()
        };

        let input = build_process_input("next", Some(&context));

        assert!(input.contains("System instructions:\nBe concise"));
        assert!(input.contains("user: previous"));
        assert!(input.contains("Project context:\nproject details"));
        assert!(input.contains("User prompt:\nnext"));
    }

    #[test]
    fn process_input_without_context_is_prompt_only() {
        assert_eq!(build_process_input("hello", None), "hello");
    }

    #[tokio::test]
    async fn cancel_during_stdin_write_returns_turn_interrupted() -> anyhow::Result<()> {
        use futures::StreamExt;

        let unique = std::time::SystemTime::now()
            .duration_since(std::time::SystemTime::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("chelix-process-cancel-{unique}"));
        std::fs::create_dir_all(&dir)?;
        let script = dir.join("sleep.sh");
        let pidfile = dir.join("pid");
        std::fs::write(
            &script,
            format!("#!/bin/sh\necho $$ > {}\nsleep 30\n", pidfile.display()),
        )?;
        let mut session = OneShotProcessSession::new(
            "/bin/sh".to_string(),
            vec![script.to_string_lossy().to_string()],
            HashMap::new(),
            None,
            Some(5),
        );
        let cancel = tokio_util::sync::CancellationToken::new();
        session.arm_turn_cancel(&cancel);
        let prompt = "x".repeat(256 * 1024);
        let send = tokio::spawn(async move { session.send_prompt(&prompt, None).await });
        let started = std::time::Instant::now();
        while !pidfile.exists() {
            if started.elapsed() > Duration::from_secs(2) {
                anyhow::bail!("process test did not start");
            }
            tokio::task::yield_now().await;
        }
        cancel.cancel();
        let events = tokio::time::timeout(Duration::from_secs(2), send)
            .await???
            .collect::<Vec<_>>()
            .await;
        assert!(matches!(
            events.first(),
            Some(ExternalAgentEvent::TurnInterrupted)
        ));
        let pid = std::fs::read_to_string(&pidfile)?;
        let status = std::process::Command::new("kill")
            .args(["-0", pid.trim()])
            .status()?;
        assert!(!status.success());
        std::fs::remove_dir_all(dir)?;
        Ok(())
    }

    #[tokio::test]
    async fn cancel_times_out_when_a_grandchild_holds_stderr() -> anyhow::Result<()> {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::SystemTime::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("chelix-process-stderr-{unique}"));
        std::fs::create_dir_all(&dir)?;
        let ready = dir.join("ready");
        let script = dir.join("hold.sh");
        std::fs::write(
            &script,
            format!(
                "#!/bin/sh\ncat >/dev/null\nexec 1>&-\nsleep 60 >&2 &\necho ready > {}\nwait\n",
                ready.display()
            ),
        )?;
        let mut session = OneShotProcessSession::new(
            "/bin/sh".to_string(),
            vec![script.to_string_lossy().to_string()],
            HashMap::new(),
            None,
            Some(1),
        );
        let cancel = tokio_util::sync::CancellationToken::new();
        session.arm_turn_cancel(&cancel);
        let send = tokio::spawn(async move { session.send_prompt("hello", None).await });
        let started = std::time::Instant::now();
        while !ready.exists() {
            if started.elapsed() > Duration::from_secs(2) {
                anyhow::bail!("process grandchild was not ready");
            }
            tokio::task::yield_now().await;
        }
        cancel.cancel();
        let error = match tokio::time::timeout(Duration::from_secs(3), send).await?? {
            Err(error) => error,
            Ok(_) => anyhow::bail!("expected read timeout"),
        };
        assert!(
            error
                .to_string()
                .contains("external agent output read timed out after cancel")
        );
        std::fs::remove_dir_all(dir)?;
        Ok(())
    }

    struct Probe {
        started: std::sync::Arc<tokio::sync::Notify>,
        dropped: std::sync::Arc<tokio::sync::Notify>,
        entered: bool,
    }

    impl Drop for Probe {
        fn drop(&mut self) {
            self.dropped.notify_one();
        }
    }

    impl tokio::io::AsyncRead for Probe {
        fn poll_read(
            mut self: Pin<&mut Self>,
            _cx: &mut std::task::Context<'_>,
            _buf: &mut tokio::io::ReadBuf<'_>,
        ) -> std::task::Poll<std::io::Result<()>> {
            if !self.entered {
                self.entered = true;
                self.started.notify_one();
            }
            std::task::Poll::Pending
        }
    }

    #[tokio::test]
    async fn output_reader_drop_drops_the_pending_read() -> anyhow::Result<()> {
        let started = std::sync::Arc::new(tokio::sync::Notify::new());
        let dropped = std::sync::Arc::new(tokio::sync::Notify::new());
        let entered = started.notified();
        let finished = dropped.notified();
        let reader = OutputReader::spawn(Probe {
            started: std::sync::Arc::clone(&started),
            dropped: std::sync::Arc::clone(&dropped),
            entered: false,
        });
        tokio::time::timeout(Duration::from_secs(1), entered)
            .await
            .map_err(|_| anyhow!("reader did not start"))?;
        drop(reader);
        tokio::time::timeout(Duration::from_secs(1), finished)
            .await
            .map_err(|_| anyhow!("reader resource was not dropped"))?;
        Ok(())
    }
}
