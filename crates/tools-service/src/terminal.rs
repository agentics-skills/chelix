use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

use {
    anyhow::{Context, Result, anyhow, bail},
    chelix_protocol::{
        ExecuteCommandRequest, ExecuteCommandResponse, ReadTerminalOutputRequest,
        ReadTerminalOutputResponse, ToolsServiceEnvVar, ToolsServiceTerminalInfo,
        ToolsServiceTerminalKind,
    },
    tokio::sync::{Mutex, RwLock, Semaphore},
};

use crate::tmux::{TmuxRuntime, command_error, is_no_server};

const DEFAULT_CAPTURE_LINES: usize = 1_000;
const MAX_CAPTURE_LINES: usize = 20_000;
const DEFAULT_TMUX_COLS: u16 = 200;
const DEFAULT_TMUX_ROWS: u16 = 50;
const FIELD_SEPARATOR: &str = "|chelix-tmux-field|";
const START_PREFIX: &str = "__CHELIX_COMMAND_START__";
const DONE_PREFIX: &str = "__CHELIX_COMMAND_DONE__";
const PIPE_FLUSHED: &str = "flushed";
const PIPE_FLUSH_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug, Clone)]
struct TmuxPaneInfo {
    session_id: String,
    session_name: String,
    window_id: String,
    window_index: u32,
    window_name: String,
    window_active: bool,
    pane_id: String,
    pane_index: u32,
    pane_active: bool,
}

#[derive(Debug, Clone)]
struct ManagedRun {
    run_id: String,
    command: String,
    baseline_lines: usize,
    marker_enabled: bool,
    completed: bool,
    exit_code: Option<i32>,
    secret_values: Vec<String>,
    capture: Arc<RunCapture>,
}

#[derive(Debug)]
struct RunCapture {
    output_path: PathBuf,
    completion_path: PathBuf,
    pending_completion_path: PathBuf,
    state: Mutex<RunCaptureState>,
}

#[derive(Debug)]
struct RunCaptureState {
    pipe_open: bool,
    completion_verified: bool,
}

#[derive(Debug, Clone)]
struct ManagedTerminal {
    id: String,
    session_key: String,
    session_id: String,
    session_name: String,
    window_id: String,
    window_name: String,
    pane_id: String,
    gate: Arc<Semaphore>,
    last_run: Option<ManagedRun>,
}

impl ManagedTerminal {
    fn is_running(&self) -> bool {
        self.last_run.as_ref().is_some_and(|run| !run.completed)
    }
}

#[derive(Debug)]
struct CaptureResult {
    output: String,
    completed: bool,
    exit_code: Option<i32>,
}

pub struct TerminalManager {
    default_working_dir: PathBuf,
    runtime: Arc<TmuxRuntime>,
    terminals: RwLock<HashMap<String, ManagedTerminal>>,
    allocation_gate: Semaphore,
    next_id: AtomicU64,
    capture_dir: Mutex<Option<tempfile::TempDir>>,
}

impl TerminalManager {
    pub fn new(default_working_dir: PathBuf, runtime: Arc<TmuxRuntime>) -> Result<Self> {
        let capture_dir = tempfile::Builder::new()
            .prefix("chelix-tools-service-")
            .tempdir()
            .context("creating terminal capture directory")?;
        Ok(Self {
            default_working_dir,
            runtime,
            terminals: RwLock::new(HashMap::new()),
            allocation_gate: Semaphore::new(1),
            next_id: AtomicU64::new(1),
            capture_dir: Mutex::new(Some(capture_dir)),
        })
    }

    pub async fn execute_command(
        &self,
        request: ExecuteCommandRequest,
    ) -> Result<ExecuteCommandResponse> {
        validate_execute_request(&request)?;
        let custom_cwd = match request.custom_cwd.as_deref() {
            Some(cwd) if !cwd.trim().is_empty() => Some(self.require_working_dir(cwd).await?),
            _ => None,
        };
        let command = request.command.trim().to_string();
        let mut allocation_permit = self
            .allocation_gate
            .acquire()
            .await
            .map_err(|_| anyhow!("terminal allocation gate closed"))?;
        let terminal = self
            .resolve_terminal_inner(
                &request.session_key,
                request.terminal_id.as_deref(),
                request.new_terminal,
                custom_cwd.as_deref(),
            )
            .await?;
        let terminal_gate = terminal.gate.clone();
        let permit = match terminal_gate.clone().try_acquire_owned() {
            Ok(permit) => permit,
            Err(tokio::sync::TryAcquireError::NoPermits) => {
                drop(allocation_permit);
                let permit = terminal_gate
                    .acquire_owned()
                    .await
                    .map_err(|_| anyhow!("terminal gate closed"))?;
                allocation_permit = self
                    .allocation_gate
                    .acquire()
                    .await
                    .map_err(|_| anyhow!("terminal allocation gate closed"))?;
                permit
            },
            Err(tokio::sync::TryAcquireError::Closed) => bail!("terminal gate closed"),
        };
        let terminal = self.refresh_completion(terminal).await?;
        if terminal.is_running() {
            bail!(
                "terminal {} is still running a command; use read_terminal_output or newTerminal=true",
                terminal.id
            );
        }

        let run_id = uuid::Uuid::new_v4().simple().to_string();
        self.wait_for_terminal_ready(&terminal).await?;
        if let Some(previous) = terminal.last_run.as_ref() {
            self.remove_run_capture(&terminal, previous).await?;
        }
        let baseline_lines = self
            .capture_pane(&terminal, MAX_CAPTURE_LINES)
            .await
            .unwrap_or_default()
            .lines()
            .count();
        let payload = build_paste_payload(&command, custom_cwd.as_deref(), &run_id, &request.env)?;
        let capture = self.start_run_capture(&terminal).await?;
        if let Err(paste_error) = self.paste_text(&terminal, &payload).await {
            let cleanup_result = self.remove_capture_files(&terminal, &capture).await;
            return match cleanup_result {
                Ok(()) => Err(paste_error),
                Err(cleanup_error) => Err(anyhow!(
                    "failed to paste command: {paste_error:#}; terminal capture cleanup also failed: {cleanup_error:#}"
                )),
            };
        }

        let mut updated = terminal.clone();
        updated.last_run = Some(ManagedRun {
            run_id: run_id.clone(),
            command: command.clone(),
            baseline_lines,
            marker_enabled: true,
            completed: false,
            exit_code: None,
            secret_values: request
                .env
                .iter()
                .filter(|variable| variable.secret && !variable.value.is_empty())
                .map(|variable| variable.value.clone())
                .collect(),
            capture,
        });
        self.store_terminal(updated.clone()).await;
        drop(allocation_permit);

        if request.background {
            drop(permit);
            return Ok(execute_response(
                updated,
                run_id,
                String::new(),
                None,
                false,
                false,
                true,
                "Command started in sandbox tmux terminal".into(),
            ));
        }

        let deadline = tokio::time::Instant::now() + Duration::from_millis(request.timeout_millis);
        let run = updated
            .last_run
            .as_ref()
            .ok_or_else(|| anyhow!("terminal run state disappeared"))?;
        let capture = loop {
            let capture = self.read_run_capture(run).await?;
            if capture.completed || tokio::time::Instant::now() >= deadline {
                break capture;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        };
        let capture = if capture.completed {
            let capture = self.finalize_run_capture(&updated, run).await?;
            self.mark_run_completed(&updated.id, capture.exit_code)
                .await;
            capture
        } else {
            capture
        };
        drop(permit);

        let timed_out = !capture.completed;
        let mut output = capture.output;
        redact_text(&mut output, updated.last_run.as_ref());
        let message = if timed_out {
            format!(
                "Command still running in sandbox tmux terminal (id: {}) after {}ms",
                updated.id, request.timeout_millis
            )
        } else {
            format!(
                "Command finished in sandbox tmux terminal (id: {})",
                updated.id
            )
        };

        Ok(execute_response(
            updated,
            run_id,
            output,
            capture.exit_code,
            capture.completed,
            timed_out,
            false,
            message,
        ))
    }

    pub async fn read_terminal_output(
        &self,
        request: ReadTerminalOutputRequest,
    ) -> Result<ReadTerminalOutputResponse> {
        if request.session_key.trim().is_empty() {
            bail!("session_key cannot be empty");
        }
        if request.terminal_id.trim().is_empty() {
            bail!("terminal_id cannot be empty");
        }
        let terminal = self
            .lookup_terminal(&request.session_key, &request.terminal_id)
            .await?;
        let terminal = self.refresh_completion(terminal).await?;
        let max_lines = request
            .max_lines
            .unwrap_or(DEFAULT_CAPTURE_LINES)
            .clamp(1, MAX_CAPTURE_LINES);
        let (raw, capture) = if let Some(run) = terminal.last_run.as_ref() {
            let capture = if terminal.is_running() {
                self.read_run_capture(run).await?
            } else {
                self.finalize_run_capture(&terminal, run).await?
            };
            (String::new(), capture)
        } else {
            let raw = self.capture_pane(&terminal, max_lines).await?;
            let capture = extract_run_capture(&raw, None);
            (raw, capture)
        };
        let running = !capture.completed && terminal.is_running();
        let mut output = if terminal.last_run.is_some() {
            take_last_lines(capture.output, max_lines)
        } else {
            raw
        };
        redact_text(&mut output, terminal.last_run.as_ref());

        Ok(ReadTerminalOutputResponse {
            terminal_id: terminal.id,
            session_id: terminal.session_id,
            session_name: terminal.session_name,
            window_id: terminal.window_id,
            window_name: terminal.window_name,
            pane_id: terminal.pane_id,
            output,
            exit_code: capture.exit_code,
            completed: capture.completed,
            running,
        })
    }

    pub async fn terminal_infos(&self) -> Result<Vec<ToolsServiceTerminalInfo>> {
        let terminals = self
            .terminals
            .read()
            .await
            .values()
            .cloned()
            .collect::<Vec<_>>();
        let mut infos = Vec::with_capacity(terminals.len());
        for terminal in terminals {
            if !self.pane_exists(&terminal.pane_id).await? {
                self.remove_missing_terminal(&terminal).await?;
                continue;
            }
            let terminal = self.refresh_completion(terminal).await?;
            infos.push(terminal_info(&terminal));
        }
        infos.sort_by(|left, right| left.id.cmp(&right.id));
        Ok(infos)
    }

    pub async fn terminal_info(
        &self,
        session_key: &str,
        terminal_id: &str,
    ) -> Result<ToolsServiceTerminalInfo> {
        let terminal = self.lookup_terminal(session_key, terminal_id).await?;
        if !self.pane_exists(&terminal.pane_id).await? {
            self.remove_missing_terminal(&terminal).await?;
            bail!("terminal id no longer exists: {terminal_id}");
        }
        let terminal = self.refresh_completion(terminal).await?;
        Ok(terminal_info(&terminal))
    }

    pub async fn create_interactive_terminal(
        &self,
        session_key: &str,
    ) -> Result<ToolsServiceTerminalInfo> {
        if session_key.trim().is_empty() {
            bail!("session_key cannot be empty");
        }
        let _allocation_permit = self
            .allocation_gate
            .acquire()
            .await
            .map_err(|_| anyhow!("terminal allocation gate closed"))?;
        let terminal = self
            .create_terminal(session_key, true, Some(&self.default_working_dir))
            .await?;
        Ok(terminal_info(&terminal))
    }

    pub async fn shutdown(&self) -> Result<()> {
        let terminals = self
            .terminals
            .write()
            .await
            .drain()
            .map(|(_, terminal)| terminal)
            .collect::<Vec<_>>();
        let mut cleanup_errors = Vec::new();
        for terminal in terminals {
            if let Some(run) = terminal.last_run.as_ref()
                && let Err(error) = self.remove_run_capture(&terminal, run).await
            {
                cleanup_errors.push(format!("terminal {}: {error:#}", terminal.id));
            }
        }
        let capture_dir = self
            .capture_dir
            .lock()
            .await
            .take()
            .ok_or_else(|| anyhow!("terminal manager is already shut down"))?;
        if let Err(error) = capture_dir.close() {
            cleanup_errors.push(format!("capture directory: {error}"));
        }
        if cleanup_errors.is_empty() {
            Ok(())
        } else {
            bail!(
                "failed to clean up terminal captures: {}",
                cleanup_errors.join("; ")
            )
        }
    }

    #[cfg(test)]
    async fn resolve_terminal(
        &self,
        session_key: &str,
        terminal_id: Option<&str>,
        force_new: bool,
        cwd: Option<&Path>,
    ) -> Result<ManagedTerminal> {
        let _allocation_permit = self
            .allocation_gate
            .acquire()
            .await
            .map_err(|_| anyhow!("terminal allocation gate closed"))?;
        self.resolve_terminal_inner(session_key, terminal_id, force_new, cwd)
            .await
    }

    async fn resolve_terminal_inner(
        &self,
        session_key: &str,
        terminal_id: Option<&str>,
        force_new: bool,
        cwd: Option<&Path>,
    ) -> Result<ManagedTerminal> {
        if let Some(terminal) = self
            .requested_terminal_for_execute(session_key, terminal_id, force_new)
            .await
        {
            return Ok(terminal);
        }
        if !force_new && let Some(terminal) = self.find_idle_terminal(session_key).await? {
            return Ok(terminal);
        }
        let has_busy_terminal = !force_new
            && self
                .terminals
                .read()
                .await
                .values()
                .any(|terminal| terminal.session_key == session_key && terminal.is_running());
        self.create_terminal(session_key, force_new || has_busy_terminal, cwd)
            .await
    }

    async fn requested_terminal_for_execute(
        &self,
        session_key: &str,
        terminal_id: Option<&str>,
        force_new: bool,
    ) -> Option<ManagedTerminal> {
        if force_new {
            return None;
        }
        let terminal_id = terminal_id.filter(|id| !id.trim().is_empty())?;
        let terminal = self.terminals.read().await.get(terminal_id).cloned()?;
        (terminal.session_key == session_key).then_some(terminal)
    }

    async fn lookup_terminal(
        &self,
        session_key: &str,
        terminal_id: &str,
    ) -> Result<ManagedTerminal> {
        let terminal = self
            .terminals
            .read()
            .await
            .get(terminal_id)
            .cloned()
            .ok_or_else(|| anyhow!("terminal id not found: {terminal_id}"))?;
        if terminal.session_key != session_key {
            bail!("terminal {terminal_id} belongs to another session");
        }
        Ok(terminal)
    }

    async fn find_idle_terminal(&self, session_key: &str) -> Result<Option<ManagedTerminal>> {
        let terminals = self
            .terminals
            .read()
            .await
            .values()
            .filter(|terminal| terminal.session_key == session_key && !terminal.is_running())
            .cloned()
            .collect::<Vec<_>>();
        for terminal in terminals {
            if self.pane_exists(&terminal.pane_id).await? {
                return Ok(Some(terminal));
            }
            self.remove_missing_terminal(&terminal).await?;
        }
        Ok(None)
    }

    async fn create_terminal(
        &self,
        session_key: &str,
        force_new: bool,
        cwd: Option<&Path>,
    ) -> Result<ManagedTerminal> {
        let cwd = cwd.unwrap_or(&self.default_working_dir);
        let session_name = default_session_name(session_key);
        let mut panes = self.list_panes().await?;
        let session_panes = panes
            .iter()
            .filter(|pane| pane.session_name == session_name)
            .cloned()
            .collect::<Vec<_>>();
        let pane = if session_panes.is_empty() {
            self.new_session(&session_name, cwd).await?;
            panes = self.list_panes().await?;
            choose_active_pane(
                &panes
                    .into_iter()
                    .filter(|pane| pane.session_name == session_name)
                    .collect::<Vec<_>>(),
            )
            .ok_or_else(|| anyhow!("new tmux session has no active pane"))?
        } else if force_new {
            self.new_window(&session_panes[0].session_id, cwd).await?
        } else {
            choose_active_pane(&session_panes)
                .ok_or_else(|| anyhow!("tmux session has no active pane"))?
        };

        let terminal = ManagedTerminal {
            id: self.next_terminal_id(),
            session_key: session_key.to_string(),
            session_id: pane.session_id,
            session_name: pane.session_name,
            window_id: pane.window_id,
            window_name: pane.window_name,
            pane_id: pane.pane_id,
            gate: Arc::new(Semaphore::new(1)),
            last_run: None,
        };
        self.store_terminal(terminal.clone()).await;
        Ok(terminal)
    }

    async fn require_working_dir(&self, cwd: &str) -> Result<PathBuf> {
        let path = PathBuf::from(cwd);
        let metadata = tokio::fs::metadata(&path)
            .await
            .with_context(|| format!("working directory is unavailable: {}", path.display()))?;
        if !metadata.is_dir() {
            bail!("working directory is not a directory: {}", path.display());
        }
        Ok(path)
    }

    async fn new_session(&self, session_name: &str, cwd: &Path) -> Result<()> {
        let output = self
            .runtime
            .run(&[
                "new-session".into(),
                "-d".into(),
                "-s".into(),
                session_name.into(),
                "-x".into(),
                DEFAULT_TMUX_COLS.to_string(),
                "-y".into(),
                DEFAULT_TMUX_ROWS.to_string(),
                "-c".into(),
                cwd.to_string_lossy().into_owned(),
                "bash -l".into(),
            ])
            .await?;
        if output.exit_code != 0
            && !output
                .stderr
                .to_ascii_lowercase()
                .contains("duplicate session")
        {
            bail!("failed to create tmux session: {}", command_error(&output));
        }
        Ok(())
    }

    async fn new_window(&self, session_id: &str, cwd: &Path) -> Result<TmuxPaneInfo> {
        let window_name = format!("term-{}", self.next_id.load(Ordering::Relaxed));
        let output = self
            .runtime
            .run(&[
                "new-window".into(),
                "-d".into(),
                "-P".into(),
                "-F".into(),
                tmux_format(),
                "-t".into(),
                session_id.into(),
                "-n".into(),
                window_name,
                "-c".into(),
                cwd.to_string_lossy().into_owned(),
                "bash -l".into(),
            ])
            .await?;
        if output.exit_code != 0 {
            bail!("failed to create tmux window: {}", command_error(&output));
        }
        parse_pane_line(output.stdout.trim())
            .ok_or_else(|| anyhow!("invalid tmux new-window output: {}", output.stdout.trim()))
    }

    async fn list_panes(&self) -> Result<Vec<TmuxPaneInfo>> {
        let output = self
            .runtime
            .run(&["list-panes".into(), "-aF".into(), tmux_format()])
            .await?;
        if output.exit_code != 0 {
            let message = command_error(&output);
            if is_no_server(&message) {
                return Ok(Vec::new());
            }
            bail!("failed to list tmux panes: {message}");
        }
        output
            .stdout
            .lines()
            .filter(|line| !line.trim().is_empty())
            .map(|line| {
                parse_pane_line(line).ok_or_else(|| anyhow!("invalid tmux pane line: {line}"))
            })
            .collect()
    }

    async fn pane_exists(&self, pane_id: &str) -> Result<bool> {
        Ok(self
            .list_panes()
            .await?
            .iter()
            .any(|pane| pane.pane_id == pane_id))
    }

    async fn capture_pane(&self, terminal: &ManagedTerminal, max_lines: usize) -> Result<String> {
        let output = self
            .runtime
            .run(&[
                "capture-pane".into(),
                "-t".into(),
                terminal.pane_id.clone(),
                "-p".into(),
                "-S".into(),
                format!("-{}", max_lines.clamp(1, MAX_CAPTURE_LINES)),
            ])
            .await?;
        if output.exit_code != 0 {
            bail!(
                "failed to capture terminal output: {}",
                command_error(&output)
            );
        }
        Ok(output.stdout)
    }

    async fn wait_for_terminal_ready(&self, terminal: &ManagedTerminal) -> Result<()> {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        loop {
            let output = self.capture_pane(terminal, DEFAULT_CAPTURE_LINES).await?;
            if terminal_prompt_ready(&output) {
                return Ok(());
            }
            if tokio::time::Instant::now() >= deadline {
                bail!(
                    "timed out waiting for terminal {} shell readiness",
                    terminal.id
                );
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    }

    async fn paste_text(&self, terminal: &ManagedTerminal, text: &str) -> Result<()> {
        let buffer = format!("chelix-{}", uuid::Uuid::new_v4().simple());
        let set = self
            .runtime
            .run(&[
                "set-buffer".into(),
                "-b".into(),
                buffer.clone(),
                "--".into(),
                text.into(),
            ])
            .await?;
        if set.exit_code != 0 {
            bail!("failed to set tmux paste buffer: {}", command_error(&set));
        }
        let paste = self
            .runtime
            .run(&[
                "paste-buffer".into(),
                "-d".into(),
                "-b".into(),
                buffer,
                "-t".into(),
                terminal.pane_id.clone(),
            ])
            .await?;
        if paste.exit_code != 0 {
            bail!("failed to paste command: {}", command_error(&paste));
        }
        Ok(())
    }

    async fn start_run_capture(&self, terminal: &ManagedTerminal) -> Result<Arc<RunCapture>> {
        let output_path = {
            let capture_dir = self.capture_dir.lock().await;
            let capture_dir = capture_dir
                .as_ref()
                .ok_or_else(|| anyhow!("terminal manager is shut down"))?;
            let capture_file = tempfile::Builder::new()
                .prefix("run-")
                .suffix(".output")
                .tempfile_in(capture_dir.path())
                .context("creating terminal run capture")?;
            let (_, output_path) = capture_file
                .keep()
                .map_err(|error| anyhow!("keeping terminal run capture: {}", error.error))?;
            output_path
        };
        let completion_path = output_path.with_extension("complete");
        let pending_completion_path = output_path.with_extension("complete.pending");
        let capture = Arc::new(RunCapture {
            output_path,
            completion_path,
            pending_completion_path,
            state: Mutex::new(RunCaptureState {
                pipe_open: true,
                completion_verified: false,
            }),
        });
        let pipe_command = pipe_capture_command(&capture);
        let output = self
            .runtime
            .run(&[
                "pipe-pane".into(),
                "-t".into(),
                terminal.pane_id.clone(),
                pipe_command,
            ])
            .await?;
        if output.exit_code == 0 {
            return Ok(capture);
        }
        let remove_result = remove_capture_artifacts(&capture).await;
        match remove_result {
            Ok(()) => bail!(
                "failed to start terminal output capture: {}",
                command_error(&output)
            ),
            Err(remove_error) => bail!(
                "failed to start terminal output capture: {}; capture cleanup also failed: {remove_error:#}",
                command_error(&output)
            ),
        }
    }

    async fn read_run_capture(&self, run: &ManagedRun) -> Result<CaptureResult> {
        let bytes = tokio::fs::read(&run.capture.output_path)
            .await
            .with_context(|| {
                format!(
                    "reading terminal run capture {}",
                    run.capture.output_path.display()
                )
            })?;
        let raw = plain_terminal_text(&bytes);
        Ok(extract_run_capture(&raw, Some(run)))
    }

    async fn finalize_run_capture(
        &self,
        terminal: &ManagedTerminal,
        run: &ManagedRun,
    ) -> Result<CaptureResult> {
        let mut state = run.capture.state.lock().await;
        if state.pipe_open {
            self.wait_for_run_marker(run).await?;
            self.stop_run_capture(terminal).await?;
            state.pipe_open = false;
        }
        if !state.completion_verified {
            self.verify_capture_completion(&run.capture).await?;
            state.completion_verified = true;
        }
        drop(state);
        self.read_run_capture(run).await
    }

    async fn wait_for_run_marker(&self, run: &ManagedRun) -> Result<()> {
        let deadline = tokio::time::Instant::now() + PIPE_FLUSH_TIMEOUT;
        loop {
            if self.read_run_capture(run).await?.completed {
                return Ok(());
            }
            if tokio::time::Instant::now() >= deadline {
                bail!(
                    "timed out waiting for terminal capture to receive completion marker for run {}",
                    run.run_id
                );
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    }

    async fn stop_run_capture(&self, terminal: &ManagedTerminal) -> Result<()> {
        let output = self
            .runtime
            .run(&["pipe-pane".into(), "-t".into(), terminal.pane_id.clone()])
            .await?;
        if output.exit_code != 0 {
            bail!(
                "failed to stop terminal output capture: {}",
                command_error(&output)
            );
        }
        Ok(())
    }

    async fn verify_capture_completion(&self, capture: &RunCapture) -> Result<()> {
        let deadline = tokio::time::Instant::now() + PIPE_FLUSH_TIMEOUT;
        loop {
            match tokio::fs::read_to_string(&capture.completion_path).await {
                Ok(status) => {
                    let status = status.trim();
                    if status == PIPE_FLUSHED {
                        return Ok(());
                    }
                    bail!("terminal output capture returned invalid completion marker: {status}");
                },
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {},
                Err(error) => {
                    return Err(error).with_context(|| {
                        format!(
                            "reading terminal capture completion {}",
                            capture.completion_path.display()
                        )
                    });
                },
            }
            if tokio::time::Instant::now() >= deadline {
                bail!("timed out waiting for terminal output capture to flush");
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    }

    async fn remove_run_capture(&self, terminal: &ManagedTerminal, run: &ManagedRun) -> Result<()> {
        self.remove_capture_files(terminal, &run.capture).await
    }

    async fn remove_capture_files(
        &self,
        terminal: &ManagedTerminal,
        capture: &RunCapture,
    ) -> Result<()> {
        let mut state = capture.state.lock().await;
        if state.pipe_open {
            self.stop_run_capture(terminal).await?;
            state.pipe_open = false;
        }
        if !state.completion_verified {
            self.verify_capture_completion(capture).await?;
            state.completion_verified = true;
        }
        remove_file_if_present(&capture.output_path).await?;
        remove_file_if_present(&capture.pending_completion_path).await?;
        remove_file_if_present(&capture.completion_path).await?;
        Ok(())
    }

    async fn remove_missing_terminal(&self, terminal: &ManagedTerminal) -> Result<()> {
        if let Some(run) = terminal.last_run.as_ref() {
            let mut state = run.capture.state.lock().await;
            if !state.completion_verified {
                self.verify_capture_completion(&run.capture).await?;
                state.completion_verified = true;
            }
            state.pipe_open = false;
            remove_file_if_present(&run.capture.output_path).await?;
            remove_file_if_present(&run.capture.pending_completion_path).await?;
            remove_file_if_present(&run.capture.completion_path).await?;
        }
        self.terminals.write().await.remove(&terminal.id);
        Ok(())
    }

    async fn refresh_completion(&self, mut terminal: ManagedTerminal) -> Result<ManagedTerminal> {
        let Some(run) = terminal.last_run.as_ref() else {
            return Ok(terminal);
        };
        if run.completed || !run.marker_enabled {
            return Ok(terminal);
        }
        let capture = self.read_run_capture(run).await?;
        if capture.completed {
            self.finalize_run_capture(&terminal, run).await?;
            self.mark_run_completed(&terminal.id, capture.exit_code)
                .await;
            if let Some(run) = terminal.last_run.as_mut() {
                run.completed = true;
                run.exit_code = capture.exit_code;
            }
        }
        Ok(terminal)
    }

    async fn store_terminal(&self, terminal: ManagedTerminal) {
        self.terminals
            .write()
            .await
            .insert(terminal.id.clone(), terminal);
    }

    async fn mark_run_completed(&self, terminal_id: &str, exit_code: Option<i32>) {
        if let Some(terminal) = self.terminals.write().await.get_mut(terminal_id)
            && let Some(run) = terminal.last_run.as_mut()
        {
            run.completed = true;
            run.exit_code = exit_code;
        }
    }

    fn next_terminal_id(&self) -> String {
        self.next_id.fetch_add(1, Ordering::Relaxed).to_string()
    }
}

fn pipe_capture_command(capture: &RunCapture) -> String {
    let output_path_raw = capture.output_path.to_string_lossy().into_owned();
    let pending_completion_path_raw = capture
        .pending_completion_path
        .to_string_lossy()
        .into_owned();
    let completion_path_raw = capture.completion_path.to_string_lossy().into_owned();
    let output_path = shell_words::quote(&output_path_raw);
    let pending_completion_path = shell_words::quote(&pending_completion_path_raw);
    let completion_path = shell_words::quote(&completion_path_raw);
    format!(
        "/bin/cat > {output_path} && printf '{PIPE_FLUSHED}\\n' > {pending_completion_path} && /bin/mv {pending_completion_path} {completion_path}"
    )
}

async fn remove_capture_artifacts(capture: &RunCapture) -> Result<()> {
    remove_file_if_present(&capture.output_path).await?;
    remove_file_if_present(&capture.pending_completion_path).await?;
    remove_file_if_present(&capture.completion_path).await
}

async fn remove_file_if_present(path: &Path) -> Result<()> {
    match tokio::fs::remove_file(path).await {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error).with_context(|| format!("removing {}", path.display())),
    }
}

#[derive(Default)]
struct PlainTerminalText {
    output: String,
}

impl vte::Perform for PlainTerminalText {
    fn print(&mut self, character: char) {
        self.output.push(character);
    }

    fn execute(&mut self, byte: u8) {
        if matches!(byte, b'\n' | b'\r' | b'\t') {
            self.output.push(char::from(byte));
        }
    }
}

fn plain_terminal_text(bytes: &[u8]) -> String {
    let mut parser = vte::Parser::new();
    let mut output = PlainTerminalText::default();
    parser.advance(&mut output, bytes);
    output.output
}

fn terminal_info(terminal: &ManagedTerminal) -> ToolsServiceTerminalInfo {
    ToolsServiceTerminalInfo {
        kind: ToolsServiceTerminalKind::Execute,
        id: terminal.id.clone(),
        session_key: terminal.session_key.clone(),
        session_id: terminal.session_id.clone(),
        session_name: terminal.session_name.clone(),
        window_id: terminal.window_id.clone(),
        window_name: terminal.window_name.clone(),
        pane_id: terminal.pane_id.clone(),
        running: terminal.is_running(),
    }
}

fn validate_execute_request(request: &ExecuteCommandRequest) -> Result<()> {
    if request.session_key.trim().is_empty() {
        bail!("session_key cannot be empty");
    }
    if request.command.trim().is_empty() {
        bail!("command cannot be empty");
    }
    if request.timeout_millis == 0 {
        bail!("timeout_millis must be greater than zero");
    }
    Ok(())
}

fn execute_response(
    terminal: ManagedTerminal,
    run_id: String,
    output: String,
    exit_code: Option<i32>,
    completed: bool,
    timed_out: bool,
    background: bool,
    message: String,
) -> ExecuteCommandResponse {
    ExecuteCommandResponse {
        terminal_id: terminal.id,
        run_id,
        session_id: terminal.session_id,
        session_name: terminal.session_name,
        window_id: terminal.window_id,
        window_name: terminal.window_name,
        pane_id: terminal.pane_id,
        output,
        exit_code,
        completed,
        timed_out,
        background,
        message,
    }
}

fn build_paste_payload(
    command: &str,
    cwd: Option<&Path>,
    run_id: &str,
    env: &[ToolsServiceEnvVar],
) -> Result<String> {
    let mut statements = Vec::new();
    if let Some(cwd) = cwd {
        statements.push(format!(
            "cd {}",
            shell_words::quote(&cwd.to_string_lossy()).as_ref()
        ));
    }
    let mut keys = Vec::with_capacity(env.len());
    for variable in env {
        if !is_shell_env_key(&variable.key) {
            continue;
        }
        keys.push(variable.key.as_str());
        statements.push(env_export_statement(&variable.key, &variable.value));
    }
    statements.push(format!("printf '\\n{START_PREFIX}{run_id}\\n'"));
    statements.push(format!("eval {}", shell_words::quote(command).as_ref()));
    statements.push("__chelix_exit=$?".into());
    if let Some(restore) = restore_env_statement(&keys) {
        statements.push(restore);
    }
    statements.push(format!(
        "printf '\\n{DONE_PREFIX}{run_id}:%s\\n' \"$__chelix_exit\""
    ));
    Ok(format!("{}\n", statements.join("; ")))
}

fn is_shell_env_key(key: &str) -> bool {
    let mut chars = key.chars();
    matches!(chars.next(), Some(ch) if ch.is_ascii_alphabetic() || ch == '_')
        && chars.all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
}

fn env_export_statement(key: &str, value: &str) -> String {
    let backup_set = shell_backup_set_var(key);
    let backup_value = shell_backup_value_var(key);
    format!(
        "{backup_set}=${{{key}+x}}; {backup_value}=${{{key}-}}; export {key}={}",
        shell_words::quote(value).as_ref()
    )
}

fn restore_env_statement(keys: &[&str]) -> Option<String> {
    (!keys.is_empty()).then(|| {
        keys.iter()
            .map(|key| {
                let backup_set = shell_backup_set_var(key);
                let backup_value = shell_backup_value_var(key);
                format!(
                    "if [ -n \"${{{backup_set}}}\" ]; then export {key}=\"${{{backup_value}}}\"; else unset {key}; fi; unset {backup_set} {backup_value}"
                )
            })
            .collect::<Vec<_>>()
            .join("; ")
    })
}

fn shell_backup_set_var(key: &str) -> String {
    format!("__chelix_env_{key}_was_set")
}

fn shell_backup_value_var(key: &str) -> String {
    format!("__chelix_env_{key}_value")
}

fn extract_run_capture(raw: &str, run: Option<&ManagedRun>) -> CaptureResult {
    let Some(run) = run else {
        return CaptureResult {
            output: raw.into(),
            completed: false,
            exit_code: None,
        };
    };
    let lines = raw.lines().collect::<Vec<_>>();
    let start_marker = format!("{START_PREFIX}{}", run.run_id);
    let done_marker = format!("{DONE_PREFIX}{}:", run.run_id);
    let start = lines
        .iter()
        .position(|line| line.trim() == start_marker)
        .map_or_else(
            || {
                if lines.len() < run.baseline_lines {
                    0
                } else {
                    run.baseline_lines
                }
            },
            |index| index + 1,
        )
        .min(lines.len());
    let mut output = Vec::new();
    let mut completed = run.completed;
    let mut exit_code = run.exit_code;
    for line in lines.iter().skip(start) {
        if let Some(code) = line.trim().strip_prefix(&done_marker) {
            exit_code = code.trim().parse().ok();
            completed = true;
            break;
        }
        if should_skip_wrapper_line(line, run) {
            continue;
        }
        output.push(*line);
    }
    CaptureResult {
        output: output.join("\n").trim().into(),
        completed,
        exit_code,
    }
}

fn take_last_lines(output: String, max_lines: usize) -> String {
    let lines = output.lines().collect::<Vec<_>>();
    lines[lines.len().saturating_sub(max_lines)..].join("\n")
}

fn should_skip_wrapper_line(line: &str, run: &ManagedRun) -> bool {
    let trimmed = line.trim();
    trimmed.is_empty()
        || trimmed == run.command.trim()
        || trimmed.ends_with(run.command.trim()) && prompt_prefix(trimmed, run.command.trim())
        || trimmed.contains(START_PREFIX)
        || trimmed.contains("__chelix_exit=$?")
        || trimmed.contains(DONE_PREFIX)
}

fn prompt_prefix(line: &str, command: &str) -> bool {
    let prefix = line.trim_end_matches(command).trim_end();
    prefix
        .chars()
        .last()
        .is_some_and(|last| matches!(last, '$' | '#' | '>' | '%'))
}

fn terminal_prompt_ready(output: &str) -> bool {
    output
        .lines()
        .rev()
        .find(|line| !line.trim().is_empty())
        .and_then(|line| line.trim_end().chars().last())
        .is_some_and(|last| matches!(last, '$' | '#' | '>' | '%'))
}

fn redact_text(output: &mut String, run: Option<&ManagedRun>) {
    let Some(run) = run else {
        return;
    };
    for value in &run.secret_values {
        for needle in redaction_needles(value) {
            *output = output.replace(&needle, "[REDACTED]");
        }
    }
}

fn redaction_needles(value: &str) -> Vec<String> {
    use base64::Engine as _;

    let mut needles = vec![value.to_string()];
    let standard = base64::engine::general_purpose::STANDARD.encode(value.as_bytes());
    let url = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(value.as_bytes());
    if standard != value {
        needles.push(standard);
    }
    if url != value {
        needles.push(url);
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

fn tmux_format() -> String {
    [
        "session_id",
        "session_name",
        "window_id",
        "window_index",
        "window_name",
        "window_active",
        "pane_id",
        "pane_index",
        "pane_active",
    ]
    .iter()
    .map(|field| format!("#{{{field}}}"))
    .collect::<Vec<_>>()
    .join(FIELD_SEPARATOR)
}

fn parse_pane_line(line: &str) -> Option<TmuxPaneInfo> {
    let parts = line.split(FIELD_SEPARATOR).collect::<Vec<_>>();
    if parts.len() != 9 {
        return None;
    }
    Some(TmuxPaneInfo {
        session_id: parts[0].into(),
        session_name: parts[1].into(),
        window_id: parts[2].into(),
        window_index: parts[3].parse().ok()?,
        window_name: parts[4].into(),
        window_active: parts[5] == "1",
        pane_id: parts[6].into(),
        pane_index: parts[7].parse().ok()?,
        pane_active: parts[8] == "1",
    })
}

fn choose_active_pane(panes: &[TmuxPaneInfo]) -> Option<TmuxPaneInfo> {
    panes
        .iter()
        .max_by_key(|pane| {
            (
                pane.window_active,
                pane.pane_active,
                std::cmp::Reverse(pane.window_index),
                std::cmp::Reverse(pane.pane_index),
            )
        })
        .cloned()
}

fn default_session_name(session_key: &str) -> String {
    let suffix = session_key
        .chars()
        .take(48)
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '-' | '_') {
                character
            } else {
                '-'
            }
        })
        .collect::<String>();
    if suffix.is_empty() {
        "chelix-main".into()
    } else {
        format!("chelix-{suffix}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn payload_quotes_cwd_command_and_environment() {
        let payload =
            build_paste_payload("printf '%s' ok", Some(Path::new("/tmp/it's")), "abc", &[
                ToolsServiceEnvVar {
                    key: "TOKEN".into(),
                    value: "value with spaces".into(),
                    secret: true,
                },
            ])
            .unwrap_or_else(|error| panic!("payload failed: {error}"));

        assert!(payload.contains("cd '/tmp/it'\\''s'"));
        assert!(payload.contains("export TOKEN='value with spaces'"));
        assert!(payload.contains("__CHELIX_COMMAND_DONE__abc"));
    }

    #[test]
    fn invalid_environment_name_is_ignored() {
        let payload = build_paste_payload("true", None, "abc", &[ToolsServiceEnvVar {
            key: "BAD-NAME".into(),
            value: "value".into(),
            secret: false,
        }])
        .unwrap_or_else(|error| panic!("payload failed: {error}"));

        assert!(!payload.contains("BAD-NAME"));
        assert!(!payload.contains("value"));
    }

    #[test]
    fn payload_restores_environment_with_original_backup_names() {
        let payload = build_paste_payload("echo 1", None, "abc", &[ToolsServiceEnvVar {
            key: "CHELIX_GATEWAY_URL".into(),
            value: "http://127.0.0.1:18789".into(),
            secret: false,
        }])
        .unwrap_or_else(|error| panic!("payload failed: {error}"));

        assert!(
            payload.contains("__chelix_env_CHELIX_GATEWAY_URL_was_set=${CHELIX_GATEWAY_URL+x}")
        );
        assert!(payload.contains("__chelix_env_CHELIX_GATEWAY_URL_value=${CHELIX_GATEWAY_URL-}"));
        assert!(!payload.contains("__chelix_env_CHELIX_GATEWAY_URL_set="));
    }

    #[test]
    fn capture_extracts_output_and_exit_code() {
        let run = ManagedRun {
            run_id: "abc".into(),
            command: "echo true".into(),
            baseline_lines: 0,
            marker_enabled: true,
            completed: false,
            exit_code: None,
            secret_values: Vec::new(),
            capture: test_run_capture(),
        };
        let capture = extract_run_capture(
            "old\nold2\n$ printf '\\n__CHELIX_COMMAND_START__abc\\n'\n__CHELIX_COMMAND_START__abc\n$ echo true\ntrue\n__chelix_exit=$?\nprintf '\\n__CHELIX_COMMAND_DONE__abc:%s\\n' \"$__chelix_exit\"\n__CHELIX_COMMAND_DONE__abc:0\n$ ",
            Some(&run),
        );

        assert!(capture.completed);
        assert_eq!(capture.exit_code, Some(0));
        assert_eq!(capture.output, "true");
    }

    #[test]
    fn capture_uses_tail_when_prior_scrollback_was_truncated() {
        let run = ManagedRun {
            run_id: "abc".into(),
            command: "yes".into(),
            baseline_lines: MAX_CAPTURE_LINES,
            marker_enabled: true,
            completed: false,
            exit_code: None,
            secret_values: Vec::new(),
            capture: test_run_capture(),
        };
        let capture = extract_run_capture("line one\nline two\nline three", Some(&run));

        assert_eq!(capture.output, "line one\nline two\nline three");
        assert!(!capture.completed);
    }

    #[test]
    fn secret_encodings_are_redacted() {
        use base64::Engine as _;

        let secret = "sensitive-token";
        let run = ManagedRun {
            run_id: "abc".into(),
            command: "printenv TOKEN".into(),
            baseline_lines: 0,
            marker_enabled: true,
            completed: true,
            exit_code: Some(0),
            secret_values: vec![secret.into()],
            capture: test_run_capture(),
        };
        let mut output = format!(
            "{secret} {} 73656e7369746976652d746f6b656e",
            base64::engine::general_purpose::STANDARD.encode(secret.as_bytes())
        );

        redact_text(&mut output, Some(&run));

        assert!(!output.contains(secret));
        assert!(output.contains("[REDACTED]"));
    }

    fn test_terminal(id: &str, session_key: &str) -> ManagedTerminal {
        ManagedTerminal {
            id: id.into(),
            session_key: session_key.into(),
            session_id: "$0".into(),
            session_name: "main".into(),
            window_id: "@1".into(),
            window_name: "bash".into(),
            pane_id: "%2".into(),
            gate: Arc::new(Semaphore::new(1)),
            last_run: None,
        }
    }

    fn test_run_capture() -> Arc<RunCapture> {
        Arc::new(RunCapture {
            output_path: PathBuf::from("capture.output"),
            completion_path: PathBuf::from("capture.complete"),
            pending_completion_path: PathBuf::from("capture.complete.pending"),
            state: Mutex::new(RunCaptureState {
                pipe_open: false,
                completion_verified: true,
            }),
        })
    }

    #[tokio::test]
    async fn pipe_capture_publishes_completion_only_after_output_flush() -> Result<()> {
        use tokio::io::AsyncWriteExt as _;

        let capture_dir = tempfile::tempdir().context("creating test capture directory")?;
        let capture = RunCapture {
            output_path: capture_dir.path().join("capture.output"),
            completion_path: capture_dir.path().join("capture.complete"),
            pending_completion_path: capture_dir.path().join("capture.complete.pending"),
            state: Mutex::new(RunCaptureState {
                pipe_open: true,
                completion_verified: false,
            }),
        };
        let mut child = tokio::process::Command::new("/bin/sh")
            .arg("-c")
            .arg(pipe_capture_command(&capture))
            .stdin(std::process::Stdio::piped())
            .spawn()
            .context("starting test capture pipe")?;
        let mut stdin = child
            .stdin
            .take()
            .ok_or_else(|| anyhow!("test capture pipe has no stdin"))?;
        stdin
            .write_all(b"captured output\n")
            .await
            .context("writing test capture output")?;

        let output_deadline = tokio::time::Instant::now() + Duration::from_secs(1);
        loop {
            if tokio::fs::read(&capture.output_path)
                .await
                .is_ok_and(|output| output == b"captured output\n")
            {
                break;
            }
            if tokio::time::Instant::now() >= output_deadline {
                bail!("test capture pipe did not flush streamed output");
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(
            !tokio::fs::try_exists(&capture.completion_path)
                .await
                .context("checking unpublished completion marker")?
        );

        drop(stdin);
        let status = child
            .wait()
            .await
            .context("waiting for test capture pipe")?;
        assert!(status.success());
        assert_eq!(
            tokio::fs::read_to_string(&capture.completion_path)
                .await
                .context("reading published completion marker")?,
            format!("{PIPE_FLUSHED}\n")
        );
        assert!(
            !tokio::fs::try_exists(&capture.pending_completion_path)
                .await
                .context("checking pending completion marker cleanup")?
        );

        Ok(())
    }

    #[test]
    fn terminal_ids_are_numeric_and_sequential() {
        let manager = TerminalManager::new(PathBuf::from("/tmp"), Arc::new(TmuxRuntime::new()))
            .unwrap_or_else(|error| panic!("manager creation failed: {error}"));

        assert_eq!(manager.next_terminal_id(), "1");
        assert_eq!(manager.next_terminal_id(), "2");
        assert_eq!(manager.next_terminal_id(), "3");
    }

    #[tokio::test]
    async fn requested_terminal_preserves_original_reuse_semantics() {
        let manager = TerminalManager::new(PathBuf::from("/tmp"), Arc::new(TmuxRuntime::new()))
            .unwrap_or_else(|error| panic!("manager creation failed: {error}"));
        manager.store_terminal(test_terminal("3", "main")).await;

        assert_eq!(
            manager
                .requested_terminal_for_execute("main", Some("3"), false)
                .await
                .map(|terminal| terminal.id),
            Some("3".into())
        );
        assert!(
            manager
                .requested_terminal_for_execute("main", Some("missing"), false)
                .await
                .is_none()
        );
        assert!(
            manager
                .requested_terminal_for_execute("other", Some("3"), false)
                .await
                .is_none()
        );
        assert!(
            manager
                .requested_terminal_for_execute("main", Some("3"), true)
                .await
                .is_none()
        );
    }

    #[tokio::test]
    async fn concurrent_new_terminals_use_distinct_tmux_windows_and_panes() {
        let runtime = Arc::new(TmuxRuntime::new());
        let working_dir = std::env::temp_dir();
        let manager = TerminalManager::new(working_dir.clone(), Arc::clone(&runtime))
            .unwrap_or_else(|error| panic!("manager creation failed: {error}"));
        let (first, second, third) = tokio::join!(
            manager.resolve_terminal("agent", None, true, Some(&working_dir)),
            manager.resolve_terminal("agent", None, true, Some(&working_dir)),
            manager.resolve_terminal("agent", None, true, Some(&working_dir)),
        );
        let first = first.unwrap_or_else(|error| panic!("first allocation failed: {error}"));
        let second = second.unwrap_or_else(|error| panic!("second allocation failed: {error}"));
        let third = third.unwrap_or_else(|error| panic!("third allocation failed: {error}"));

        assert_eq!([&first.id, &second.id, &third.id], ["1", "2", "3"]);
        assert_eq!(first.session_id, second.session_id);
        assert_eq!(second.session_id, third.session_id);
        assert_ne!(first.window_id, second.window_id);
        assert_ne!(first.window_id, third.window_id);
        assert_ne!(second.window_id, third.window_id);
        assert_ne!(first.pane_id, second.pane_id);
        assert_ne!(first.pane_id, third.pane_id);
        assert_ne!(second.pane_id, third.pane_id);

        runtime
            .shutdown()
            .await
            .unwrap_or_else(|error| panic!("tmux shutdown failed: {error}"));
    }

    #[tokio::test]
    async fn different_agents_share_service_without_sharing_tmux_panes() {
        let runtime = Arc::new(TmuxRuntime::new());
        let working_dir = std::env::temp_dir();
        let manager = TerminalManager::new(working_dir.clone(), Arc::clone(&runtime))
            .unwrap_or_else(|error| panic!("manager creation failed: {error}"));
        let (first, second) = tokio::join!(
            manager.resolve_terminal("agent-one", None, false, Some(&working_dir)),
            manager.resolve_terminal("agent-two", None, false, Some(&working_dir)),
        );
        let first = first.unwrap_or_else(|error| panic!("first allocation failed: {error}"));
        let second = second.unwrap_or_else(|error| panic!("second allocation failed: {error}"));

        assert_ne!(first.session_id, second.session_id);
        assert_ne!(first.session_name, second.session_name);
        assert_ne!(first.window_id, second.window_id);
        assert_ne!(first.pane_id, second.pane_id);

        runtime
            .shutdown()
            .await
            .unwrap_or_else(|error| panic!("tmux shutdown failed: {error}"));
    }

    #[tokio::test]
    async fn execute_echo_with_injected_environment_returns_only_command_output() {
        let runtime = Arc::new(TmuxRuntime::new());
        let manager = TerminalManager::new(std::env::temp_dir(), Arc::clone(&runtime))
            .unwrap_or_else(|error| panic!("manager creation failed: {error}"));
        let result = manager
            .execute_command(ExecuteCommandRequest {
                session_key: format!("session-{}", uuid::Uuid::new_v4()),
                command: "echo \"1\"".into(),
                custom_cwd: None,
                new_terminal: false,
                background: false,
                timeout_millis: 5_000,
                terminal_id: None,
                env: vec![
                    ToolsServiceEnvVar {
                        key: "CHELIX_GATEWAY_URL".into(),
                        value: "http://127.0.0.1:18789".into(),
                        secret: false,
                    },
                    ToolsServiceEnvVar {
                        key: "CHELIX_API_KEY".into(),
                        value: "test-key".into(),
                        secret: true,
                    },
                ],
            })
            .await;
        let shutdown = runtime.shutdown().await;
        let response = result.unwrap_or_else(|error| panic!("execute failed: {error}"));
        shutdown.unwrap_or_else(|error| panic!("tmux shutdown failed: {error}"));

        assert_eq!(response.terminal_id, "1");
        assert_eq!(response.output, "1");
        assert_eq!(response.exit_code, Some(0));
        assert!(response.completed);
        assert!(!response.output.contains("__chelix_env_"));
        assert!(!response.output.contains(START_PREFIX));
        assert!(!response.output.contains(DONE_PREFIX));
    }

    fn live_environment() -> Vec<ToolsServiceEnvVar> {
        vec![
            ToolsServiceEnvVar {
                key: "CHELIX_GATEWAY_URL".into(),
                value: "http://host.docker.internal:13131".into(),
                secret: false,
            },
            ToolsServiceEnvVar {
                key: "CHELIX_API_KEY".into(),
                value: "test-api-key".into(),
                secret: true,
            },
        ]
    }

    fn execute_request(
        session_key: &str,
        command: &str,
        new_terminal: bool,
        background: bool,
    ) -> ExecuteCommandRequest {
        ExecuteCommandRequest {
            session_key: session_key.into(),
            command: command.into(),
            custom_cwd: None,
            new_terminal,
            background,
            timeout_millis: 10_000,
            terminal_id: None,
            env: live_environment(),
        }
    }

    #[tokio::test]
    async fn concurrent_live_shape_commands_reserve_distinct_terminals_and_stream_output() {
        let runtime = Arc::new(TmuxRuntime::new());
        let manager = TerminalManager::new(std::env::temp_dir(), Arc::clone(&runtime))
            .unwrap_or_else(|error| panic!("manager creation failed: {error}"));
        let session_key = format!("session-{}", uuid::Uuid::new_v4());
        let (first, second, background) = tokio::join!(
            manager.execute_command(execute_request(&session_key, "echo \"1\"", false, false)),
            manager.execute_command(execute_request(&session_key, "echo \"2\"", true, false)),
            manager.execute_command(execute_request(
                &session_key,
                "printf 'background-line\\n'; sleep 2",
                true,
                true,
            )),
        );
        let first = first.unwrap_or_else(|error| panic!("first command failed: {error}"));
        let second = second.unwrap_or_else(|error| panic!("second command failed: {error}"));
        let background =
            background.unwrap_or_else(|error| panic!("background command failed: {error}"));

        assert_eq!(first.output, "1");
        assert_eq!(first.exit_code, Some(0));
        assert_eq!(second.output, "2");
        assert_eq!(second.exit_code, Some(0));
        assert_eq!(
            [
                &first.terminal_id,
                &second.terminal_id,
                &background.terminal_id
            ],
            ["1", "2", "3"]
        );
        assert_ne!(first.window_id, second.window_id);
        assert_ne!(first.window_id, background.window_id);
        assert_ne!(second.window_id, background.window_id);
        assert_ne!(first.pane_id, second.pane_id);
        assert_ne!(first.pane_id, background.pane_id);
        assert_ne!(second.pane_id, background.pane_id);
        assert!(background.background);
        assert!(!background.completed);

        tokio::time::sleep(Duration::from_millis(100)).await;
        let output = manager
            .read_terminal_output(ReadTerminalOutputRequest {
                session_key,
                terminal_id: background.terminal_id,
                max_lines: Some(30),
            })
            .await
            .unwrap_or_else(|error| panic!("background output failed: {error}"));
        assert_eq!(output.output, "background-line");
        assert!(output.running);

        runtime
            .shutdown()
            .await
            .unwrap_or_else(|error| panic!("tmux shutdown failed: {error}"));
    }
}
